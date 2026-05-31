use crate::SchedulerState;
use coldstore_proto::cache::cache_service_client::CacheServiceClient;
use coldstore_proto::cache::get_staging_response::Payload as GetStagingPayload;
use coldstore_proto::cache::{
    DeleteStagingRequest, GetStagingRequest, ListStagingKeysRequest, StagingKeyEntry,
    StagingObjectMeta,
};
use coldstore_proto::common;
use coldstore_proto::scheduler::scheduler_service_server::SchedulerService;
use coldstore_proto::scheduler::*;
use coldstore_proto::tape::tape_service_client::TapeServiceClient as TapeGrpcClient;
use coldstore_proto::tape::write_bundle_request::Payload as TapeWriteRequestPayload;
use coldstore_proto::tape::{
    WriteBundleMeta as TapeWriteBundleMeta, WriteBundleRequest as TapeWriteBundleRequest,
};
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
    async fn list_objects(
        &self,
        bucket: &str,
        prefix: Option<&str>,
        marker: Option<&str>,
        max_keys: u32,
    ) -> std::result::Result<Vec<common::ObjectMetadata>, Status>;
}

#[derive(Debug, Clone)]
pub struct Phase1StagedObject {
    pub meta: StagingObjectMeta,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TapeArchiveWrite {
    pub tape_id: String,
    pub tape_set: Vec<String>,
    pub filemark_start: u32,
    pub filemark_end: u32,
    pub bytes_written: u64,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct ArchiveBatchResult {
    pub archived_objects: u32,
    pub bytes_written: u64,
    pub bundle_ids: Vec<String>,
}

#[tonic::async_trait]
pub trait Phase1ArchiveCache: Send + Sync {
    async fn list_staging_keys(
        &self,
        limit: u32,
    ) -> std::result::Result<Vec<StagingKeyEntry>, Status>;

    async fn get_staging(
        &self,
        bucket: &str,
        key: &str,
        version_id: Option<&str>,
    ) -> std::result::Result<Phase1StagedObject, Status>;

    async fn delete_staging(
        &self,
        bucket: &str,
        key: &str,
        version_id: Option<&str>,
    ) -> std::result::Result<(), Status>;
}

#[derive(Clone)]
pub struct CacheArchiveClient {
    client: CacheServiceClient<tonic::transport::Channel>,
}

impl CacheArchiveClient {
    pub fn new(client: CacheServiceClient<tonic::transport::Channel>) -> Self {
        Self { client }
    }
}

#[tonic::async_trait]
impl Phase1ArchiveCache for CacheArchiveClient {
    async fn list_staging_keys(
        &self,
        limit: u32,
    ) -> std::result::Result<Vec<StagingKeyEntry>, Status> {
        let mut client = self.client.clone();
        Ok(client
            .list_staging_keys(Request::new(ListStagingKeysRequest { limit, after: None }))
            .await?
            .into_inner()
            .entries)
    }

    async fn get_staging(
        &self,
        bucket: &str,
        key: &str,
        version_id: Option<&str>,
    ) -> std::result::Result<Phase1StagedObject, Status> {
        let mut client = self.client.clone();
        let mut stream = client
            .get_staging(Request::new(GetStagingRequest {
                bucket: bucket.into(),
                key: key.into(),
                version_id: version_id.map(str::to_owned),
            }))
            .await?
            .into_inner();

        let mut meta = None;
        let mut data = Vec::new();
        while let Some(message) = stream.message().await? {
            match message.payload {
                Some(GetStagingPayload::Meta(next_meta)) => {
                    if meta.replace(next_meta).is_some() {
                        return Err(Status::invalid_argument(
                            "cache get_staging returned duplicate metadata",
                        ));
                    }
                }
                Some(GetStagingPayload::Data(chunk)) => data.extend_from_slice(&chunk),
                None => return Err(Status::internal("cache get_staging returned empty chunk")),
            }
        }

        let meta = meta
            .ok_or_else(|| Status::internal("cache get_staging stream ended without metadata"))?;
        Ok(Phase1StagedObject { meta, data })
    }

    async fn delete_staging(
        &self,
        bucket: &str,
        key: &str,
        version_id: Option<&str>,
    ) -> std::result::Result<(), Status> {
        let mut client = self.client.clone();
        client
            .delete_staging(Request::new(DeleteStagingRequest {
                bucket: bucket.into(),
                key: key.into(),
                version_id: version_id.map(str::to_owned),
            }))
            .await?;
        Ok(())
    }
}

#[tonic::async_trait]
pub trait TapeArchiveWriter: Send + Sync {
    async fn write_bundle(
        &self,
        bundle_id: &str,
        object_count: u32,
        data: Vec<u8>,
    ) -> std::result::Result<TapeArchiveWrite, Status>;
}

#[derive(Clone)]
pub struct TapeArchiveClient {
    client: TapeGrpcClient<tonic::transport::Channel>,
    drive_id: String,
    tape_id: String,
    tape_set: Vec<String>,
    block_size: u32,
}

impl TapeArchiveClient {
    pub fn new(
        client: TapeGrpcClient<tonic::transport::Channel>,
        drive_id: impl Into<String>,
        tape_id: impl Into<String>,
        tape_set: Vec<String>,
        block_size: u32,
    ) -> Self {
        Self {
            client,
            drive_id: drive_id.into(),
            tape_id: tape_id.into(),
            tape_set,
            block_size,
        }
    }
}

#[tonic::async_trait]
impl TapeArchiveWriter for TapeArchiveClient {
    async fn write_bundle(
        &self,
        bundle_id: &str,
        object_count: u32,
        data: Vec<u8>,
    ) -> std::result::Result<TapeArchiveWrite, Status> {
        let mut client = self.client.clone();
        let response = client
            .write_bundle(Request::new(tokio_stream::iter(vec![
                TapeWriteBundleRequest {
                    payload: Some(TapeWriteRequestPayload::Meta(TapeWriteBundleMeta {
                        drive_id: self.drive_id.clone(),
                        bundle_id: bundle_id.into(),
                        total_size: data.len() as u64,
                        object_count,
                        block_size: self.block_size,
                    })),
                },
                TapeWriteBundleRequest {
                    payload: Some(TapeWriteRequestPayload::Data(data)),
                },
            ])))
            .await?
            .into_inner();

        if !response.success {
            return Err(Status::internal(format!(
                "tape write_bundle failed for {bundle_id}: {}",
                response
                    .error
                    .unwrap_or_else(|| "unknown tape error".into())
            )));
        }
        if response.drive_id != self.drive_id {
            return Err(Status::internal(format!(
                "tape write_bundle responded for drive {}, expected {}",
                response.drive_id, self.drive_id
            )));
        }
        if response.bundle_id != bundle_id {
            return Err(Status::internal(format!(
                "tape write_bundle responded for bundle {}, expected {bundle_id}",
                response.bundle_id
            )));
        }

        Ok(TapeArchiveWrite {
            tape_id: self.tape_id.clone(),
            tape_set: self.tape_set.clone(),
            filemark_start: response.filemark_start,
            filemark_end: response.filemark_end,
            bytes_written: response.bytes_written,
        })
    }
}

pub struct MetadataBackedSchedulerBackend {
    metadata: coldstore_proto::metadata::metadata_service_client::MetadataServiceClient<
        tonic::transport::Channel,
    >,
}

impl MetadataBackedSchedulerBackend {
    pub fn new(
        metadata: coldstore_proto::metadata::metadata_service_client::MetadataServiceClient<
            tonic::transport::Channel,
        >,
    ) -> Self {
        Self { metadata }
    }

    pub async fn archive_staging_batch<C, T>(
        &self,
        cache: &C,
        tape: &T,
        limit: u32,
    ) -> std::result::Result<ArchiveBatchResult, Status>
    where
        C: Phase1ArchiveCache + ?Sized,
        T: TapeArchiveWriter + ?Sized,
    {
        let entries = cache.list_staging_keys(limit).await?;
        let mut result = ArchiveBatchResult::default();

        for entry in entries {
            let object = self.head_object(&entry.bucket, &entry.key).await?;
            if object.storage_class != common::StorageClass::ColdPending as i32 {
                continue;
            }

            let staged = cache
                .get_staging(&entry.bucket, &entry.key, entry.version_id.as_deref())
                .await?;
            if staged.meta.size != staged.data.len() as u64 {
                return Err(Status::invalid_argument(format!(
                    "staging size mismatch for {}/{}: meta={}, bytes={}",
                    entry.bucket,
                    entry.key,
                    staged.meta.size,
                    staged.data.len()
                )));
            }

            let bundle_id =
                phase1_bundle_id(&entry.bucket, &entry.key, entry.version_id.as_deref());
            let checksum = staged
                .meta
                .checksum
                .clone()
                .unwrap_or_else(|| sha256_hex(&staged.data));
            let write = tape
                .write_bundle(&bundle_id, 1, staged.data.clone())
                .await?;
            if write.bytes_written != staged.data.len() as u64 {
                return Err(Status::internal(format!(
                    "tape writer reported {} bytes for {bundle_id}, expected {}",
                    write.bytes_written,
                    staged.data.len()
                )));
            }

            let now = now_timestamp();
            let bundle = common::ArchiveBundle {
                id: bundle_id.clone(),
                tape_id: write.tape_id.clone(),
                tape_set: write.tape_set.clone(),
                entries: vec![common::BundleEntry {
                    bucket: entry.bucket.clone(),
                    key: entry.key.clone(),
                    version_id: entry.version_id.clone(),
                    size: staged.data.len() as u64,
                    offset_in_bundle: 0,
                    tape_block_offset: write.filemark_start as u64,
                    checksum: checksum.clone(),
                }],
                total_size: write.bytes_written,
                filemark_start: write.filemark_start,
                filemark_end: write.filemark_end,
                checksum: Some(checksum),
                status: common::ArchiveBundleStatus::BundleCompleted as i32,
                created_at: Some(now),
                completed_at: Some(now),
            };

            let mut client = self.metadata.clone();
            client.put_archive_bundle(Request::new(bundle)).await?;
            client
                .update_archive_location(Request::new(
                    coldstore_proto::metadata::UpdateArchiveLocationRequest {
                        bucket: entry.bucket.clone(),
                        key: entry.key.clone(),
                        archive_id: bundle_id.clone(),
                        tape_id: write.tape_id.clone(),
                        tape_set: write.tape_set.clone(),
                        tape_block_offset: write.filemark_start as u64,
                    },
                ))
                .await?;
            client
                .update_storage_class(Request::new(
                    coldstore_proto::metadata::UpdateStorageClassRequest {
                        bucket: entry.bucket.clone(),
                        key: entry.key.clone(),
                        storage_class: common::StorageClass::Cold as i32,
                    },
                ))
                .await?;
            cache
                .delete_staging(&entry.bucket, &entry.key, entry.version_id.as_deref())
                .await?;

            result.archived_objects += 1;
            result.bytes_written += write.bytes_written;
            result.bundle_ids.push(bundle_id);
        }

        Ok(result)
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
        Err(Status::failed_precondition(format!(
            "scheduler.get_object requires phase-1 cache wiring; metadata is available for {}/{} but body retrieval is not yet connected",
            object.bucket, object.key
        )))
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
            content_type,
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

        let restore_status = object
            .restore_status
            .and_then(|status| common::RestoreStatus::try_from(status).ok());

        match restore_status {
            Some(common::RestoreStatus::RestoreCompleted) => {
                Ok(RestoreObjectResponse { status_code: 200 })
            }
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
            Some(common::RestoreStatus::Unspecified) | None => {
                client
                    .update_restore_status(Request::new(
                        coldstore_proto::metadata::UpdateRestoreStatusRequest {
                            bucket: bucket.into(),
                            key: key.into(),
                            status: common::RestoreStatus::RestorePending as i32,
                            expire_at: Some(days_from_now(days.max(1))),
                        },
                    ))
                    .await?;
                Ok(RestoreObjectResponse { status_code: 202 })
            }
        }
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
        let backend = Arc::new(MetadataBackedSchedulerBackend::new(state.metadata.clone()));
        Self {
            _state: state,
            backend,
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

fn phase1_bundle_id(bucket: &str, key: &str, version_id: Option<&str>) -> String {
    match version_id.filter(|version| !version.is_empty()) {
        Some(version) => format!("phase1-bundle:{bucket}/{key}#{version}"),
        None => format!("phase1-bundle:{bucket}/{key}"),
    }
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
        CacheBackendConfig, CacheConfig, MetadataConfig, SchedulerConfig, TapeConfig,
    };
    use coldstore_metadata::service::MetadataServiceImpl;
    use coldstore_proto::cache::cache_service_client::CacheServiceClient;
    use coldstore_proto::cache::cache_service_server::CacheServiceServer;
    use coldstore_proto::cache::put_staging_request::Payload as PutStagingPayload;
    use coldstore_proto::cache::{
        GetStagingRequest, ListStagingKeysRequest, PutStagingMeta, PutStagingRequest,
        StagingKeyEntry, StagingObjectMeta,
    };
    use coldstore_proto::metadata::metadata_service_server::MetadataServiceServer;
    use coldstore_proto::tape::read_bundle_request::Location as TapeReadLocation;
    use coldstore_proto::tape::read_bundle_response::Payload as TapeReadPayload;
    use coldstore_proto::tape::tape_service_client::TapeServiceClient;
    use coldstore_proto::tape::tape_service_server::{
        TapeService as TapeWorkerService, TapeServiceServer,
    };
    use coldstore_proto::tape::write_bundle_request::Payload as TapeWritePayload;
    use coldstore_proto::tape::{
        LoadTapeRequest, ReadBundleRequest as TapeReadBundleRequest, WriteBundleMeta,
        WriteBundleRequest,
    };
    use coldstore_tape::service::{SimulatorTapeBackend, TapeServiceImpl as TapeWorkerServiceImpl};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::sync::oneshot;
    use tokio::time::{sleep, Duration};
    use tokio_stream::StreamExt;
    use tonic::transport::{Channel, Server};

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

        let mut metadata_client = None;
        for _ in 0..20 {
            match coldstore_proto::metadata::metadata_service_client::MetadataServiceClient::connect(
                format!("http://{addr}"),
            )
            .await
            {
                Ok(client) => {
                    metadata_client = Some(client);
                    break;
                }
                Err(_) => sleep(Duration::from_millis(25)).await,
            }
        }
        let metadata = metadata_client.expect("connect metadata client");
        let state = Arc::new(SchedulerState {
            metadata,
            cache: None,
            tape: None,
            config: SchedulerConfig::default(),
        });
        (SchedulerServiceImpl::new(state.clone()), state, shutdown_tx)
    }

    fn test_cache_config() -> CacheConfig {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        CacheConfig {
            backend: CacheBackendConfig::Hdd {
                path: format!("/tmp/coldstore-scheduler-cache-test-{unique}"),
                max_size_gb: 1,
            },
            ..CacheConfig::default()
        }
    }

    async fn cache_backed_service() -> (CacheServiceClient<Channel>, oneshot::Sender<()>) {
        let cache = CacheServiceImpl::new(&test_cache_config())
            .await
            .expect("cache service init");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind test listener");
        let addr = listener.local_addr().expect("listener addr");
        drop(listener);

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        tokio::spawn(async move {
            Server::builder()
                .add_service(CacheServiceServer::new(cache))
                .serve_with_shutdown(addr, async {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("cache server should run");
        });

        let mut cache_client = None;
        for _ in 0..20 {
            match CacheServiceClient::connect(format!("http://{addr}")).await {
                Ok(client) => {
                    cache_client = Some(client);
                    break;
                }
                Err(_) => sleep(Duration::from_millis(25)).await,
            }
        }
        (cache_client.expect("connect cache client"), shutdown_tx)
    }

    async fn tape_backed_service() -> (TapeServiceClient<Channel>, oneshot::Sender<()>) {
        let backend = SimulatorTapeBackend::new(2, 1);
        backend.insert_tape("slot-1", "TAPE-GRPC").unwrap();
        let tape = TapeWorkerServiceImpl::new_with_backend(TapeConfig::default(), backend);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind test listener");
        let addr = listener.local_addr().expect("listener addr");
        drop(listener);

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        tokio::spawn(async move {
            Server::builder()
                .add_service(TapeServiceServer::new(tape))
                .serve_with_shutdown(addr, async {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("tape server should run");
        });

        let mut tape_client = None;
        for _ in 0..20 {
            match TapeServiceClient::connect(format!("http://{addr}")).await {
                Ok(client) => {
                    tape_client = Some(client);
                    break;
                }
                Err(_) => sleep(Duration::from_millis(25)).await,
            }
        }
        let mut tape_client = tape_client.expect("connect tape client");
        tape_client
            .load_tape(Request::new(LoadTapeRequest {
                tape_id: "TAPE-GRPC".into(),
                drive_id: "drive-0".into(),
                slot_id: Some("slot-1".into()),
            }))
            .await
            .expect("load tape into in-process simulator");
        (tape_client, shutdown_tx)
    }

    async fn read_tape_client_filemark(
        client: &mut TapeServiceClient<Channel>,
        filemark: u32,
        length: u64,
    ) -> Vec<u8> {
        let mut stream = client
            .read_bundle(Request::new(TapeReadBundleRequest {
                drive_id: "drive-0".into(),
                location: Some(TapeReadLocation::Filemark(filemark)),
                length,
            }))
            .await
            .expect("read bundle through tape client")
            .into_inner();
        while let Some(message) = stream.next().await {
            match message.unwrap().payload.unwrap() {
                TapeReadPayload::Meta(_) => {}
                TapeReadPayload::Data(data) => return data,
            }
        }
        Vec::new()
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
        let (svc, state, shutdown_tx) = metadata_backed_service().await;

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
        assert_eq!(
            restored.restore_info.as_deref(),
            Some("ongoing-request=\"true\"")
        );

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

        shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn default_metadata_backend_puts_object_and_reports_cache_gap() {
        let (_svc, state, shutdown_tx) = metadata_backed_service().await;
        let backend = MetadataBackedSchedulerBackend::new(state.metadata.clone());

        backend
            .create_bucket("docs")
            .await
            .expect("create bucket through metadata backend");

        let put = backend
            .put_object(
                "docs",
                "guide.txt",
                b"hello".to_vec(),
                Some("text/plain".into()),
            )
            .await
            .expect("put object through metadata backend");
        assert!(!put.etag.is_empty());

        let listed = backend
            .list_objects("docs", Some("gui"), None, 10)
            .await
            .expect("list objects through metadata backend");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].etag.as_deref(), Some(put.etag.as_str()));

        let err = backend
            .get_object("docs", "guide.txt")
            .await
            .expect_err("get_object should explain that cache is still not wired");
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
        assert!(err.message().contains("cache wiring"));

        shutdown_tx.send(()).ok();
    }

    struct TestArchiveCache {
        staged: RwLock<HashMap<String, Phase1StagedObject>>,
        deleted: RwLock<Vec<String>>,
    }

    impl TestArchiveCache {
        fn with_object(bucket: &str, key: &str, data: Vec<u8>) -> Self {
            let meta = StagingObjectMeta {
                bucket: bucket.into(),
                key: key.into(),
                version_id: None,
                size: data.len() as u64,
                checksum: Some(sha256_hex(&data)),
                content_type: Some("text/plain".into()),
                etag: Some("etag-staged".into()),
                staged_at: Some(Timestamp {
                    seconds: 30,
                    nanos: 0,
                }),
            };
            let mut staged = HashMap::new();
            staged.insert(format!("{bucket}/{key}"), Phase1StagedObject { meta, data });
            Self {
                staged: RwLock::new(staged),
                deleted: RwLock::new(Vec::new()),
            }
        }

        fn deleted_keys(&self) -> Vec<String> {
            self.deleted.read().unwrap().clone()
        }
    }

    #[tonic::async_trait]
    impl Phase1ArchiveCache for TestArchiveCache {
        async fn list_staging_keys(
            &self,
            limit: u32,
        ) -> std::result::Result<Vec<StagingKeyEntry>, Status> {
            let mut entries: Vec<_> = self
                .staged
                .read()
                .unwrap()
                .values()
                .map(|object| StagingKeyEntry {
                    bucket: object.meta.bucket.clone(),
                    key: object.meta.key.clone(),
                    version_id: object.meta.version_id.clone(),
                    size: object.meta.size,
                    staged_at: object.meta.staged_at,
                })
                .collect();
            entries.sort_by(|a, b| (&a.bucket, &a.key).cmp(&(&b.bucket, &b.key)));
            entries.truncate(limit as usize);
            Ok(entries)
        }

        async fn get_staging(
            &self,
            bucket: &str,
            key: &str,
            _version_id: Option<&str>,
        ) -> std::result::Result<Phase1StagedObject, Status> {
            self.staged
                .read()
                .unwrap()
                .get(&format!("{bucket}/{key}"))
                .cloned()
                .ok_or_else(|| Status::not_found("staging object missing"))
        }

        async fn delete_staging(
            &self,
            bucket: &str,
            key: &str,
            _version_id: Option<&str>,
        ) -> std::result::Result<(), Status> {
            self.staged
                .write()
                .unwrap()
                .remove(&format!("{bucket}/{key}"));
            self.deleted
                .write()
                .unwrap()
                .push(format!("{bucket}/{key}"));
            Ok(())
        }
    }

    struct DirectTapeWriter {
        service: TapeWorkerServiceImpl,
    }

    impl DirectTapeWriter {
        async fn loaded() -> Self {
            let backend = SimulatorTapeBackend::new(2, 1);
            backend.insert_tape("slot-1", "TAPE-PHASE1").unwrap();
            let service = TapeWorkerServiceImpl::new_with_backend(TapeConfig::default(), backend);
            service
                .load_tape(Request::new(LoadTapeRequest {
                    tape_id: "TAPE-PHASE1".into(),
                    drive_id: "drive-0".into(),
                    slot_id: Some("slot-1".into()),
                }))
                .await
                .unwrap();
            Self { service }
        }

        async fn read_filemark(&self, filemark: u32, length: u64) -> Vec<u8> {
            let mut stream = self
                .service
                .read_bundle(Request::new(TapeReadBundleRequest {
                    drive_id: "drive-0".into(),
                    location: Some(TapeReadLocation::Filemark(filemark)),
                    length,
                }))
                .await
                .unwrap()
                .into_inner();
            while let Some(message) = stream.next().await {
                match message.unwrap().payload.unwrap() {
                    TapeReadPayload::Meta(_) => {}
                    TapeReadPayload::Data(data) => return data,
                }
            }
            Vec::new()
        }
    }

    #[tonic::async_trait]
    impl TapeArchiveWriter for DirectTapeWriter {
        async fn write_bundle(
            &self,
            bundle_id: &str,
            object_count: u32,
            data: Vec<u8>,
        ) -> std::result::Result<TapeArchiveWrite, Status> {
            let response = self
                .service
                .write_bundle_from_messages(tokio_stream::iter(vec![
                    Ok(WriteBundleRequest {
                        payload: Some(TapeWritePayload::Meta(WriteBundleMeta {
                            drive_id: "drive-0".into(),
                            bundle_id: bundle_id.into(),
                            total_size: data.len() as u64,
                            object_count,
                            block_size: 262_144,
                        })),
                    }),
                    Ok(WriteBundleRequest {
                        payload: Some(TapeWritePayload::Data(data)),
                    }),
                ]))
                .await?;
            Ok(TapeArchiveWrite {
                tape_id: "TAPE-PHASE1".into(),
                tape_set: vec!["TAPE-PHASE1".into()],
                filemark_start: response.filemark_start,
                filemark_end: response.filemark_end,
                bytes_written: response.bytes_written,
            })
        }
    }

    #[tokio::test]
    async fn archive_staging_batch_writes_tape_and_updates_metadata() {
        let (_svc, state, shutdown_tx) = metadata_backed_service().await;
        let backend = MetadataBackedSchedulerBackend::new(state.metadata.clone());

        backend
            .create_bucket("docs")
            .await
            .expect("create bucket through metadata backend");
        backend
            .put_object(
                "docs",
                "guide.txt",
                b"abcdef".to_vec(),
                Some("text/plain".into()),
            )
            .await
            .expect("put object metadata");

        let cache = TestArchiveCache::with_object("docs", "guide.txt", b"abcdef".to_vec());
        let tape = DirectTapeWriter::loaded().await;

        let archived = backend
            .archive_staging_batch(&cache, &tape, 10)
            .await
            .expect("archive staging batch");
        assert_eq!(archived.archived_objects, 1);
        assert_eq!(archived.bytes_written, 6);
        assert_eq!(archived.bundle_ids, vec!["phase1-bundle:docs/guide.txt"]);

        let object = backend
            .head_object("docs", "guide.txt")
            .await
            .expect("head archived object");
        assert_eq!(object.storage_class, common::StorageClass::Cold as i32);
        assert_eq!(
            object.archive_id.as_deref(),
            Some("phase1-bundle:docs/guide.txt")
        );
        assert_eq!(object.tape_id.as_deref(), Some("TAPE-PHASE1"));
        assert_eq!(object.tape_set, vec!["TAPE-PHASE1"]);
        assert_eq!(object.tape_block_offset, Some(0));

        let mut metadata = state.metadata.clone();
        let bundle = metadata
            .get_archive_bundle(Request::new(
                coldstore_proto::metadata::GetArchiveBundleRequest {
                    id: "phase1-bundle:docs/guide.txt".into(),
                },
            ))
            .await
            .expect("archive bundle stored")
            .into_inner();
        assert_eq!(bundle.tape_id, "TAPE-PHASE1");
        assert_eq!(bundle.filemark_start, 0);
        assert_eq!(bundle.filemark_end, 1);
        assert_eq!(bundle.total_size, 6);
        assert_eq!(bundle.entries.len(), 1);
        assert_eq!(bundle.entries[0].bucket, "docs");
        assert_eq!(bundle.entries[0].key, "guide.txt");
        assert_eq!(bundle.entries[0].tape_block_offset, 0);

        assert_eq!(cache.deleted_keys(), vec!["docs/guide.txt"]);
        assert_eq!(tape.read_filemark(0, 6).await, b"abcdef");

        shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn archive_staging_batch_consumes_real_cache_service_staging() {
        let (_svc, state, metadata_shutdown_tx) = metadata_backed_service().await;
        let backend = MetadataBackedSchedulerBackend::new(state.metadata.clone());

        backend
            .create_bucket("docs")
            .await
            .expect("create bucket through metadata backend");
        backend
            .put_object(
                "docs",
                "from-cache.txt",
                b"cache-body".to_vec(),
                Some("text/plain".into()),
            )
            .await
            .expect("put object metadata");

        let (mut raw_cache_client, cache_shutdown_tx) = cache_backed_service().await;
        let body = b"cache-body".to_vec();
        raw_cache_client
            .put_staging(Request::new(tokio_stream::iter(vec![
                PutStagingRequest {
                    payload: Some(PutStagingPayload::Meta(PutStagingMeta {
                        bucket: "docs".into(),
                        key: "from-cache.txt".into(),
                        version_id: None,
                        size: body.len() as u64,
                        checksum: Some(sha256_hex(&body)),
                        content_type: Some("text/plain".into()),
                        etag: Some("etag-from-cache".into()),
                    })),
                },
                PutStagingRequest {
                    payload: Some(PutStagingPayload::Data(body.clone())),
                },
            ])))
            .await
            .expect("put staging through real cache service");

        let cache = CacheArchiveClient::new(raw_cache_client.clone());
        let tape = DirectTapeWriter::loaded().await;

        let archived = backend
            .archive_staging_batch(&cache, &tape, 10)
            .await
            .expect("archive real cache staging batch");
        assert_eq!(archived.archived_objects, 1);
        assert_eq!(archived.bytes_written, body.len() as u64);
        assert_eq!(
            archived.bundle_ids,
            vec!["phase1-bundle:docs/from-cache.txt"]
        );

        let object = backend
            .head_object("docs", "from-cache.txt")
            .await
            .expect("head archived object");
        assert_eq!(object.storage_class, common::StorageClass::Cold as i32);
        assert_eq!(
            object.archive_id.as_deref(),
            Some("phase1-bundle:docs/from-cache.txt")
        );

        let listed = raw_cache_client
            .list_staging_keys(Request::new(ListStagingKeysRequest {
                limit: 10,
                after: None,
            }))
            .await
            .expect("list staging after archive")
            .into_inner();
        assert!(listed.entries.is_empty());

        let missing = raw_cache_client
            .get_staging(Request::new(GetStagingRequest {
                bucket: "docs".into(),
                key: "from-cache.txt".into(),
                version_id: None,
            }))
            .await
            .expect_err("staging should be deleted after archive");
        assert_eq!(missing.code(), tonic::Code::NotFound);
        assert_eq!(tape.read_filemark(0, body.len() as u64).await, body);

        metadata_shutdown_tx.send(()).ok();
        cache_shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn archive_staging_batch_uses_cache_and_tape_grpc_clients() {
        let (_svc, state, metadata_shutdown_tx) = metadata_backed_service().await;
        let backend = MetadataBackedSchedulerBackend::new(state.metadata.clone());

        backend
            .create_bucket("docs")
            .await
            .expect("create bucket through metadata backend");
        let body = b"grpc-tape-body".to_vec();
        backend
            .put_object(
                "docs",
                "grpc-tape.txt",
                body.clone(),
                Some("text/plain".into()),
            )
            .await
            .expect("put object metadata");

        let (mut raw_cache_client, cache_shutdown_tx) = cache_backed_service().await;
        raw_cache_client
            .put_staging(Request::new(tokio_stream::iter(vec![
                PutStagingRequest {
                    payload: Some(PutStagingPayload::Meta(PutStagingMeta {
                        bucket: "docs".into(),
                        key: "grpc-tape.txt".into(),
                        version_id: None,
                        size: body.len() as u64,
                        checksum: Some(sha256_hex(&body)),
                        content_type: Some("text/plain".into()),
                        etag: Some("etag-grpc-tape".into()),
                    })),
                },
                PutStagingRequest {
                    payload: Some(PutStagingPayload::Data(body.clone())),
                },
            ])))
            .await
            .expect("put staging through real cache service");

        let (mut raw_tape_client, tape_shutdown_tx) = tape_backed_service().await;
        let cache = CacheArchiveClient::new(raw_cache_client.clone());
        let tape = TapeArchiveClient::new(
            raw_tape_client.clone(),
            "drive-0",
            "TAPE-GRPC",
            vec!["TAPE-GRPC".into()],
            262_144,
        );

        let archived = backend
            .archive_staging_batch(&cache, &tape, 10)
            .await
            .expect("archive through cache and tape grpc clients");
        assert_eq!(archived.archived_objects, 1);
        assert_eq!(archived.bytes_written, body.len() as u64);
        assert_eq!(
            archived.bundle_ids,
            vec!["phase1-bundle:docs/grpc-tape.txt"]
        );

        let object = backend
            .head_object("docs", "grpc-tape.txt")
            .await
            .expect("head archived object");
        assert_eq!(object.storage_class, common::StorageClass::Cold as i32);
        assert_eq!(object.tape_id.as_deref(), Some("TAPE-GRPC"));
        assert_eq!(object.tape_set, vec!["TAPE-GRPC"]);
        assert_eq!(object.tape_block_offset, Some(0));

        let listed = raw_cache_client
            .list_staging_keys(Request::new(ListStagingKeysRequest {
                limit: 10,
                after: None,
            }))
            .await
            .expect("list staging after archive")
            .into_inner();
        assert!(listed.entries.is_empty());
        assert_eq!(
            read_tape_client_filemark(&mut raw_tape_client, 0, body.len() as u64).await,
            body
        );

        metadata_shutdown_tx.send(()).ok();
        cache_shutdown_tx.send(()).ok();
        tape_shutdown_tx.send(()).ok();
    }

    #[test]
    fn phase1_unimplemented_message_is_stable() {
        let status = phase1_unimplemented("scheduler.list_buckets");
        assert_eq!(status.code(), tonic::Code::Unimplemented);
        assert!(status.message().contains("phase-1 safe mode"));
    }
}
