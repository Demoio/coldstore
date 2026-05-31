use coldstore_common::config::TapeConfig;
use coldstore_proto::common::{DriveStatus, TapeStatus};
use coldstore_proto::tape::read_bundle_request::Location;
use coldstore_proto::tape::read_bundle_response::Payload as ReadPayload;
use coldstore_proto::tape::tape_service_server::TapeService;
use coldstore_proto::tape::write_bundle_request::Payload as WritePayload;
use coldstore_proto::tape::{
    LoadTapeRequest, ReadBundleRequest, RewindRequest, SeekToFilemarkRequest, UnloadTapeRequest,
    WriteBundleMeta, WriteBundleRequest,
};
use coldstore_tape::service::{SimulatorTapeBackend, TapeServiceImpl};
use tokio_stream::StreamExt;
use tonic::Request;

#[tokio::test]
async fn simulator_backend_exposes_inventory_and_drive_operations_through_service() {
    let backend = SimulatorTapeBackend::new(3, 1);
    backend.insert_tape("slot-1", "TAPE0001L9").unwrap();
    let service = TapeServiceImpl::new_with_backend(TapeConfig::default(), backend);

    let drives = service
        .list_drives(Request::new(()))
        .await
        .unwrap()
        .into_inner()
        .drives;
    assert_eq!(drives.len(), 1);
    assert_eq!(drives[0].drive_id, "drive-0");
    assert_eq!(drives[0].status, DriveStatus::DriveIdle as i32);
    assert_eq!(drives[0].current_tape.as_deref(), None);

    let inventory = service
        .inventory(Request::new(()))
        .await
        .unwrap()
        .into_inner();
    let slot_1 = inventory
        .slots
        .iter()
        .find(|slot| slot.slot_id == "slot-1")
        .unwrap();
    assert!(!slot_1.is_drive);
    assert_eq!(slot_1.tape_id.as_deref(), Some("TAPE0001L9"));

    service
        .load_tape(Request::new(LoadTapeRequest {
            tape_id: "TAPE0001L9".to_string(),
            drive_id: "drive-0".to_string(),
            slot_id: None,
        }))
        .await
        .unwrap();

    let status = service
        .get_drive_status(Request::new(coldstore_proto::tape::GetDriveStatusRequest {
            drive_id: "drive-0".to_string(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(status.current_tape.as_deref(), Some("TAPE0001L9"));

    let media = service
        .get_tape_media_status(Request::new(
            coldstore_proto::tape::GetTapeMediaStatusRequest {
                drive_id: "drive-0".to_string(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(media.tape_id.as_deref(), Some("TAPE0001L9"));
    assert_eq!(media.tape_status, TapeStatus::TapeOnline as i32);

    service
        .unload_tape(Request::new(UnloadTapeRequest {
            drive_id: "drive-0".to_string(),
            target_slot_id: Some("slot-2".to_string()),
        }))
        .await
        .unwrap();

    let inventory = service
        .inventory(Request::new(()))
        .await
        .unwrap()
        .into_inner();
    let slot_2 = inventory
        .slots
        .iter()
        .find(|slot| slot.slot_id == "slot-2")
        .unwrap();
    assert_eq!(slot_2.tape_id.as_deref(), Some("TAPE0001L9"));
}

#[tokio::test]
async fn simulator_service_writes_filemark_delimited_bundles_and_reads_them_back() {
    let backend = SimulatorTapeBackend::new(2, 1);
    backend.insert_tape("slot-1", "TAPE0002L9").unwrap();
    let service = TapeServiceImpl::new_with_backend(TapeConfig::default(), backend);

    service
        .load_tape(Request::new(LoadTapeRequest {
            tape_id: "TAPE0002L9".to_string(),
            drive_id: "drive-0".to_string(),
            slot_id: Some("slot-1".to_string()),
        }))
        .await
        .unwrap();

    let response = service
        .write_bundle_from_messages(tokio_stream::iter(vec![
            Ok(WriteBundleRequest {
                payload: Some(WritePayload::Meta(WriteBundleMeta {
                    drive_id: "drive-0".to_string(),
                    bundle_id: "bundle-a".to_string(),
                    total_size: 6,
                    object_count: 1,
                    block_size: 262_144,
                })),
            }),
            Ok(WriteBundleRequest {
                payload: Some(WritePayload::Data(b"abc".to_vec())),
            }),
            Ok(WriteBundleRequest {
                payload: Some(WritePayload::Data(b"def".to_vec())),
            }),
        ]))
        .await
        .unwrap();
    assert!(response.success);
    assert_eq!(response.bundle_id, "bundle-a");
    assert_eq!(response.bytes_written, 6);
    assert_eq!(response.filemark_start, 0);
    assert_eq!(response.filemark_end, 1);

    let media = service
        .get_tape_media_status(Request::new(
            coldstore_proto::tape::GetTapeMediaStatusRequest {
                drive_id: "drive-0".to_string(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(media.used_bytes, 6);
    assert_eq!(media.current_filemark, 1);

    service
        .rewind(Request::new(RewindRequest {
            drive_id: "drive-0".to_string(),
        }))
        .await
        .unwrap();

    let mut stream = service
        .read_bundle(Request::new(ReadBundleRequest {
            drive_id: "drive-0".to_string(),
            location: Some(Location::Filemark(0)),
            length: 6,
        }))
        .await
        .unwrap()
        .into_inner();

    let meta = stream.next().await.unwrap().unwrap().payload.unwrap();
    assert!(matches!(meta, ReadPayload::Meta(_)));
    let data = stream.next().await.unwrap().unwrap().payload.unwrap();
    assert_eq!(data, ReadPayload::Data(b"abcdef".to_vec()));
    assert!(stream.next().await.is_none());

    service
        .seek_to_filemark(Request::new(SeekToFilemarkRequest {
            drive_id: "drive-0".to_string(),
            filemark: 1,
        }))
        .await
        .unwrap();
}
