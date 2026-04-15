use crate::SchedulerState;
use coldstore_proto::common;
use coldstore_proto::scheduler::scheduler_service_server::SchedulerService;
use coldstore_proto::scheduler::*;
use prost_types::Timestamp;
#[cfg(test)]
use std::collections::HashMap;
use std::sync::Arc;
#[cfg(test)]
use std::sync::RwLock;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

#[tonic::async_trait]
pub trait Phase1SchedulerBackend: Send + Sync + 'static {
    async fn list_buckets(&self) -> std::result::Result<Vec<common::BucketInfo>, Status>;
    async fn create_bucket(&self, bucket: &str) -> std::result::Result<(), Status>;
    async fn delete_bucket(&self, bucket: &str) -> std::result::Result<(), Status>;
    async fn head_bucket(&self, bucket: &str) -> std::result::Result<(), Status>;
    async fn head_object(
        &self,
        bucket: &str,
        key: &str,
    ) -> std::result::Result<common::ObjectMetadata, Status>;
    async fn get_object(
        &self,
        bucket: &str,
        key: &str,
    ) -> std::result::Result<(common::ObjectMetadata, Vec<u8>), Status>;
    async fn list_objects(
        &self,
        bucket: &str,
        prefix: Option<&str>,
        marker: Option<&str>,
        max_keys: u32,
    ) -> std::result::Result<Vec<common::ObjectMetadata>, Status>;
}

struct RemoteSchedulerBackend;

#[tonic::async_trait]
impl Phase1SchedulerBackend for RemoteSchedulerBackend {
    async fn list_buckets(&self) -> std::result::Result<Vec<common::BucketInfo>, Status> {
        Err(phase1_unimplemented("scheduler.list_buckets"))
    }
    async fn create_bucket(&self, _bucket: &str) -> std::result::Result<(), Status> {
        Err(phase1_unimplemented("scheduler.create_bucket"))
    }
    async fn delete_bucket(&self, _bucket: &str) -> std::result::Result<(), Status> {
        Err(phase1_unimplemented("scheduler.delete_bucket"))
    }
    async fn head_bucket(&self, _bucket: &str) -> std::result::Result<(), Status> {
        Err(phase1_unimplemented("scheduler.head_bucket"))
    }
    async fn head_object(
        &self,
        _bucket: &str,
        _key: &str,
    ) -> std::result::Result<common::ObjectMetadata, Status> {
        Err(phase1_unimplemented("scheduler.head_object"))
    }
    async fn get_object(
        &self,
        _bucket: &str,
        _key: &str,
    ) -> std::result::Result<(common::ObjectMetadata, Vec<u8>), Status> {
        Err(phase1_unimplemented("scheduler.get_object"))
    }
    async fn list_objects(
        &self,
        _bucket: &str,
        _prefix: Option<&str>,
        _marker: Option<&str>,
        _max_keys: u32,
    ) -> std::result::Result<Vec<common::ObjectMetadata>, Status> {
        Err(phase1_unimplemented("scheduler.list_objects"))
    }
}

pub struct SchedulerServiceImpl {
    _state: Arc<SchedulerState>,
    backend: Arc<dyn Phase1SchedulerBackend>,
}

impl SchedulerServiceImpl {
    pub fn new(state: Arc<SchedulerState>) -> Self {
        Self {
            _state: state,
            backend: Arc::new(RemoteSchedulerBackend),
        }
    }

    pub fn new_with_backend(
        state: Arc<SchedulerState>,
        backend: Arc<dyn Phase1SchedulerBackend>,
    ) -> Self {
        Self {
            _state: state,
            backend,
        }
    }
}

fn phase1_unimplemented(op: &str) -> Status {
    Status::unimplemented(format!(
        "{op} is not implemented in phase-1 safe mode; use unit-tested metadata/cache services only"
    ))
}

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

fn build_get_object_meta(object: &common::ObjectMetadata) -> GetObjectMeta {
    GetObjectMeta {
        content_length: object.size,
        content_type: object.content_type.clone(),
        etag: object.etag.clone().unwrap_or_default(),
        storage_class: object.storage_class,
        restore_info: build_restore_info(object.restore_status, object.restore_expire_at.as_ref()),
        last_modified: object.updated_at,
    }
}

fn build_object_entry(object: &common::ObjectMetadata) -> ObjectEntry {
    ObjectEntry {
        key: object.key.clone(),
        last_modified: object.updated_at,
        etag: object.etag.clone().unwrap_or_default(),
        size: object.size,
        storage_class: storage_class_label(object.storage_class).into(),
    }
}

fn build_bucket_entry(bucket: &common::BucketInfo) -> BucketEntry {
    BucketEntry {
        name: bucket.name.clone(),
        creation_date: bucket.created_at,
    }
}

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

    type GetObjectStream = ReceiverStream<Result<GetObjectResponse, Status>>;

    async fn get_object(
        &self,
        request: Request<GetObjectRequest>,
    ) -> std::result::Result<Response<Self::GetObjectStream>, Status> {
        let request = request.into_inner();
        let (object, data) = self
            .backend
            .get_object(&request.bucket, &request.key)
            .await?;
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(GetObjectResponse {
                    payload: Some(get_object_response::Payload::Meta(build_get_object_meta(
                        &object,
                    ))),
                }))
                .await;
            let _ = tx
                .send(Ok(GetObjectResponse {
                    payload: Some(get_object_response::Payload::Data(data)),
                }))
                .await;
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn head_object(
        &self,
        request: Request<HeadObjectRequest>,
    ) -> std::result::Result<Response<HeadObjectResponse>, Status> {
        let request = request.into_inner();
        let object = self
            .backend
            .head_object(&request.bucket, &request.key)
            .await?;
        Ok(Response::new(build_head_object_response(&object)))
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
        request: Request<ListObjectsRequest>,
    ) -> std::result::Result<Response<ListObjectsResponse>, Status> {
        let request = request.into_inner();
        let objects = self
            .backend
            .list_objects(
                &request.bucket,
                request.prefix.as_deref(),
                request.marker.as_deref(),
                request.max_keys,
            )
            .await?;
        let is_truncated = request.max_keys > 0 && objects.len() > request.max_keys as usize;
        let next_marker = if is_truncated {
            objects
                .get(request.max_keys as usize - 1)
                .map(|object| object.key.clone())
        } else {
            None
        };
        Ok(Response::new(ListObjectsResponse {
            bucket: request.bucket,
            prefix: request.prefix,
            marker: request.marker,
            next_marker,
            max_keys: request.max_keys,
            is_truncated,
            contents: objects
                .into_iter()
                .take(if request.max_keys == 0 {
                    usize::MAX
                } else {
                    request.max_keys as usize
                })
                .map(|object| build_object_entry(&object))
                .collect(),
            common_prefixes: vec![],
        }))
    }

    async fn create_bucket(
        &self,
        request: Request<CreateBucketRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        self.backend
            .create_bucket(&request.into_inner().bucket)
            .await?;
        Ok(Response::new(()))
    }

    async fn delete_bucket(
        &self,
        request: Request<DeleteBucketRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        self.backend
            .delete_bucket(&request.into_inner().bucket)
            .await?;
        Ok(Response::new(()))
    }

    async fn head_bucket(
        &self,
        request: Request<HeadBucketRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        self.backend
            .head_bucket(&request.into_inner().bucket)
            .await?;
        Ok(Response::new(()))
    }

    async fn list_buckets(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<ListBucketsResponse>, Status> {
        let buckets = self.backend.list_buckets().await?;
        Ok(Response::new(ListBucketsResponse {
            buckets: buckets.iter().map(build_bucket_entry).collect(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coldstore_common::config::SchedulerConfig;
    use tokio_stream::StreamExt;

    #[derive(Default)]
    struct InMemoryBackend {
        buckets: RwLock<Vec<common::BucketInfo>>,
        objects: RwLock<HashMap<String, (common::ObjectMetadata, Vec<u8>)>>,
    }

    impl InMemoryBackend {
        fn with_fixture() -> Self {
            let bucket = common::BucketInfo {
                name: "docs".into(),
                created_at: Some(Timestamp {
                    seconds: 5,
                    nanos: 0,
                }),
                owner: None,
                versioning_enabled: false,
                object_count: 1,
                total_size: 42,
            };
            let object = common::ObjectMetadata {
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
            };
            let mut objects = HashMap::new();
            objects.insert("docs/readme.txt".into(), (object, b"hello world".to_vec()));
            Self {
                buckets: RwLock::new(vec![bucket]),
                objects: RwLock::new(objects),
            }
        }
    }

    #[tonic::async_trait]
    impl Phase1SchedulerBackend for InMemoryBackend {
        async fn list_buckets(&self) -> std::result::Result<Vec<common::BucketInfo>, Status> {
            Ok(self.buckets.read().unwrap().clone())
        }
        async fn create_bucket(&self, bucket: &str) -> std::result::Result<(), Status> {
            let mut buckets = self.buckets.write().unwrap();
            if buckets.iter().any(|b| b.name == bucket) {
                return Err(Status::already_exists("bucket exists"));
            }
            buckets.push(common::BucketInfo {
                name: bucket.into(),
                created_at: None,
                owner: None,
                versioning_enabled: false,
                object_count: 0,
                total_size: 0,
            });
            Ok(())
        }
        async fn delete_bucket(&self, bucket: &str) -> std::result::Result<(), Status> {
            let mut buckets = self.buckets.write().unwrap();
            let before = buckets.len();
            buckets.retain(|b| b.name != bucket);
            if buckets.len() == before {
                Err(Status::not_found("bucket missing"))
            } else {
                Ok(())
            }
        }
        async fn head_bucket(&self, bucket: &str) -> std::result::Result<(), Status> {
            if self
                .buckets
                .read()
                .unwrap()
                .iter()
                .any(|b| b.name == bucket)
            {
                Ok(())
            } else {
                Err(Status::not_found("bucket missing"))
            }
        }
        async fn head_object(
            &self,
            bucket: &str,
            key: &str,
        ) -> std::result::Result<common::ObjectMetadata, Status> {
            self.objects
                .read()
                .unwrap()
                .get(&format!("{bucket}/{key}"))
                .map(|(o, _)| o.clone())
                .ok_or_else(|| Status::not_found("object missing"))
        }
        async fn get_object(
            &self,
            bucket: &str,
            key: &str,
        ) -> std::result::Result<(common::ObjectMetadata, Vec<u8>), Status> {
            self.objects
                .read()
                .unwrap()
                .get(&format!("{bucket}/{key}"))
                .cloned()
                .ok_or_else(|| Status::not_found("object missing"))
        }
        async fn list_objects(
            &self,
            bucket: &str,
            prefix: Option<&str>,
            marker: Option<&str>,
            _max_keys: u32,
        ) -> std::result::Result<Vec<common::ObjectMetadata>, Status> {
            let prefix = prefix.unwrap_or_default();
            let marker = marker.unwrap_or_default();
            let mut objects: Vec<_> = self
                .objects
                .read()
                .unwrap()
                .values()
                .map(|(o, _)| o.clone())
                .filter(|o| o.bucket == bucket)
                .filter(|o| o.key.starts_with(prefix))
                .filter(|o| o.key.as_str() > marker)
                .collect();
            objects.sort_by(|a, b| a.key.cmp(&b.key));
            Ok(objects)
        }
    }

    fn service() -> SchedulerServiceImpl {
        let state = Arc::new(SchedulerState {
            metadata:
                coldstore_proto::metadata::metadata_service_client::MetadataServiceClient::new(
                    tonic::transport::Channel::from_static("http://127.0.0.1:1").connect_lazy(),
                ),
            cache: None,
            tape: None,
            config: SchedulerConfig::default(),
        });
        SchedulerServiceImpl::new_with_backend(state, Arc::new(InMemoryBackend::with_fixture()))
    }

    #[test]
    fn helper_head_object_response_contains_restore_info() {
        let (object, _) = InMemoryBackend::with_fixture()
            .objects
            .read()
            .unwrap()
            .get("docs/readme.txt")
            .unwrap()
            .clone();
        let response = build_head_object_response(&object);
        assert_eq!(response.content_length, 42);
        assert_eq!(response.etag, "etag-1");
        assert_eq!(
            response.restore_info.as_deref(),
            Some("ongoing-request=\"false\", expiry-ts=\"123\"")
        );
    }

    #[tokio::test]
    async fn get_object_stream_uses_backend() {
        let mut stream = service()
            .get_object(Request::new(GetObjectRequest {
                bucket: "docs".into(),
                key: "readme.txt".into(),
                version_id: None,
            }))
            .await
            .unwrap()
            .into_inner();
        let first = stream.next().await.unwrap().unwrap();
        match first.payload {
            Some(get_object_response::Payload::Meta(meta)) => assert_eq!(meta.etag, "etag-1"),
            _ => panic!("expected meta"),
        }
        let second = stream.next().await.unwrap().unwrap();
        match second.payload {
            Some(get_object_response::Payload::Data(bytes)) => assert_eq!(bytes, b"hello world"),
            _ => panic!("expected data"),
        }
    }

    #[tokio::test]
    async fn list_buckets_uses_backend() {
        let response = service()
            .list_buckets(Request::new(()))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.buckets.len(), 1);
        assert_eq!(response.buckets[0].name, "docs");
    }

    #[tokio::test]
    async fn create_bucket_uses_backend() {
        let svc = service();
        svc.create_bucket(Request::new(CreateBucketRequest {
            bucket: "new-bucket".into(),
        }))
        .await
        .unwrap();
        let response = svc
            .list_buckets(Request::new(()))
            .await
            .unwrap()
            .into_inner();
        assert!(response.buckets.iter().any(|b| b.name == "new-bucket"));
    }

    #[tokio::test]
    async fn delete_bucket_uses_backend() {
        let svc = service();
        svc.delete_bucket(Request::new(DeleteBucketRequest {
            bucket: "docs".into(),
        }))
        .await
        .unwrap();
        let response = svc
            .list_buckets(Request::new(()))
            .await
            .unwrap()
            .into_inner();
        assert!(!response.buckets.iter().any(|b| b.name == "docs"));
    }

    #[tokio::test]
    async fn head_bucket_uses_backend() {
        assert!(service()
            .head_bucket(Request::new(HeadBucketRequest {
                bucket: "docs".into()
            }))
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn head_object_uses_backend() {
        let response = service()
            .head_object(Request::new(HeadObjectRequest {
                bucket: "docs".into(),
                key: "readme.txt".into(),
                version_id: None,
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.content_length, 42);
        assert_eq!(response.etag, "etag-1");
    }

    #[tokio::test]
    async fn list_objects_uses_backend() {
        let response = service()
            .list_objects(Request::new(ListObjectsRequest {
                bucket: "docs".into(),
                prefix: Some("read".into()),
                marker: None,
                delimiter: None,
                max_keys: 100,
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.contents.len(), 1);
        assert_eq!(response.contents[0].key, "readme.txt");
        assert_eq!(response.contents[0].storage_class, "COLD");
    }

    #[test]
    fn phase1_unimplemented_message_is_stable() {
        let status = phase1_unimplemented("scheduler.list_buckets");
        assert_eq!(status.code(), tonic::Code::Unimplemented);
        assert!(status.message().contains("phase-1 safe mode"));
    }
}
