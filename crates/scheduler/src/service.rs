use crate::SchedulerState;
use coldstore_proto::common;
use coldstore_proto::scheduler::scheduler_service_server::SchedulerService;
use coldstore_proto::scheduler::*;
use prost_types::Timestamp;
use std::sync::Arc;
use tonic::{Request, Response, Status, Streaming};

pub struct SchedulerServiceImpl {
    _state: Arc<SchedulerState>,
}

impl SchedulerServiceImpl {
    pub fn new(state: Arc<SchedulerState>) -> Self {
        Self { _state: state }
    }
}

fn phase1_unimplemented(op: &str) -> Status {
    Status::unimplemented(format!(
        "{op} is not implemented in phase-1 safe mode; use unit-tested metadata/cache services only"
    ))
}

#[allow(dead_code)]
fn build_restore_info(
    restore_status: Option<i32>,
    restore_expire_at: Option<&Timestamp>,
) -> Option<String> {
    match restore_status.and_then(|status| common::RestoreStatus::try_from(status).ok()) {
        Some(common::RestoreStatus::RestoreInProgress)
        | Some(common::RestoreStatus::RestorePending)
        | Some(common::RestoreStatus::RestoreWaitingForMedia) => {
            Some("ongoing-request=\"true\"".into())
        }
        Some(common::RestoreStatus::RestoreCompleted) => {
            if let Some(expire_at) = restore_expire_at {
                Some(format!(
                    "ongoing-request=\"false\", expiry-ts=\"{}\"",
                    expire_at.seconds
                ))
            } else {
                Some("ongoing-request=\"false\"".into())
            }
        }
        _ => None,
    }
}

#[allow(dead_code)]
fn build_head_object_response(object: &common::ObjectMetadata) -> HeadObjectResponse {
    HeadObjectResponse {
        content_length: object.size,
        content_type: object.content_type.clone(),
        etag: object.etag.clone().unwrap_or_default(),
        storage_class: object.storage_class,
        restore_info: build_restore_info(object.restore_status, object.restore_expire_at.as_ref()),
        last_modified: object.updated_at,
    }
}

#[allow(dead_code)]
fn build_object_entry(object: &common::ObjectMetadata) -> ObjectEntry {
    ObjectEntry {
        key: object.key.clone(),
        last_modified: object.updated_at,
        etag: object.etag.clone().unwrap_or_default(),
        size: object.size,
        storage_class: storage_class_label(object.storage_class).into(),
    }
}

#[allow(dead_code)]
fn build_bucket_entry(bucket: &common::BucketInfo) -> BucketEntry {
    BucketEntry {
        name: bucket.name.clone(),
        creation_date: bucket.created_at,
    }
}

#[allow(dead_code)]
fn storage_class_label(storage_class: i32) -> &'static str {
    match common::StorageClass::try_from(storage_class).ok() {
        Some(common::StorageClass::ColdPending) => "COLD_PENDING",
        Some(common::StorageClass::Cold) => "COLD",
        _ => "UNKNOWN",
    }
}

#[tonic::async_trait]
impl SchedulerService for SchedulerServiceImpl {
    async fn put_object(
        &self,
        _request: Request<Streaming<PutObjectRequest>>,
    ) -> std::result::Result<Response<PutObjectResponse>, Status> {
        Err(phase1_unimplemented("scheduler.put_object"))
    }

    type GetObjectStream =
        tokio_stream::wrappers::ReceiverStream<Result<GetObjectResponse, Status>>;

    async fn get_object(
        &self,
        _request: Request<GetObjectRequest>,
    ) -> std::result::Result<Response<Self::GetObjectStream>, Status> {
        Err(phase1_unimplemented("scheduler.get_object"))
    }

    async fn head_object(
        &self,
        _request: Request<HeadObjectRequest>,
    ) -> std::result::Result<Response<HeadObjectResponse>, Status> {
        Err(phase1_unimplemented("scheduler.head_object"))
    }

    async fn delete_object(
        &self,
        _request: Request<DeleteObjectRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        Err(phase1_unimplemented("scheduler.delete_object"))
    }

    async fn restore_object(
        &self,
        _request: Request<RestoreObjectRequest>,
    ) -> std::result::Result<Response<RestoreObjectResponse>, Status> {
        Err(phase1_unimplemented("scheduler.restore_object"))
    }

    async fn list_objects(
        &self,
        _request: Request<ListObjectsRequest>,
    ) -> std::result::Result<Response<ListObjectsResponse>, Status> {
        Err(phase1_unimplemented("scheduler.list_objects"))
    }

    async fn create_bucket(
        &self,
        _request: Request<CreateBucketRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        Err(phase1_unimplemented("scheduler.create_bucket"))
    }

    async fn delete_bucket(
        &self,
        _request: Request<DeleteBucketRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        Err(phase1_unimplemented("scheduler.delete_bucket"))
    }

    async fn head_bucket(
        &self,
        _request: Request<HeadBucketRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        Err(phase1_unimplemented("scheduler.head_bucket"))
    }

    async fn list_buckets(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<ListBucketsResponse>, Status> {
        Err(phase1_unimplemented("scheduler.list_buckets"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_object() -> common::ObjectMetadata {
        common::ObjectMetadata {
            bucket: "docs".into(),
            key: "readme.txt".into(),
            version_id: None,
            size: 42,
            checksum: "sum".into(),
            content_type: Some("text/plain".into()),
            etag: Some("etag-1".into()),
            storage_class: common::StorageClass::Cold as i32,
            archive_id: Some("archive-1".into()),
            tape_id: Some("tape-1".into()),
            tape_set: vec!["tape-1".into()],
            tape_block_offset: Some(1),
            restore_status: Some(common::RestoreStatus::RestoreCompleted as i32),
            restore_expire_at: Some(Timestamp {
                seconds: 123,
                nanos: 0,
            }),
            created_at: Some(Timestamp {
                seconds: 1,
                nanos: 0,
            }),
            updated_at: Some(Timestamp {
                seconds: 2,
                nanos: 0,
            }),
        }
    }

    #[test]
    fn head_object_response_contains_restore_info() {
        let object = test_object();
        let response = build_head_object_response(&object);
        assert_eq!(response.content_length, 42);
        assert_eq!(response.etag, "etag-1");
        assert_eq!(
            response.restore_info.as_deref(),
            Some("ongoing-request=\"false\", expiry-ts=\"123\"")
        );
    }

    #[test]
    fn object_entry_uses_storage_class_label() {
        let object = test_object();
        let entry = build_object_entry(&object);
        assert_eq!(entry.key, "readme.txt");
        assert_eq!(entry.storage_class, "COLD");
    }

    #[test]
    fn bucket_entry_preserves_name_and_creation_date() {
        let bucket = common::BucketInfo {
            name: "docs".into(),
            created_at: Some(Timestamp {
                seconds: 5,
                nanos: 0,
            }),
            owner: None,
            versioning_enabled: false,
            object_count: 0,
            total_size: 0,
        };
        let entry = build_bucket_entry(&bucket);
        assert_eq!(entry.name, "docs");
        assert_eq!(entry.creation_date.unwrap().seconds, 5);
    }

    #[test]
    fn phase1_unimplemented_message_is_stable() {
        let status = phase1_unimplemented("scheduler.list_buckets");
        assert_eq!(status.code(), tonic::Code::Unimplemented);
        assert!(status.message().contains("phase-1 safe mode"));
    }
}
