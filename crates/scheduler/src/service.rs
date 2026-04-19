use crate::SchedulerState;
use coldstore_proto::cache::cache_service_client::CacheServiceClient;
use coldstore_proto::cache::{
    self, put_restored_request, put_staging_request, ContainsRequest, DeleteRequest,
    DeleteStagingRequest, GetRequest as CacheGetRequest, GetStagingRequest, PutRestoredMeta,
    PutRestoredRequest, PutStagingMeta, PutStagingRequest,
};
use coldstore_proto::common;
use coldstore_proto::scheduler::scheduler_service_server::SchedulerService;
use coldstore_proto::scheduler::*;
use prost_types::Timestamp;
use sha2::{Digest, Sha256};
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
    async fn put_object(
        &self,
        bucket: &str,
        key: &str,
        body: Vec<u8>,
        content_type: Option<String>,
    ) -> std::result::Result<PutObjectResponse, Status>;
    async fn delete_object(&self, bucket: &str, key: &str) -> std::result::Result<(), Status>;
    async fn restore_object(
        &self,
        bucket: &str,
        key: &str,
        days: u32,
        tier: common::RestoreTier,
    ) -> std::result::Result<RestoreObjectResponse, Status>;
    async fn archive_staging_batch(&self, limit: u32) -> std::result::Result<Vec<String>, Status>;
    async fn list_objects(
        &self,
        bucket: &str,
        prefix: Option<&str>,
        marker: Option<&str>,
        max_keys: u32,
    ) -> std::result::Result<Vec<common::ObjectMetadata>, Status>;
}

struct MetadataBackedSchedulerBackend {
    metadata: coldstore_proto::metadata::metadata_service_client::MetadataServiceClient<
        tonic::transport::Channel,
    >,
    cache: Option<CacheServiceClient<tonic::transport::Channel>>,
}

impl MetadataBackedSchedulerBackend {
    fn new(
        metadata: coldstore_proto::metadata::metadata_service_client::MetadataServiceClient<
            tonic::transport::Channel,
        >,
        cache: Option<CacheServiceClient<tonic::transport::Channel>>,
    ) -> Self {
        Self { metadata, cache }
    }

    #[allow(clippy::result_large_err)]
    fn cache_client(
        &self,
    ) -> std::result::Result<CacheServiceClient<tonic::transport::Channel>, Status> {
        self.cache.clone().ok_or_else(|| {
            Status::failed_precondition(
                "scheduler cache client is not configured; restore/get body paths require cache wiring",
            )
        })
    }

    async fn archive_staging_batch(&self, limit: u32) -> std::result::Result<Vec<String>, Status> {
        let mut cache = self.cache_client()?;
        let staging = cache
            .list_staging_keys(Request::new(cache::ListStagingKeysRequest {
                limit,
                after: None,
            }))
            .await?
            .into_inner();

        let mut metadata = self.metadata.clone();
        let mut archived = Vec::new();
        for entry in staging.entries {
            let archive_id = phase1_archive_id(&entry.bucket, &entry.key);
            metadata
                .update_archive_location(Request::new(
                    coldstore_proto::metadata::UpdateArchiveLocationRequest {
                        bucket: entry.bucket.clone(),
                        key: entry.key.clone(),
                        archive_id,
                        tape_id: phase1_archive_tape_id().to_string(),
                        tape_set: vec![phase1_archive_tape_id().to_string()],
                        tape_block_offset: 0,
                    },
                ))
                .await?;
            metadata
                .update_storage_class(Request::new(
                    coldstore_proto::metadata::UpdateStorageClassRequest {
                        bucket: entry.bucket.clone(),
                        key: entry.key.clone(),
                        storage_class: common::StorageClass::Cold as i32,
                    },
                ))
                .await?;
            cache
                .delete_staging(Request::new(DeleteStagingRequest {
                    bucket: entry.bucket.clone(),
                    key: entry.key.clone(),
                    version_id: entry.version_id.clone(),
                }))
                .await?;
            archived.push(format!("{}/{}", entry.bucket, entry.key));
        }
        Ok(archived)
    }
}

#[tonic::async_trait]
impl Phase1SchedulerBackend for MetadataBackedSchedulerBackend {
    async fn list_buckets(&self) -> std::result::Result<Vec<common::BucketInfo>, Status> {
        let mut client = self.metadata.clone();
        Ok(client
            .list_buckets(Request::new(()))
            .await?
            .into_inner()
            .buckets)
    }

    async fn create_bucket(&self, bucket: &str) -> std::result::Result<(), Status> {
        let mut client = self.metadata.clone();
        client
            .create_bucket(Request::new(common::BucketInfo {
                name: bucket.into(),
                created_at: Some(now_timestamp()),
                owner: None,
                versioning_enabled: false,
                object_count: 0,
                total_size: 0,
            }))
            .await?;
        Ok(())
    }

    async fn delete_bucket(&self, bucket: &str) -> std::result::Result<(), Status> {
        let mut client = self.metadata.clone();
        client
            .delete_bucket(Request::new(
                coldstore_proto::metadata::DeleteBucketRequest {
                    name: bucket.into(),
                },
            ))
            .await?;
        Ok(())
    }

    async fn head_bucket(&self, bucket: &str) -> std::result::Result<(), Status> {
        let mut client = self.metadata.clone();
        client
            .get_bucket(Request::new(coldstore_proto::metadata::GetBucketRequest {
                name: bucket.into(),
            }))
            .await?;
        Ok(())
    }

    async fn head_object(
        &self,
        bucket: &str,
        key: &str,
    ) -> std::result::Result<common::ObjectMetadata, Status> {
        let mut client = self.metadata.clone();
        Ok(client
            .head_object(Request::new(coldstore_proto::metadata::HeadObjectRequest {
                bucket: bucket.into(),
                key: key.into(),
            }))
            .await?
            .into_inner())
    }

    async fn get_object(
        &self,
        bucket: &str,
        key: &str,
    ) -> std::result::Result<(common::ObjectMetadata, Vec<u8>), Status> {
        let object = self.head_object(bucket, key).await?;
        let mut cache = self.cache_client()?;
        let mut stream = cache
            .get(Request::new(CacheGetRequest {
                bucket: bucket.into(),
                key: key.into(),
                version_id: None,
            }))
            .await
            .map_err(|status| {
                if status.code() == tonic::Code::NotFound {
                    Status::failed_precondition(format!(
                        "object {bucket}/{key} is not currently restored in cache"
                    ))
                } else {
                    status
                }
            })?
            .into_inner();

        let first = stream
            .message()
            .await?
            .ok_or_else(|| Status::internal("missing restored cache metadata chunk"))?;
        match first.payload {
            Some(cache::get_response::Payload::Meta(_)) => {}
            _ => {
                return Err(Status::internal(
                    "first restored cache chunk was not metadata",
                ))
            }
        }

        let mut body = Vec::new();
        while let Some(chunk) = stream.message().await? {
            if let Some(cache::get_response::Payload::Data(bytes)) = chunk.payload {
                body.extend_from_slice(&bytes);
            }
        }
        Ok((object, body))
    }

    async fn put_object(
        &self,
        bucket: &str,
        key: &str,
        body: Vec<u8>,
        content_type: Option<String>,
    ) -> std::result::Result<PutObjectResponse, Status> {
        let checksum = sha256_hex(&body);
        let now = now_timestamp();
        let object = common::ObjectMetadata {
            bucket: bucket.into(),
            key: key.into(),
            version_id: None,
            size: body.len() as u64,
            checksum: checksum.clone(),
            content_type: content_type.clone(),
            etag: Some(checksum.clone()),
            storage_class: common::StorageClass::ColdPending as i32,
            archive_id: None,
            tape_id: None,
            tape_set: vec![],
            tape_block_offset: None,
            restore_status: None,
            restore_expire_at: None,
            created_at: Some(now),
            updated_at: Some(now),
        };
        let mut client = self.metadata.clone();
        client.put_object(Request::new(object)).await?;

        if let Some(mut cache) = self.cache.clone() {
            cache
                .put_staging(Request::new(tokio_stream::iter(vec![
                    PutStagingRequest {
                        payload: Some(put_staging_request::Payload::Meta(PutStagingMeta {
                            bucket: bucket.into(),
                            key: key.into(),
                            version_id: None,
                            size: body.len() as u64,
                            checksum: Some(checksum.clone()),
                            content_type,
                            etag: Some(checksum.clone()),
                        })),
                    },
                    PutStagingRequest {
                        payload: Some(put_staging_request::Payload::Data(body)),
                    },
                ])))
                .await?;
        }

        Ok(PutObjectResponse {
            etag: checksum,
            version_id: String::new(),
        })
    }

    async fn delete_object(&self, bucket: &str, key: &str) -> std::result::Result<(), Status> {
        let mut client = self.metadata.clone();
        client
            .delete_object(Request::new(
                coldstore_proto::metadata::DeleteObjectRequest {
                    bucket: bucket.into(),
                    key: key.into(),
                },
            ))
            .await?;

        if let Some(mut cache) = self.cache.clone() {
            let _ = cache
                .delete(Request::new(DeleteRequest {
                    bucket: bucket.into(),
                    key: key.into(),
                    version_id: None,
                }))
                .await;
            let _ = cache
                .delete_staging(Request::new(DeleteStagingRequest {
                    bucket: bucket.into(),
                    key: key.into(),
                    version_id: None,
                }))
                .await;
        }
        Ok(())
    }

    async fn restore_object(
        &self,
        bucket: &str,
        key: &str,
        days: u32,
        _tier: common::RestoreTier,
    ) -> std::result::Result<RestoreObjectResponse, Status> {
        let mut client = self.metadata.clone();
        let object = client
            .get_object(Request::new(coldstore_proto::metadata::GetObjectRequest {
                bucket: bucket.into(),
                key: key.into(),
            }))
            .await?
            .into_inner();

        if object.storage_class != common::StorageClass::Cold as i32 {
            return Err(Status::failed_precondition(
                "restore_object requires an archived COLD object in phase-1 metadata-backed mode",
            ));
        }

        let expire_at = days_from_now(days.max(1));
        let mut cache = self.cache_client()?;
        let contains = cache
            .contains(Request::new(ContainsRequest {
                bucket: bucket.into(),
                key: key.into(),
                version_id: None,
            }))
            .await?
            .into_inner();

        let restore_status = object
            .restore_status
            .and_then(|status| common::RestoreStatus::try_from(status).ok());

        if matches!(
            restore_status,
            Some(common::RestoreStatus::RestoreCompleted)
        ) && contains.exists
        {
            return Ok(RestoreObjectResponse { status_code: 200 });
        }

        match restore_status {
            Some(
                common::RestoreStatus::RestorePending
                | common::RestoreStatus::RestoreWaitingForMedia
                | common::RestoreStatus::RestoreInProgress,
            ) => Ok(RestoreObjectResponse { status_code: 202 }),
            Some(common::RestoreStatus::RestoreExpired | common::RestoreStatus::RestoreFailed) => {
                Err(Status::failed_precondition(
                    "restore_object cannot reopen expired or failed restores in phase-1 metadata-backed mode",
                ))
            }
            Some(common::RestoreStatus::RestoreCompleted)
            | Some(common::RestoreStatus::Unspecified)
            | None => {
                let mut staging = cache
                    .get_staging(Request::new(GetStagingRequest {
                        bucket: bucket.into(),
                        key: key.into(),
                        version_id: None,
                    }))
                    .await
                    .map_err(|status| {
                        if status.code() == tonic::Code::NotFound {
                            Status::failed_precondition(format!(
                                "restore_object requires staged bytes for {bucket}/{key} in phase-1 mode"
                            ))
                        } else {
                            status
                        }
                    })?
                    .into_inner();

                let first = staging
                    .message()
                    .await?
                    .ok_or_else(|| Status::internal("missing staging metadata chunk"))?;
                let staging_meta = match first.payload {
                    Some(cache::get_staging_response::Payload::Meta(meta)) => meta,
                    _ => return Err(Status::internal("first staging chunk was not metadata")),
                };
                let mut body = Vec::new();
                while let Some(chunk) = staging.message().await? {
                    if let Some(cache::get_staging_response::Payload::Data(bytes)) = chunk.payload {
                        body.extend_from_slice(&bytes);
                    }
                }

                client
                    .update_restore_status(Request::new(
                        coldstore_proto::metadata::UpdateRestoreStatusRequest {
                            bucket: bucket.into(),
                            key: key.into(),
                            status: common::RestoreStatus::RestorePending as i32,
                            expire_at: Some(expire_at),
                        },
                    ))
                    .await?;
                client
                    .update_restore_status(Request::new(
                        coldstore_proto::metadata::UpdateRestoreStatusRequest {
                            bucket: bucket.into(),
                            key: key.into(),
                            status: common::RestoreStatus::RestoreInProgress as i32,
                            expire_at: Some(expire_at),
                        },
                    ))
                    .await?;
                cache
                    .put_restored(Request::new(tokio_stream::iter(vec![
                        PutRestoredRequest {
                            payload: Some(put_restored_request::Payload::Meta(PutRestoredMeta {
                                bucket: bucket.into(),
                                key: key.into(),
                                version_id: staging_meta.version_id,
                                size: staging_meta.size,
                                checksum: staging_meta.checksum,
                                content_type: staging_meta.content_type,
                                etag: staging_meta.etag,
                                expire_at: Some(expire_at),
                            })),
                        },
                        PutRestoredRequest {
                            payload: Some(put_restored_request::Payload::Data(body)),
                        },
                    ])))
                    .await?;
                client
                    .update_restore_status(Request::new(
                        coldstore_proto::metadata::UpdateRestoreStatusRequest {
                            bucket: bucket.into(),
                            key: key.into(),
                            status: common::RestoreStatus::RestoreCompleted as i32,
                            expire_at: Some(expire_at),
                        },
                    ))
                    .await?;
                Ok(RestoreObjectResponse { status_code: 202 })
            }
        }
    }

    async fn archive_staging_batch(&self, limit: u32) -> std::result::Result<Vec<String>, Status> {
        MetadataBackedSchedulerBackend::archive_staging_batch(self, limit).await
    }

    async fn list_objects(
        &self,
        bucket: &str,
        prefix: Option<&str>,
        marker: Option<&str>,
        max_keys: u32,
    ) -> std::result::Result<Vec<common::ObjectMetadata>, Status> {
        let mut client = self.metadata.clone();
        Ok(client
            .list_objects(Request::new(
                coldstore_proto::metadata::ListObjectsRequest {
                    bucket: bucket.into(),
                    prefix: prefix.map(str::to_owned),
                    marker: marker.map(str::to_owned),
                    max_keys,
                },
            ))
            .await?
            .into_inner()
            .objects)
    }
}

pub struct SchedulerServiceImpl {
    _state: Arc<SchedulerState>,
    backend: Arc<dyn Phase1SchedulerBackend>,
}

impl SchedulerServiceImpl {
    pub fn new(state: Arc<SchedulerState>) -> Self {
        let backend = Arc::new(MetadataBackedSchedulerBackend::new(
            state.metadata.clone(),
            state.cache.clone(),
        ));
        Self {
            _state: state,
            backend,
        }
    }

    pub async fn archive_staging_batch(
        &self,
        limit: u32,
    ) -> std::result::Result<Vec<String>, Status> {
        self.backend.archive_staging_batch(limit).await
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

#[cfg(test)]
fn phase1_unimplemented(op: &str) -> Status {
    Status::unimplemented(format!(
        "{op} is not implemented in phase-1 safe mode; use unit-tested metadata/cache services only"
    ))
}

fn sha256_hex(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    format!("{:x}", hasher.finalize())
}

fn now_timestamp() -> Timestamp {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    Timestamp {
        seconds: now.as_secs() as i64,
        nanos: now.subsec_nanos() as i32,
    }
}

fn days_from_now(days: u32) -> Timestamp {
    let now = chrono::Utc::now() + chrono::Duration::days(days as i64);
    Timestamp {
        seconds: now.timestamp(),
        nanos: now.timestamp_subsec_nanos() as i32,
    }
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

fn phase1_archive_tape_id() -> &'static str {
    "phase1-cache-archive"
}

fn phase1_archive_id(bucket: &str, key: &str) -> String {
    format!("phase1-archive:{bucket}/{key}")
}

#[tonic::async_trait]
impl SchedulerService for SchedulerServiceImpl {
    async fn put_object(
        &self,
        request: Request<Streaming<PutObjectRequest>>,
    ) -> std::result::Result<Response<PutObjectResponse>, Status> {
        let mut stream = request.into_inner();
        let mut meta: Option<PutObjectMeta> = None;
        let mut body = Vec::new();
        while let Some(chunk) = stream.message().await? {
            match chunk.payload {
                Some(put_object_request::Payload::Meta(m)) => meta = Some(m),
                Some(put_object_request::Payload::Data(bytes)) => body.extend_from_slice(&bytes),
                None => return Err(Status::invalid_argument("empty put_object chunk")),
            }
        }
        let meta = meta.ok_or_else(|| Status::invalid_argument("missing put_object metadata"))?;
        if meta.content_length != body.len() as u64 {
            return Err(Status::invalid_argument(
                "content_length does not match body size",
            ));
        }
        let response = self
            .backend
            .put_object(&meta.bucket, &meta.key, body, meta.content_type)
            .await?;
        Ok(Response::new(response))
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
        request: Request<DeleteObjectRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        let request = request.into_inner();
        self.backend
            .delete_object(&request.bucket, &request.key)
            .await?;
        Ok(Response::new(()))
    }

    async fn restore_object(
        &self,
        request: Request<RestoreObjectRequest>,
    ) -> std::result::Result<Response<RestoreObjectResponse>, Status> {
        let request = request.into_inner();
        let tier = common::RestoreTier::try_from(request.tier)
            .map_err(|_| Status::invalid_argument("invalid restore tier"))?;
        let response = self
            .backend
            .restore_object(&request.bucket, &request.key, request.days, tier)
            .await?;
        Ok(Response::new(response))
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
    use coldstore_cache::service::CacheServiceImpl;
    use coldstore_common::config::{
        CacheBackendConfig, CacheConfig, MetadataConfig, SchedulerConfig,
    };
    use coldstore_metadata::service::MetadataServiceImpl;
    use coldstore_proto::cache::cache_service_server::CacheServiceServer;
    use coldstore_proto::cache::{
        cache_service_client::CacheServiceClient, put_restored_request, put_staging_request,
        ContainsRequest, GetRequest as CacheGetRequest, PutRestoredMeta, PutRestoredRequest,
        PutStagingMeta, PutStagingRequest,
    };
    use coldstore_proto::metadata::metadata_service_server::MetadataServiceServer;
    use tokio::sync::oneshot;
    use tokio::time::{sleep, Duration};
    use tokio_stream::StreamExt;
    use tonic::transport::Server;

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
        async fn put_object(
            &self,
            bucket: &str,
            key: &str,
            body: Vec<u8>,
            content_type: Option<String>,
        ) -> std::result::Result<PutObjectResponse, Status> {
            let object = common::ObjectMetadata {
                bucket: bucket.into(),
                key: key.into(),
                version_id: None,
                size: body.len() as u64,
                checksum: "sum".into(),
                content_type,
                etag: Some("etag-put".into()),
                storage_class: common::StorageClass::ColdPending as i32,
                archive_id: None,
                tape_id: None,
                tape_set: vec![],
                tape_block_offset: None,
                restore_status: None,
                restore_expire_at: None,
                created_at: Some(Timestamp {
                    seconds: 10,
                    nanos: 0,
                }),
                updated_at: Some(Timestamp {
                    seconds: 10,
                    nanos: 0,
                }),
            };
            self.objects
                .write()
                .unwrap()
                .insert(format!("{bucket}/{key}"), (object, body));
            Ok(PutObjectResponse {
                etag: "etag-put".into(),
                version_id: "v1".into(),
            })
        }
        async fn delete_object(&self, bucket: &str, key: &str) -> std::result::Result<(), Status> {
            if self
                .objects
                .write()
                .unwrap()
                .remove(&format!("{bucket}/{key}"))
                .is_some()
            {
                Ok(())
            } else {
                Err(Status::not_found("object missing"))
            }
        }
        async fn restore_object(
            &self,
            bucket: &str,
            key: &str,
            _days: u32,
            _tier: common::RestoreTier,
        ) -> std::result::Result<RestoreObjectResponse, Status> {
            let mut objects = self.objects.write().unwrap();
            let (object, _) = objects
                .get_mut(&format!("{bucket}/{key}"))
                .ok_or_else(|| Status::not_found("object missing"))?;
            let status_code =
                if object.restore_status == Some(common::RestoreStatus::RestoreCompleted as i32) {
                    200
                } else {
                    202
                };
            object.restore_status = Some(common::RestoreStatus::RestoreCompleted as i32);
            object.restore_expire_at = Some(Timestamp {
                seconds: 999,
                nanos: 0,
            });
            Ok(RestoreObjectResponse { status_code })
        }
        async fn archive_staging_batch(
            &self,
            _limit: u32,
        ) -> std::result::Result<Vec<String>, Status> {
            Ok(vec![])
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
    async fn delete_object_uses_backend() {
        let svc = service();
        svc.delete_object(Request::new(DeleteObjectRequest {
            bucket: "docs".into(),
            key: "readme.txt".into(),
            version_id: None,
        }))
        .await
        .unwrap();
        assert!(svc
            .head_object(Request::new(HeadObjectRequest {
                bucket: "docs".into(),
                key: "readme.txt".into(),
                version_id: None
            }))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn restore_object_uses_backend() {
        let response = service()
            .restore_object(Request::new(RestoreObjectRequest {
                bucket: "docs".into(),
                key: "readme.txt".into(),
                version_id: None,
                days: 2,
                tier: common::RestoreTier::Standard as i32,
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.status_code, 200);
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

    async fn connect_with_retry<T, F, Fut>(mut connector: F) -> T
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T, tonic::transport::Error>>,
    {
        for _ in 0..20 {
            match connector().await {
                Ok(client) => return client,
                Err(_) => sleep(Duration::from_millis(25)).await,
            }
        }
        panic!("failed to connect test client after retries");
    }

    async fn metadata_backed_service() -> (
        SchedulerServiceImpl,
        Arc<SchedulerState>,
        oneshot::Sender<()>,
    ) {
        let metadata = MetadataServiceImpl::new(&MetadataConfig::default())
            .await
            .expect("metadata service init");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind test listener");
        let addr = listener.local_addr().expect("listener addr");
        drop(listener);

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        tokio::spawn(async move {
            Server::builder()
                .add_service(MetadataServiceServer::new(metadata))
                .serve_with_shutdown(addr, async {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("metadata server should run");
        });

        let metadata = connect_with_retry(|| {
            coldstore_proto::metadata::metadata_service_client::MetadataServiceClient::connect(
                format!("http://{addr}"),
            )
        })
        .await;
        let state = Arc::new(SchedulerState {
            metadata,
            cache: None,
            tape: None,
            config: SchedulerConfig::default(),
        });
        (SchedulerServiceImpl::new(state.clone()), state, shutdown_tx)
    }

    async fn metadata_and_cache_backed_service() -> (
        Arc<SchedulerState>,
        MetadataBackedSchedulerBackend,
        CacheServiceClient<tonic::transport::Channel>,
        oneshot::Sender<()>,
        oneshot::Sender<()>,
    ) {
        let metadata = MetadataServiceImpl::new(&MetadataConfig::default())
            .await
            .expect("metadata service init");
        let metadata_listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind metadata listener");
        let metadata_addr = metadata_listener.local_addr().expect("metadata addr");
        drop(metadata_listener);

        let cache_dir =
            std::env::temp_dir().join(format!("coldstore-cache-test-{}", uuid::Uuid::new_v4()));
        let cache_service = CacheServiceImpl::new(&CacheConfig {
            listen: "127.0.0.1:0".into(),
            metadata_addrs: vec![],
            backend: CacheBackendConfig::Hdd {
                path: cache_dir.to_string_lossy().into_owned(),
                max_size_gb: 1,
            },
            ..CacheConfig::default()
        })
        .await
        .expect("cache service init");
        let cache_listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind cache listener");
        let cache_addr = cache_listener.local_addr().expect("cache addr");
        drop(cache_listener);

        let (metadata_shutdown_tx, metadata_shutdown_rx) = oneshot::channel();
        tokio::spawn(async move {
            Server::builder()
                .add_service(MetadataServiceServer::new(metadata))
                .serve_with_shutdown(metadata_addr, async {
                    let _ = metadata_shutdown_rx.await;
                })
                .await
                .expect("metadata server should run");
        });

        let (cache_shutdown_tx, cache_shutdown_rx) = oneshot::channel();
        tokio::spawn(async move {
            Server::builder()
                .add_service(CacheServiceServer::new(cache_service))
                .serve_with_shutdown(cache_addr, async {
                    let _ = cache_shutdown_rx.await;
                })
                .await
                .expect("cache server should run");
        });

        let metadata = connect_with_retry(|| {
            coldstore_proto::metadata::metadata_service_client::MetadataServiceClient::connect(
                format!("http://{metadata_addr}"),
            )
        })
        .await;
        let cache =
            connect_with_retry(|| CacheServiceClient::connect(format!("http://{cache_addr}")))
                .await;

        let state = Arc::new(SchedulerState {
            metadata,
            cache: Some(cache.clone()),
            tape: None,
            config: SchedulerConfig::default(),
        });
        let backend =
            MetadataBackedSchedulerBackend::new(state.metadata.clone(), state.cache.clone());
        (
            state,
            backend,
            cache,
            metadata_shutdown_tx,
            cache_shutdown_tx,
        )
    }

    #[tokio::test]
    async fn default_service_uses_metadata_for_bucket_ops() {
        let (svc, _state, shutdown_tx) = metadata_backed_service().await;

        svc.create_bucket(Request::new(CreateBucketRequest {
            bucket: "phase1-bucket".into(),
        }))
        .await
        .expect("create bucket through metadata-backed scheduler");

        let buckets = svc
            .list_buckets(Request::new(()))
            .await
            .expect("list buckets through metadata-backed scheduler")
            .into_inner();
        assert!(buckets
            .buckets
            .iter()
            .any(|bucket| bucket.name == "phase1-bucket"));

        shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn default_service_uses_metadata_for_object_metadata_ops() {
        let (state, backend, mut cache, metadata_shutdown_tx, cache_shutdown_tx) =
            metadata_and_cache_backed_service().await;
        let svc = SchedulerServiceImpl::new(state.clone());

        svc.create_bucket(Request::new(CreateBucketRequest {
            bucket: "docs".into(),
        }))
        .await
        .expect("create bucket");

        let mut metadata = state.metadata.clone();
        metadata
            .put_object(Request::new(common::ObjectMetadata {
                bucket: "docs".into(),
                key: "guide.txt".into(),
                version_id: None,
                size: 5,
                checksum: "seed-checksum".into(),
                content_type: Some("text/plain".into()),
                etag: Some("seed-etag".into()),
                storage_class: common::StorageClass::ColdPending as i32,
                archive_id: None,
                tape_id: None,
                tape_set: vec![],
                tape_block_offset: None,
                restore_status: None,
                restore_expire_at: None,
                created_at: Some(Timestamp {
                    seconds: 10,
                    nanos: 0,
                }),
                updated_at: Some(Timestamp {
                    seconds: 10,
                    nanos: 0,
                }),
            }))
            .await
            .expect("seed object in metadata");

        let head = svc
            .head_object(Request::new(HeadObjectRequest {
                bucket: "docs".into(),
                key: "guide.txt".into(),
                version_id: None,
            }))
            .await
            .expect("head object")
            .into_inner();
        assert_eq!(head.content_length, 5);
        assert_eq!(head.storage_class, common::StorageClass::ColdPending as i32);

        let list = svc
            .list_objects(Request::new(ListObjectsRequest {
                bucket: "docs".into(),
                prefix: Some("gui".into()),
                marker: None,
                delimiter: None,
                max_keys: 10,
            }))
            .await
            .expect("list objects")
            .into_inner();
        assert_eq!(list.contents.len(), 1);
        assert_eq!(list.contents[0].key, "guide.txt");

        let mut metadata = state.metadata.clone();
        metadata
            .update_storage_class(Request::new(
                coldstore_proto::metadata::UpdateStorageClassRequest {
                    bucket: "docs".into(),
                    key: "guide.txt".into(),
                    storage_class: common::StorageClass::Cold as i32,
                },
            ))
            .await
            .expect("mark object as cold");

        cache
            .put_staging(Request::new(tokio_stream::iter(vec![
                PutStagingRequest {
                    payload: Some(put_staging_request::Payload::Meta(PutStagingMeta {
                        bucket: "docs".into(),
                        key: "guide.txt".into(),
                        version_id: None,
                        size: 5,
                        checksum: Some("seed-checksum".into()),
                        content_type: Some("text/plain".into()),
                        etag: Some("seed-etag".into()),
                    })),
                },
                PutStagingRequest {
                    payload: Some(put_staging_request::Payload::Data(b"hello".to_vec())),
                },
            ])))
            .await
            .expect("seed staging cache object");

        let restore = svc
            .restore_object(Request::new(RestoreObjectRequest {
                bucket: "docs".into(),
                key: "guide.txt".into(),
                version_id: None,
                days: 3,
                tier: common::RestoreTier::Standard as i32,
            }))
            .await
            .expect("restore object")
            .into_inner();
        assert_eq!(restore.status_code, 202);

        let restored = svc
            .head_object(Request::new(HeadObjectRequest {
                bucket: "docs".into(),
                key: "guide.txt".into(),
                version_id: None,
            }))
            .await
            .expect("head restored object")
            .into_inner();
        assert!(restored
            .restore_info
            .as_deref()
            .is_some_and(|info| info.contains("ongoing-request=\"false\"")));

        let get = backend
            .get_object("docs", "guide.txt")
            .await
            .expect("backend get after restore");
        assert_eq!(get.1, b"hello");

        svc.delete_object(Request::new(DeleteObjectRequest {
            bucket: "docs".into(),
            key: "guide.txt".into(),
            version_id: None,
        }))
        .await
        .expect("delete object through metadata-backed scheduler");

        assert!(svc
            .head_object(Request::new(HeadObjectRequest {
                bucket: "docs".into(),
                key: "guide.txt".into(),
                version_id: None,
            }))
            .await
            .is_err());

        metadata_shutdown_tx.send(()).ok();
        cache_shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn default_metadata_backend_reads_restored_object_from_cache() {
        let (state, backend, mut cache, metadata_shutdown_tx, cache_shutdown_tx) =
            metadata_and_cache_backed_service().await;

        backend
            .create_bucket("docs")
            .await
            .expect("create bucket through metadata backend");

        let expire_at = days_from_now(2);
        state
            .metadata
            .clone()
            .put_object(Request::new(common::ObjectMetadata {
                bucket: "docs".into(),
                key: "guide.txt".into(),
                version_id: None,
                size: 5,
                checksum: "seed-checksum".into(),
                content_type: Some("text/plain".into()),
                etag: Some("seed-etag".into()),
                storage_class: common::StorageClass::Cold as i32,
                archive_id: Some("archive-1".into()),
                tape_id: Some("tape-1".into()),
                tape_set: vec!["tape-1".into()],
                tape_block_offset: Some(7),
                restore_status: Some(common::RestoreStatus::RestoreCompleted as i32),
                restore_expire_at: Some(expire_at),
                created_at: Some(Timestamp {
                    seconds: 10,
                    nanos: 0,
                }),
                updated_at: Some(Timestamp {
                    seconds: 11,
                    nanos: 0,
                }),
            }))
            .await
            .expect("seed object metadata");

        cache
            .put_restored(Request::new(tokio_stream::iter(vec![
                PutRestoredRequest {
                    payload: Some(put_restored_request::Payload::Meta(PutRestoredMeta {
                        bucket: "docs".into(),
                        key: "guide.txt".into(),
                        version_id: None,
                        size: 5,
                        checksum: Some("seed-checksum".into()),
                        content_type: Some("text/plain".into()),
                        etag: Some("seed-etag".into()),
                        expire_at: Some(days_from_now(2)),
                    })),
                },
                PutRestoredRequest {
                    payload: Some(put_restored_request::Payload::Data(b"hello".to_vec())),
                },
            ])))
            .await
            .expect("seed restored cache object");

        let (object, body) = backend
            .get_object("docs", "guide.txt")
            .await
            .expect("get object should read through cache");
        assert_eq!(object.etag.as_deref(), Some("seed-etag"));
        assert_eq!(body, b"hello");

        metadata_shutdown_tx.send(()).ok();
        cache_shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn default_metadata_backend_restore_populates_cache_and_enables_get() {
        let (state, backend, mut cache, metadata_shutdown_tx, cache_shutdown_tx) =
            metadata_and_cache_backed_service().await;

        backend
            .create_bucket("docs")
            .await
            .expect("create bucket through metadata backend");

        state
            .metadata
            .clone()
            .put_object(Request::new(common::ObjectMetadata {
                bucket: "docs".into(),
                key: "guide.txt".into(),
                version_id: None,
                size: 5,
                checksum: "seed-checksum".into(),
                content_type: Some("text/plain".into()),
                etag: Some("seed-etag".into()),
                storage_class: common::StorageClass::Cold as i32,
                archive_id: Some("archive-1".into()),
                tape_id: Some("tape-1".into()),
                tape_set: vec!["tape-1".into()],
                tape_block_offset: Some(7),
                restore_status: None,
                restore_expire_at: None,
                created_at: Some(Timestamp {
                    seconds: 10,
                    nanos: 0,
                }),
                updated_at: Some(Timestamp {
                    seconds: 11,
                    nanos: 0,
                }),
            }))
            .await
            .expect("seed cold object metadata");

        cache
            .put_staging(Request::new(tokio_stream::iter(vec![
                PutStagingRequest {
                    payload: Some(put_staging_request::Payload::Meta(PutStagingMeta {
                        bucket: "docs".into(),
                        key: "guide.txt".into(),
                        version_id: None,
                        size: 5,
                        checksum: Some("seed-checksum".into()),
                        content_type: Some("text/plain".into()),
                        etag: Some("seed-etag".into()),
                    })),
                },
                PutStagingRequest {
                    payload: Some(put_staging_request::Payload::Data(b"hello".to_vec())),
                },
            ])))
            .await
            .expect("seed staging cache object");

        let response = backend
            .restore_object("docs", "guide.txt", 3, common::RestoreTier::Standard)
            .await
            .expect("restore object should populate cache");
        assert_eq!(response.status_code, 202);

        let contains = cache
            .contains(Request::new(ContainsRequest {
                bucket: "docs".into(),
                key: "guide.txt".into(),
                version_id: None,
            }))
            .await
            .expect("contains should succeed after restore")
            .into_inner();
        assert!(contains.exists);

        let (object, body) = backend
            .get_object("docs", "guide.txt")
            .await
            .expect("restored object should become readable through cache");
        assert_eq!(
            object.restore_status,
            Some(common::RestoreStatus::RestoreCompleted as i32)
        );
        assert_eq!(body, b"hello");

        let mut stream = cache
            .get(Request::new(CacheGetRequest {
                bucket: "docs".into(),
                key: "guide.txt".into(),
                version_id: None,
            }))
            .await
            .expect("cache get should succeed after restore")
            .into_inner();
        let first = stream.next().await.unwrap().unwrap();
        match first.payload {
            Some(coldstore_proto::cache::get_response::Payload::Meta(meta)) => {
                assert_eq!(meta.etag.as_deref(), Some("seed-etag"));
            }
            _ => panic!("expected restored cache metadata"),
        }

        metadata_shutdown_tx.send(()).ok();
        cache_shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn metadata_backed_archive_marks_cold_and_clears_staging() {
        let (state, backend, mut cache, metadata_shutdown_tx, cache_shutdown_tx) =
            metadata_and_cache_backed_service().await;

        backend
            .create_bucket("docs")
            .await
            .expect("create bucket through metadata backend");

        backend
            .put_object(
                "docs",
                "guide.txt",
                b"hello".to_vec(),
                Some("text/plain".into()),
            )
            .await
            .expect("put object through metadata backend");

        let archived = backend
            .archive_staging_batch(10)
            .await
            .expect("archive staging batch should succeed");
        assert_eq!(archived, vec!["docs/guide.txt"]);

        let object = state
            .metadata
            .clone()
            .get_object(Request::new(coldstore_proto::metadata::GetObjectRequest {
                bucket: "docs".into(),
                key: "guide.txt".into(),
            }))
            .await
            .expect("get archived object metadata")
            .into_inner();
        assert_eq!(object.storage_class, common::StorageClass::Cold as i32);
        assert_eq!(
            object.archive_id.as_deref(),
            Some("phase1-archive:docs/guide.txt")
        );
        assert_eq!(object.tape_id.as_deref(), Some("phase1-cache-archive"));

        let staging = cache
            .list_staging_keys(Request::new(
                coldstore_proto::cache::ListStagingKeysRequest {
                    limit: 100,
                    after: None,
                },
            ))
            .await
            .expect("list staging keys")
            .into_inner();
        assert!(staging.entries.is_empty());

        metadata_shutdown_tx.send(()).ok();
        cache_shutdown_tx.send(()).ok();
    }

    #[test]
    fn phase1_unimplemented_message_is_stable() {
        let status = phase1_unimplemented("scheduler.list_buckets");
        assert_eq!(status.code(), tonic::Code::Unimplemented);
        assert!(status.message().contains("phase-1 safe mode"));
    }
}
