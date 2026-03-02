use coldstore_common::config::TapeConfig;
use coldstore_proto::common;
use coldstore_proto::tape::tape_service_server::TapeService;
use coldstore_proto::tape::*;
use tonic::{Request, Response, Status, Streaming};

pub struct TapeServiceImpl {
    _config: TapeConfig,
}

impl TapeServiceImpl {
    pub fn new(config: &TapeConfig) -> anyhow::Result<Self> {
        Ok(Self {
            _config: config.clone(),
        })
    }
}

#[tonic::async_trait]
impl TapeService for TapeServiceImpl {
    async fn write_bundle(
        &self,
        _req: Request<Streaming<WriteBundleRequest>>,
    ) -> std::result::Result<Response<WriteBundleResponse>, Status> {
        todo!()
    }

    type ReadBundleStream =
        tokio_stream::wrappers::ReceiverStream<Result<ReadBundleResponse, Status>>;
    async fn read_bundle(
        &self,
        _req: Request<ReadBundleRequest>,
    ) -> std::result::Result<Response<Self::ReadBundleStream>, Status> {
        todo!()
    }

    async fn list_drives(
        &self,
        _req: Request<()>,
    ) -> std::result::Result<Response<ListDrivesResponse>, Status> {
        todo!()
    }
    async fn get_drive_status(
        &self,
        _req: Request<GetDriveStatusRequest>,
    ) -> std::result::Result<Response<common::DriveEndpoint>, Status> {
        todo!()
    }
    async fn acquire_drive(
        &self,
        _req: Request<AcquireDriveRequest>,
    ) -> std::result::Result<Response<AcquireDriveResponse>, Status> {
        todo!()
    }
    async fn release_drive(
        &self,
        _req: Request<ReleaseDriveRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }
    async fn load_tape(
        &self,
        _req: Request<LoadTapeRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }
    async fn unload_tape(
        &self,
        _req: Request<UnloadTapeRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }
    async fn rewind(
        &self,
        _req: Request<RewindRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }
    async fn seek_to_filemark(
        &self,
        _req: Request<SeekToFilemarkRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }
    async fn get_tape_media_status(
        &self,
        _req: Request<GetTapeMediaStatusRequest>,
    ) -> std::result::Result<Response<TapeMediaStatus>, Status> {
        todo!()
    }
    async fn inventory(
        &self,
        _req: Request<()>,
    ) -> std::result::Result<Response<InventoryResponse>, Status> {
        todo!()
    }
}
