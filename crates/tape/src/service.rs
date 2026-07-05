#![allow(clippy::result_large_err)]

use std::sync::{Arc, Mutex, MutexGuard};

use coldstore_common::config::TapeConfig;
use coldstore_proto::common::{self, DriveStatus, TapeStatus};
use coldstore_proto::tape::read_bundle_request::Location;
use coldstore_proto::tape::read_bundle_response::Payload as ReadPayload;
use coldstore_proto::tape::tape_service_server::TapeService;
use coldstore_proto::tape::write_bundle_request::Payload as WritePayload;
use coldstore_proto::tape::*;
use coldstore_vtl::model::{ElementAddress, TapeBarcode};
use coldstore_vtl::simulator::VirtualTapeLibrary;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};
use tonic::{Request, Response, Status, Streaming};

const SIMULATED_TAPE_CAPACITY_BYTES: u64 = 12 * 1024 * 1024 * 1024 * 1024;

type ServiceResult<T> = std::result::Result<T, Status>;

pub trait TapeBackend: Send + Sync {
    fn list_drives(&self) -> ServiceResult<Vec<common::DriveEndpoint>>;
    fn get_drive_status(&self, drive_id: &str) -> ServiceResult<common::DriveEndpoint>;
    fn acquire_drive(
        &self,
        preferred_drive_id: Option<&str>,
        required_tape_id: Option<&str>,
    ) -> ServiceResult<AcquireDriveResponse>;
    fn release_drive(&self, drive_id: &str) -> ServiceResult<()>;
    fn load_tape(&self, tape_id: &str, drive_id: &str, slot_id: Option<&str>) -> ServiceResult<()>;
    fn unload_tape(&self, drive_id: &str, target_slot_id: Option<&str>) -> ServiceResult<()>;
    fn rewind(&self, drive_id: &str) -> ServiceResult<()>;
    fn seek_to_filemark(&self, drive_id: &str, filemark: u32) -> ServiceResult<()>;
    fn get_tape_media_status(&self, drive_id: &str) -> ServiceResult<TapeMediaStatus>;
    fn inventory(&self) -> ServiceResult<InventoryResponse>;
    fn write_bundle(&self, drive_id: &str, data: &[u8]) -> ServiceResult<(u32, u32)>;
    fn read_bundle(&self, drive_id: &str, filemark: u32, length: u64) -> ServiceResult<Vec<u8>>;
}

pub struct TapeServiceImpl {
    _config: TapeConfig,
    backend: Arc<dyn TapeBackend>,
}

impl TapeServiceImpl {
    pub fn new(config: &TapeConfig) -> anyhow::Result<Self> {
        let backend = SimulatorTapeBackend::from_config(config)?;
        Ok(Self::new_with_backend(config.clone(), backend))
    }

    pub fn new_with_backend<B>(config: TapeConfig, backend: B) -> Self
    where
        B: TapeBackend + 'static,
    {
        Self {
            _config: config,
            backend: Arc::new(backend),
        }
    }

    pub async fn write_bundle_from_messages<S>(
        &self,
        mut messages: S,
    ) -> ServiceResult<WriteBundleResponse>
    where
        S: Stream<Item = ServiceResult<WriteBundleRequest>> + Unpin,
    {
        let first = match messages.next().await {
            Some(Ok(request)) => request,
            Some(Err(status)) => return Err(status),
            None => return Err(Status::invalid_argument("write_bundle stream is empty")),
        };
        let meta = match first.payload {
            Some(WritePayload::Meta(meta)) => meta,
            Some(WritePayload::Data(_)) => {
                return Err(Status::invalid_argument(
                    "first write_bundle message must carry metadata",
                ))
            }
            None => {
                return Err(Status::invalid_argument(
                    "write_bundle message has no payload",
                ))
            }
        };

        let mut data = Vec::new();
        while let Some(message) = messages.next().await {
            match message?.payload {
                Some(WritePayload::Data(chunk)) => data.extend_from_slice(&chunk),
                Some(WritePayload::Meta(_)) => {
                    return Err(Status::invalid_argument(
                        "write_bundle metadata must appear only once as the first message",
                    ))
                }
                None => {
                    return Err(Status::invalid_argument(
                        "write_bundle message has no payload",
                    ))
                }
            }
        }

        if meta.total_size != data.len() as u64 {
            return Err(Status::invalid_argument(format!(
                "write_bundle total_size={} does not match received bytes={}",
                meta.total_size,
                data.len()
            )));
        }

        let (filemark_start, filemark_end) = self.backend.write_bundle(&meta.drive_id, &data)?;
        Ok(WriteBundleResponse {
            drive_id: meta.drive_id,
            bundle_id: meta.bundle_id,
            bytes_written: data.len() as u64,
            filemark_start,
            filemark_end,
            checksum: None,
            success: true,
            error: None,
        })
    }
}

#[derive(Debug)]
pub struct SimulatorTapeBackend {
    state: Mutex<SimulatorState>,
}

impl SimulatorTapeBackend {
    pub fn new(slot_count: u32, drive_count: u32) -> Self {
        Self {
            state: Mutex::new(SimulatorState::new(slot_count, drive_count)),
        }
    }

    pub fn from_config(config: &TapeConfig) -> anyhow::Result<Self> {
        let drive_count = config.scsi.devices.len().max(1) as u32;
        let backend = Self::new(config.simulator.slot_count.max(1), drive_count);
        for (index, tape_id) in config.simulator.tape_ids.iter().enumerate() {
            let slot_id = format!("slot-{}", index + 1);
            backend
                .insert_tape(&slot_id, tape_id)
                .map_err(|status| anyhow::anyhow!(status.message().to_string()))?;
        }
        if let Some(tape_id) = &config.simulator.autoload_tape_id {
            backend
                .load_tape(tape_id, "drive-0", None)
                .map_err(|status| anyhow::anyhow!(status.message().to_string()))?;
        }
        Ok(backend)
    }

    pub fn insert_tape(&self, slot_id: &str, tape_id: &str) -> ServiceResult<()> {
        let slot = parse_slot_id(slot_id)?;
        let mut state = self.lock_state()?;
        state
            .library
            .insert_tape(slot, TapeBarcode::new(tape_id))
            .map_err(vtl_status)
    }

    fn lock_state(&self) -> ServiceResult<MutexGuard<'_, SimulatorState>> {
        self.state
            .lock()
            .map_err(|_| Status::internal("simulator tape backend mutex poisoned"))
    }
}

#[derive(Debug)]
struct SimulatorState {
    library: VirtualTapeLibrary,
    acquired: Vec<bool>,
    next_filemark: Vec<u32>,
}

impl SimulatorState {
    fn new(slot_count: u32, drive_count: u32) -> Self {
        Self {
            library: VirtualTapeLibrary::new(slot_count, drive_count),
            acquired: vec![false; drive_count as usize],
            next_filemark: vec![0; drive_count as usize],
        }
    }

    fn drive_endpoint(&self, drive_index: u32) -> ServiceResult<common::DriveEndpoint> {
        let drive = self
            .library
            .drive(ElementAddress::drive(drive_index))
            .map_err(vtl_status)?;
        let acquired = self
            .acquired
            .get(drive_index as usize)
            .copied()
            .unwrap_or(false);
        Ok(common::DriveEndpoint {
            drive_id: drive_id_string(drive_index),
            device_path: format!("vtl://drive/{drive_index}"),
            drive_type: "virtual-lto".to_string(),
            status: if acquired {
                DriveStatus::DriveInUse as i32
            } else {
                DriveStatus::DriveIdle as i32
            },
            current_tape: drive
                .loaded_barcode()
                .map(|barcode| barcode.as_str().to_string()),
        })
    }

    fn drive_index(&self, drive_id: &str) -> ServiceResult<u32> {
        let address = parse_drive_id(drive_id)?;
        self.library.drive(address).map_err(vtl_status)?;
        Ok(address.index())
    }

    fn find_slot_with_tape(&self, tape_id: &str) -> ServiceResult<ElementAddress> {
        self.library
            .slots()
            .iter()
            .find(|slot| slot.barcode().map(|barcode| barcode.as_str()) == Some(tape_id))
            .map(|slot| slot.address())
            .ok_or_else(|| {
                Status::not_found(format!("tape {tape_id} not found in simulator slots"))
            })
    }

    fn find_empty_slot(&self) -> ServiceResult<ElementAddress> {
        self.library
            .slots()
            .iter()
            .find(|slot| slot.is_empty())
            .map(|slot| slot.address())
            .ok_or_else(|| Status::failed_precondition("no empty simulator slot is available"))
    }
}

impl TapeBackend for SimulatorTapeBackend {
    fn list_drives(&self) -> ServiceResult<Vec<common::DriveEndpoint>> {
        let state = self.lock_state()?;
        (0..state.library.drives().len() as u32)
            .map(|drive_index| state.drive_endpoint(drive_index))
            .collect()
    }

    fn get_drive_status(&self, drive_id: &str) -> ServiceResult<common::DriveEndpoint> {
        let state = self.lock_state()?;
        let drive_index = state.drive_index(drive_id)?;
        state.drive_endpoint(drive_index)
    }

    fn acquire_drive(
        &self,
        preferred_drive_id: Option<&str>,
        required_tape_id: Option<&str>,
    ) -> ServiceResult<AcquireDriveResponse> {
        let mut state = self.lock_state()?;
        let drive_index = if let Some(preferred) = preferred_drive_id {
            state.drive_index(preferred)?
        } else {
            state
                .library
                .drives()
                .iter()
                .find(|drive| {
                    !state.acquired[drive.address().index() as usize]
                        && required_tape_id.is_none_or(|tape_id| {
                            drive.loaded_barcode().map(|barcode| barcode.as_str()) == Some(tape_id)
                        })
                })
                .map(|drive| drive.address().index())
                .ok_or_else(|| Status::resource_exhausted("no matching simulator drive is free"))?
        };

        let index = drive_index as usize;
        if state.acquired[index] {
            return Err(Status::failed_precondition(format!(
                "drive {} is already acquired",
                drive_id_string(drive_index)
            )));
        }
        if let Some(tape_id) = required_tape_id {
            let drive = state
                .library
                .drive(ElementAddress::drive(drive_index))
                .map_err(vtl_status)?;
            if drive.loaded_barcode().map(|barcode| barcode.as_str()) != Some(tape_id) {
                return Err(Status::failed_precondition(format!(
                    "drive {} does not contain required tape {tape_id}",
                    drive_id_string(drive_index)
                )));
            }
        }

        state.acquired[index] = true;
        let drive = state
            .library
            .drive(ElementAddress::drive(drive_index))
            .map_err(vtl_status)?;
        Ok(AcquireDriveResponse {
            drive_id: drive_id_string(drive_index),
            current_tape: drive
                .loaded_barcode()
                .map(|barcode| barcode.as_str().to_string()),
        })
    }

    fn release_drive(&self, drive_id: &str) -> ServiceResult<()> {
        let mut state = self.lock_state()?;
        let drive_index = state.drive_index(drive_id)?;
        state.acquired[drive_index as usize] = false;
        Ok(())
    }

    fn load_tape(&self, tape_id: &str, drive_id: &str, slot_id: Option<&str>) -> ServiceResult<()> {
        let drive = parse_drive_id(drive_id)?;
        let mut state = self.lock_state()?;
        state.library.drive(drive).map_err(vtl_status)?;
        let slot = if let Some(slot_id) = slot_id {
            parse_slot_id(slot_id)?
        } else {
            state.find_slot_with_tape(tape_id)?
        };
        let slot_ref = state.library.slot(slot).map_err(vtl_status)?;
        if slot_ref.barcode().map(|barcode| barcode.as_str()) != Some(tape_id) {
            return Err(Status::failed_precondition(format!(
                "slot {slot} does not contain tape {tape_id}"
            )));
        }
        state.library.load(slot, drive).map_err(vtl_status)
    }

    fn unload_tape(&self, drive_id: &str, target_slot_id: Option<&str>) -> ServiceResult<()> {
        let drive = parse_drive_id(drive_id)?;
        let mut state = self.lock_state()?;
        state.library.drive(drive).map_err(vtl_status)?;
        let slot = if let Some(slot_id) = target_slot_id {
            parse_slot_id(slot_id)?
        } else {
            state.find_empty_slot()?
        };
        state.library.unload(drive, slot).map_err(vtl_status)
    }

    fn rewind(&self, drive_id: &str) -> ServiceResult<()> {
        let drive = parse_drive_id(drive_id)?;
        let mut state = self.lock_state()?;
        state.library.rewind(drive).map_err(vtl_status)
    }

    fn seek_to_filemark(&self, drive_id: &str, filemark: u32) -> ServiceResult<()> {
        let drive = parse_drive_id(drive_id)?;
        let mut state = self.lock_state()?;
        state.library.rewind(drive).map_err(vtl_status)?;
        if filemark > 0 {
            state
                .library
                .seek_filemark(drive, filemark)
                .map_err(vtl_status)?;
        }
        Ok(())
    }

    fn get_tape_media_status(&self, drive_id: &str) -> ServiceResult<TapeMediaStatus> {
        let state = self.lock_state()?;
        let drive_index = state.drive_index(drive_id)?;
        let drive = state
            .library
            .drive(ElementAddress::drive(drive_index))
            .map_err(vtl_status)?;
        let Some(tape) = drive.loaded_tape() else {
            return Ok(TapeMediaStatus {
                drive_id: drive_id_string(drive_index),
                tape_id: None,
                tape_status: TapeStatus::TapeOffline as i32,
                capacity_bytes: SIMULATED_TAPE_CAPACITY_BYTES,
                used_bytes: 0,
                remaining_bytes: SIMULATED_TAPE_CAPACITY_BYTES,
                current_position: 0,
                current_filemark: 0,
                is_write_protected: false,
            });
        };

        let used_bytes = tape.used_bytes();
        Ok(TapeMediaStatus {
            drive_id: drive_id_string(drive_index),
            tape_id: Some(tape.barcode().as_str().to_string()),
            tape_status: TapeStatus::TapeOnline as i32,
            capacity_bytes: SIMULATED_TAPE_CAPACITY_BYTES,
            used_bytes,
            remaining_bytes: SIMULATED_TAPE_CAPACITY_BYTES.saturating_sub(used_bytes),
            current_position: tape.current_position(),
            current_filemark: tape.current_filemark(),
            is_write_protected: false,
        })
    }

    fn inventory(&self) -> ServiceResult<InventoryResponse> {
        let state = self.lock_state()?;
        let mut slots = Vec::new();
        for slot in state.library.slots() {
            slots.push(SlotInfo {
                slot_id: slot_id(slot.address().index()),
                tape_id: slot.barcode().map(|barcode| barcode.as_str().to_string()),
                is_drive: false,
                drive_id: None,
                is_import_export: false,
            });
        }
        for drive in state.library.drives() {
            slots.push(SlotInfo {
                slot_id: drive_id_string(drive.address().index()),
                tape_id: drive
                    .loaded_barcode()
                    .map(|barcode| barcode.as_str().to_string()),
                is_drive: true,
                drive_id: Some(drive_id_string(drive.address().index())),
                is_import_export: false,
            });
        }
        Ok(InventoryResponse { slots })
    }

    fn write_bundle(&self, drive_id: &str, data: &[u8]) -> ServiceResult<(u32, u32)> {
        let drive = parse_drive_id(drive_id)?;
        let mut state = self.lock_state()?;
        state.library.drive(drive).map_err(vtl_status)?;
        let drive_index = drive.index() as usize;
        let filemark_start = state.next_filemark[drive_index];
        state.library.write(drive, data).map_err(vtl_status)?;
        state.library.write_filemark(drive).map_err(vtl_status)?;
        state.next_filemark[drive_index] += 1;
        Ok((filemark_start, state.next_filemark[drive_index]))
    }

    fn read_bundle(&self, drive_id: &str, filemark: u32, length: u64) -> ServiceResult<Vec<u8>> {
        let drive = parse_drive_id(drive_id)?;
        let mut state = self.lock_state()?;
        state.library.rewind(drive).map_err(vtl_status)?;
        if filemark > 0 {
            state
                .library
                .seek_filemark(drive, filemark)
                .map_err(vtl_status)?;
        }
        let max_len = if length == 0 {
            usize::MAX
        } else {
            length.min(usize::MAX as u64) as usize
        };
        state.library.read(drive, max_len).map_err(vtl_status)
    }
}

#[tonic::async_trait]
impl TapeService for TapeServiceImpl {
    async fn write_bundle(
        &self,
        req: Request<Streaming<WriteBundleRequest>>,
    ) -> ServiceResult<Response<WriteBundleResponse>> {
        self.write_bundle_from_messages(req.into_inner())
            .await
            .map(Response::new)
    }

    type ReadBundleStream = ReceiverStream<ServiceResult<ReadBundleResponse>>;

    async fn read_bundle(
        &self,
        req: Request<ReadBundleRequest>,
    ) -> ServiceResult<Response<Self::ReadBundleStream>> {
        let req = req.into_inner();
        let filemark = match req.location {
            Some(Location::Filemark(filemark)) => filemark,
            Some(Location::BlockOffset(_)) => {
                return Err(Status::unimplemented(
                    "phase-1 simulator supports filemark-based reads; block offset reads require live SCSI/tape integration",
                ))
            }
            None => return Err(Status::invalid_argument("read_bundle location is required")),
        };
        let data = self
            .backend
            .read_bundle(&req.drive_id, filemark, req.length)?;
        let (tx, rx) = mpsc::channel(2);
        tx.send(Ok(ReadBundleResponse {
            payload: Some(ReadPayload::Meta(ReadBundleMeta {
                total_size: data.len() as u64,
                checksum: None,
            })),
        }))
        .await
        .map_err(|_| Status::cancelled("read_bundle receiver dropped before metadata send"))?;
        if !data.is_empty() {
            tx.send(Ok(ReadBundleResponse {
                payload: Some(ReadPayload::Data(data)),
            }))
            .await
            .map_err(|_| Status::cancelled("read_bundle receiver dropped before data send"))?;
        }
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn list_drives(&self, _req: Request<()>) -> ServiceResult<Response<ListDrivesResponse>> {
        Ok(Response::new(ListDrivesResponse {
            drives: self.backend.list_drives()?,
        }))
    }

    async fn get_drive_status(
        &self,
        req: Request<GetDriveStatusRequest>,
    ) -> ServiceResult<Response<common::DriveEndpoint>> {
        Ok(Response::new(
            self.backend.get_drive_status(&req.into_inner().drive_id)?,
        ))
    }

    async fn acquire_drive(
        &self,
        req: Request<AcquireDriveRequest>,
    ) -> ServiceResult<Response<AcquireDriveResponse>> {
        let req = req.into_inner();
        Ok(Response::new(self.backend.acquire_drive(
            req.preferred_drive_id.as_deref(),
            req.required_tape_id.as_deref(),
        )?))
    }

    async fn release_drive(
        &self,
        req: Request<ReleaseDriveRequest>,
    ) -> ServiceResult<Response<()>> {
        self.backend.release_drive(&req.into_inner().drive_id)?;
        Ok(Response::new(()))
    }

    async fn load_tape(&self, req: Request<LoadTapeRequest>) -> ServiceResult<Response<()>> {
        let req = req.into_inner();
        self.backend
            .load_tape(&req.tape_id, &req.drive_id, req.slot_id.as_deref())?;
        Ok(Response::new(()))
    }

    async fn unload_tape(&self, req: Request<UnloadTapeRequest>) -> ServiceResult<Response<()>> {
        let req = req.into_inner();
        self.backend
            .unload_tape(&req.drive_id, req.target_slot_id.as_deref())?;
        Ok(Response::new(()))
    }

    async fn rewind(&self, req: Request<RewindRequest>) -> ServiceResult<Response<()>> {
        self.backend.rewind(&req.into_inner().drive_id)?;
        Ok(Response::new(()))
    }

    async fn seek_to_filemark(
        &self,
        req: Request<SeekToFilemarkRequest>,
    ) -> ServiceResult<Response<()>> {
        let req = req.into_inner();
        self.backend.seek_to_filemark(&req.drive_id, req.filemark)?;
        Ok(Response::new(()))
    }

    async fn get_tape_media_status(
        &self,
        req: Request<GetTapeMediaStatusRequest>,
    ) -> ServiceResult<Response<TapeMediaStatus>> {
        Ok(Response::new(
            self.backend
                .get_tape_media_status(&req.into_inner().drive_id)?,
        ))
    }

    async fn inventory(&self, _req: Request<()>) -> ServiceResult<Response<InventoryResponse>> {
        Ok(Response::new(self.backend.inventory()?))
    }
}

fn parse_slot_id(slot_id: &str) -> ServiceResult<ElementAddress> {
    let raw = slot_id.strip_prefix("slot-").unwrap_or(slot_id);
    let index = raw
        .parse::<u32>()
        .map_err(|_| Status::invalid_argument(format!("invalid slot id: {slot_id}")))?;
    if index == 0 {
        return Err(Status::invalid_argument("slot ids are 1-based"));
    }
    Ok(ElementAddress::slot(index))
}

fn parse_drive_id(drive_id: &str) -> ServiceResult<ElementAddress> {
    let raw = drive_id.strip_prefix("drive-").unwrap_or(drive_id);
    let index = raw
        .parse::<u32>()
        .map_err(|_| Status::invalid_argument(format!("invalid drive id: {drive_id}")))?;
    Ok(ElementAddress::drive(index))
}

fn slot_id(index: u32) -> String {
    format!("slot-{index}")
}

fn drive_id_string(index: u32) -> String {
    format!("drive-{index}")
}

fn vtl_status(error: coldstore_vtl::VtlError) -> Status {
    match error {
        coldstore_vtl::VtlError::SlotEmpty(_)
        | coldstore_vtl::VtlError::SlotOccupied(_)
        | coldstore_vtl::VtlError::DriveEmpty(_)
        | coldstore_vtl::VtlError::DriveOccupied(_)
        | coldstore_vtl::VtlError::FilemarkNotFound => {
            Status::failed_precondition(error.to_string())
        }
        coldstore_vtl::VtlError::SlotOutOfRange(_)
        | coldstore_vtl::VtlError::DriveOutOfRange(_)
        | coldstore_vtl::VtlError::WrongElement { .. }
        | coldstore_vtl::VtlError::InvalidLsscsiLine(_)
        | coldstore_vtl::VtlError::InvalidElementAddress(_) => {
            Status::invalid_argument(error.to_string())
        }
        coldstore_vtl::VtlError::CommandFailed { .. } => Status::unavailable(error.to_string()),
        coldstore_vtl::VtlError::Io(_) => Status::unavailable(error.to_string()),
    }
}
