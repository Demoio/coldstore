use crate::backend::{CacheBackend, CacheCategory, CacheXattrs};
use crate::hdd::HddBackend;
use anyhow::Result;
use coldstore_common::config::{CacheBackendConfig, CacheConfig};
use coldstore_proto::cache::cache_service_server::CacheService;
use coldstore_proto::cache::*;
use prost_types::Timestamp;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

const STREAM_CHUNK_SIZE: usize = 64 * 1024;

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
struct CacheKey {
    bucket: String,
    key: String,
    version_id: Option<String>,
}

impl CacheKey {
    fn new(bucket: String, key: String, version_id: Option<String>) -> Self {
        Self {
            bucket,
            key,
            version_id,
        }
    }

    fn as_cursor(&self) -> String {
        format!(
            "{}\u{0}{}\u{0}{}",
            self.bucket,
            self.key,
            self.version_id.clone().unwrap_or_default()
        )
    }
}

#[derive(Debug, Clone)]
struct StoredEntry {
    storage_id: u64,
    xattrs: CacheXattrs,
}

#[derive(Default)]
struct CacheIndex {
    staging: HashMap<CacheKey, StoredEntry>,
    restored: HashMap<CacheKey, StoredEntry>,
    hit_count: u64,
    miss_count: u64,
    evict_count: u64,
    evict_bytes: u64,
}

pub struct CacheServiceImpl {
    backend: Arc<dyn CacheBackend>,
    _config: CacheConfig,
    index: Arc<RwLock<CacheIndex>>,
}

impl CacheServiceImpl {
    pub async fn new(config: &CacheConfig) -> Result<Self> {
        let backend: Arc<dyn CacheBackend> = match &config.backend {
            CacheBackendConfig::Hdd { path, max_size_gb } => {
                Arc::new(HddBackend::new(path.clone(), *max_size_gb).await?)
            }
            CacheBackendConfig::Spdk { .. } => {
                anyhow::bail!("SPDK backend not yet implemented")
            }
        };

        let svc = Self {
            backend,
            _config: config.clone(),
            index: Arc::new(RwLock::new(CacheIndex::default())),
        };
        svc.rebuild_index().await?;
        Ok(svc)
    }

    async fn rebuild_index(&self) -> Result<()> {
        let mut index = CacheIndex::default();
        for (storage_id, xattrs) in self.backend.list_all().await? {
            let key = CacheKey::new(
                xattrs.bucket.clone(),
                xattrs.key.clone(),
                xattrs.version_id.clone(),
            );
            let entry = StoredEntry { storage_id, xattrs };
            match entry.xattrs.category {
                CacheCategory::Staging => {
                    index.staging.insert(key, entry);
                }
                CacheCategory::Restored => {
                    index.restored.insert(key, entry);
                }
            }
        }
        *self.index.write().await = index;
        Ok(())
    }

    async fn remove_existing(&self, key: &CacheKey, category: CacheCategory) -> Result<()> {
        let existing = {
            let index = self.index.read().await;
            match category {
                CacheCategory::Staging => index.staging.get(key).cloned(),
                CacheCategory::Restored => index.restored.get(key).cloned(),
            }
        };

        if let Some(existing) = existing {
            self.backend.delete(existing.storage_id).await?;
            let mut index = self.index.write().await;
            match category {
                CacheCategory::Staging => {
                    index.staging.remove(key);
                }
                CacheCategory::Restored => {
                    index.restored.remove(key);
                }
            }
            index.evict_count += 1;
            index.evict_bytes += existing.xattrs.size;
        }
        Ok(())
    }

    async fn find_entry(&self, key: &CacheKey, category: CacheCategory) -> Option<StoredEntry> {
        let index = self.index.read().await;
        match category {
            CacheCategory::Staging => index.staging.get(key).cloned(),
            CacheCategory::Restored => index.restored.get(key).cloned(),
        }
    }

    async fn insert_entry(&self, key: CacheKey, entry: StoredEntry) {
        let mut index = self.index.write().await;
        match entry.xattrs.category {
            CacheCategory::Staging => {
                index.staging.insert(key, entry);
            }
            CacheCategory::Restored => {
                index.restored.insert(key, entry);
            }
        }
    }

    async fn update_hit_state(&self, hit: bool) {
        let mut index = self.index.write().await;
        if hit {
            index.hit_count += 1;
        } else {
            index.miss_count += 1;
        }
    }

    async fn delete_entry(&self, key: &CacheKey, category: CacheCategory) -> Result<bool> {
        let removed = {
            let mut index = self.index.write().await;
            match category {
                CacheCategory::Staging => index.staging.remove(key),
                CacheCategory::Restored => index.restored.remove(key),
            }
        };

        if let Some(entry) = removed {
            self.backend.delete(entry.storage_id).await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn put_bytes(&self, key: CacheKey, data: Vec<u8>, xattrs: CacheXattrs) -> Result<u64> {
        self.remove_existing(&key, xattrs.category).await?;
        let storage_id = self.backend.write(&key.as_cursor(), &data, &xattrs).await?;
        self.insert_entry(key, StoredEntry { storage_id, xattrs })
            .await;
        Ok(storage_id)
    }

    async fn read_restored(&self, key: &CacheKey) -> Result<StoredEntry, Status> {
        let Some(entry) = self.find_entry(key, CacheCategory::Restored).await else {
            self.update_hit_state(false).await;
            return Err(Status::not_found("restored object not found in cache"));
        };

        if is_expired(entry.xattrs.expire_at) {
            let _ = self.delete_entry(key, CacheCategory::Restored).await;
            self.update_hit_state(false).await;
            return Err(Status::not_found("restored object has expired"));
        }

        self.update_hit_state(true).await;
        Ok(entry)
    }

    async fn build_get_stream(
        &self,
        entry: StoredEntry,
    ) -> Result<Response<ReceiverStream<Result<GetResponse, Status>>>, Status> {
        let data = self
            .backend
            .read(entry.storage_id)
            .await
            .map_err(internal_status)?;
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(async move {
            let meta = GetResponse {
                payload: Some(get_response::Payload::Meta(CachedObjectMeta {
                    size: entry.xattrs.size,
                    expire_at: Some(timestamp_from_unix(entry.xattrs.expire_at)),
                    content_type: entry.xattrs.content_type.clone(),
                    etag: entry.xattrs.etag.clone(),
                    checksum: entry.xattrs.checksum.clone(),
                })),
            };
            let _ = tx.send(Ok(meta)).await;
            for chunk in data.chunks(STREAM_CHUNK_SIZE) {
                let _ = tx
                    .send(Ok(GetResponse {
                        payload: Some(get_response::Payload::Data(chunk.to_vec())),
                    }))
                    .await;
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn build_staging_stream(
        &self,
        entry: StoredEntry,
    ) -> Result<Response<ReceiverStream<Result<GetStagingResponse, Status>>>, Status> {
        let data = self
            .backend
            .read(entry.storage_id)
            .await
            .map_err(internal_status)?;
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(async move {
            let meta = GetStagingResponse {
                payload: Some(get_staging_response::Payload::Meta(StagingObjectMeta {
                    bucket: entry.xattrs.bucket.clone(),
                    key: entry.xattrs.key.clone(),
                    version_id: entry.xattrs.version_id.clone(),
                    size: entry.xattrs.size,
                    checksum: entry.xattrs.checksum.clone(),
                    content_type: entry.xattrs.content_type.clone(),
                    etag: entry.xattrs.etag.clone(),
                    staged_at: Some(timestamp_from_unix(entry.xattrs.cached_at)),
                })),
            };
            let _ = tx.send(Ok(meta)).await;
            for chunk in data.chunks(STREAM_CHUNK_SIZE) {
                let _ = tx
                    .send(Ok(GetStagingResponse {
                        payload: Some(get_staging_response::Payload::Data(chunk.to_vec())),
                    }))
                    .await;
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

#[tonic::async_trait]
impl CacheService for CacheServiceImpl {
    async fn put_staging(
        &self,
        req: Request<Streaming<PutStagingRequest>>,
    ) -> std::result::Result<Response<PutStagingResponse>, Status> {
        let mut stream = req.into_inner();
        let mut meta: Option<PutStagingMeta> = None;
        let mut data = Vec::new();

        while let Some(chunk) = stream.message().await? {
            match chunk.payload {
                Some(put_staging_request::Payload::Meta(m)) => meta = Some(m),
                Some(put_staging_request::Payload::Data(bytes)) => data.extend_from_slice(&bytes),
                None => return Err(Status::invalid_argument("empty put_staging chunk")),
            }
        }

        let meta = meta.ok_or_else(|| Status::invalid_argument("missing staging metadata"))?;
        if meta.size != data.len() as u64 {
            return Err(Status::invalid_argument(
                "staging object size does not match payload",
            ));
        }

        let key = CacheKey::new(
            meta.bucket.clone(),
            meta.key.clone(),
            meta.version_id.clone(),
        );
        let xattrs = CacheXattrs {
            bucket: meta.bucket,
            key: meta.key,
            version_id: meta.version_id,
            size: meta.size,
            expire_at: 0,
            cached_at: now_unix(),
            checksum: meta.checksum,
            content_type: meta.content_type,
            etag: meta.etag,
            category: CacheCategory::Staging,
        };
        let storage_id = self
            .put_bytes(key, data, xattrs)
            .await
            .map_err(internal_status)?;

        Ok(Response::new(PutStagingResponse {
            staging_id: storage_id.to_string(),
        }))
    }

    async fn put_restored(
        &self,
        req: Request<Streaming<PutRestoredRequest>>,
    ) -> std::result::Result<Response<()>, Status> {
        let mut stream = req.into_inner();
        let mut meta: Option<PutRestoredMeta> = None;
        let mut data = Vec::new();

        while let Some(chunk) = stream.message().await? {
            match chunk.payload {
                Some(put_restored_request::Payload::Meta(m)) => meta = Some(m),
                Some(put_restored_request::Payload::Data(bytes)) => data.extend_from_slice(&bytes),
                None => return Err(Status::invalid_argument("empty put_restored chunk")),
            }
        }

        let meta = meta.ok_or_else(|| Status::invalid_argument("missing restored metadata"))?;
        if meta.size != data.len() as u64 {
            return Err(Status::invalid_argument(
                "restored object size does not match payload",
            ));
        }
        let expire_at = meta
            .expire_at
            .ok_or_else(|| Status::invalid_argument("restored object missing expire_at"))?;

        let key = CacheKey::new(
            meta.bucket.clone(),
            meta.key.clone(),
            meta.version_id.clone(),
        );
        let xattrs = CacheXattrs {
            bucket: meta.bucket,
            key: meta.key,
            version_id: meta.version_id,
            size: meta.size,
            expire_at: expire_at.seconds,
            cached_at: now_unix(),
            checksum: meta.checksum,
            content_type: meta.content_type,
            etag: meta.etag,
            category: CacheCategory::Restored,
        };
        self.put_bytes(key, data, xattrs)
            .await
            .map_err(internal_status)?;

        Ok(Response::new(()))
    }

    async fn delete(
        &self,
        req: Request<DeleteRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        let req = req.into_inner();
        let key = CacheKey::new(req.bucket, req.key, req.version_id);
        self.delete_entry(&key, CacheCategory::Restored)
            .await
            .map_err(internal_status)?;
        Ok(Response::new(()))
    }

    type GetStream = ReceiverStream<Result<GetResponse, Status>>;

    async fn get(
        &self,
        req: Request<GetRequest>,
    ) -> std::result::Result<Response<Self::GetStream>, Status> {
        let req = req.into_inner();
        let key = CacheKey::new(req.bucket, req.key, req.version_id);
        let entry = self.read_restored(&key).await?;
        self.build_get_stream(entry).await
    }

    async fn contains(
        &self,
        req: Request<ContainsRequest>,
    ) -> std::result::Result<Response<ContainsResponse>, Status> {
        let req = req.into_inner();
        let key = CacheKey::new(req.bucket, req.key, req.version_id);
        let exists = self.find_entry(&key, CacheCategory::Restored).await;
        let response = if let Some(entry) = exists {
            if is_expired(entry.xattrs.expire_at) {
                let _ = self.delete_entry(&key, CacheCategory::Restored).await;
                self.update_hit_state(false).await;
                ContainsResponse {
                    exists: false,
                    expire_at: None,
                }
            } else {
                self.update_hit_state(true).await;
                ContainsResponse {
                    exists: true,
                    expire_at: Some(timestamp_from_unix(entry.xattrs.expire_at)),
                }
            }
        } else {
            self.update_hit_state(false).await;
            ContainsResponse {
                exists: false,
                expire_at: None,
            }
        };
        Ok(Response::new(response))
    }

    type GetStagingStream = ReceiverStream<Result<GetStagingResponse, Status>>;

    async fn get_staging(
        &self,
        req: Request<GetStagingRequest>,
    ) -> std::result::Result<Response<Self::GetStagingStream>, Status> {
        let req = req.into_inner();
        let key = CacheKey::new(req.bucket, req.key, req.version_id);
        let Some(entry) = self.find_entry(&key, CacheCategory::Staging).await else {
            return Err(Status::not_found("staging object not found"));
        };
        self.build_staging_stream(entry).await
    }

    async fn list_staging_keys(
        &self,
        req: Request<ListStagingKeysRequest>,
    ) -> std::result::Result<Response<ListStagingKeysResponse>, Status> {
        let req = req.into_inner();
        let after = req.after.unwrap_or_default();
        let limit = if req.limit == 0 {
            usize::MAX
        } else {
            req.limit as usize
        };

        let index = self.index.read().await;
        let mut entries: Vec<_> = index
            .staging
            .iter()
            .filter(|(key, _)| key.as_cursor() > after)
            .map(|(key, entry)| (key.clone(), entry.clone()))
            .collect();
        entries.sort_by(|(a, _), (b, _)| a.as_cursor().cmp(&b.as_cursor()));

        let has_more = entries.len() > limit;
        let response_entries = entries
            .into_iter()
            .take(limit)
            .map(|(key, entry)| StagingKeyEntry {
                bucket: key.bucket,
                key: key.key,
                version_id: key.version_id,
                size: entry.xattrs.size,
                staged_at: Some(timestamp_from_unix(entry.xattrs.cached_at)),
            })
            .collect();

        Ok(Response::new(ListStagingKeysResponse {
            entries: response_entries,
            has_more,
        }))
    }

    async fn delete_staging(
        &self,
        req: Request<DeleteStagingRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        let req = req.into_inner();
        let key = CacheKey::new(req.bucket, req.key, req.version_id);
        self.delete_entry(&key, CacheCategory::Staging)
            .await
            .map_err(internal_status)?;
        Ok(Response::new(()))
    }

    async fn stats(&self, _req: Request<()>) -> std::result::Result<Response<CacheStats>, Status> {
        let available = self
            .backend
            .available_bytes()
            .await
            .map_err(internal_status)?;
        let index = self.index.read().await;
        let staging_bytes: u64 = index.staging.values().map(|entry| entry.xattrs.size).sum();
        let restored_bytes: u64 = index.restored.values().map(|entry| entry.xattrs.size).sum();
        let used_capacity = staging_bytes + restored_bytes;
        let total_capacity = used_capacity + available;

        Ok(Response::new(CacheStats {
            total_capacity,
            used_capacity,
            object_count: (index.staging.len() + index.restored.len()) as u64,
            staging_count: index.staging.len() as u64,
            staging_bytes,
            restored_count: index.restored.len() as u64,
            restored_bytes,
            hit_count: index.hit_count,
            miss_count: index.miss_count,
            evict_count: index.evict_count,
            evict_bytes: index.evict_bytes,
        }))
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs() as i64
}

fn timestamp_from_unix(seconds: i64) -> Timestamp {
    Timestamp { seconds, nanos: 0 }
}

fn is_expired(expire_at: i64) -> bool {
    expire_at > 0 && expire_at <= now_unix()
}

fn internal_status(err: anyhow::Error) -> Status {
    Status::internal(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use coldstore_proto::cache::ContainsRequest;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio_stream::StreamExt;

    fn test_config() -> CacheConfig {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        CacheConfig {
            backend: CacheBackendConfig::Hdd {
                path: format!("/tmp/coldstore-cache-test-{unique}"),
                max_size_gb: 1,
            },
            ..CacheConfig::default()
        }
    }

    #[tokio::test]
    async fn empty_cache_reports_miss() {
        let svc = CacheServiceImpl::new(&test_config())
            .await
            .expect("service init");
        let response = svc
            .contains(Request::new(ContainsRequest {
                bucket: "docs".into(),
                key: "readme.txt".into(),
                version_id: None,
            }))
            .await
            .expect("contains should return response")
            .into_inner();

        assert!(!response.exists);
    }

    #[tokio::test]
    async fn restored_object_round_trip_streams_data() {
        let svc = CacheServiceImpl::new(&test_config())
            .await
            .expect("service init");
        let expires_at = now_unix() + 3600;
        let key = CacheKey::new("docs".into(), "guide.txt".into(), Some("v1".into()));
        let xattrs = CacheXattrs {
            bucket: "docs".into(),
            key: "guide.txt".into(),
            version_id: Some("v1".into()),
            size: 11,
            expire_at: expires_at,
            cached_at: now_unix(),
            checksum: Some("sum".into()),
            content_type: Some("text/plain".into()),
            etag: Some("etag-1".into()),
            category: CacheCategory::Restored,
        };
        svc.put_bytes(key, b"hello world".to_vec(), xattrs)
            .await
            .expect("put restored should succeed");

        let contains = svc
            .contains(Request::new(ContainsRequest {
                bucket: "docs".into(),
                key: "guide.txt".into(),
                version_id: Some("v1".into()),
            }))
            .await
            .expect("contains should succeed")
            .into_inner();
        assert!(contains.exists);

        let mut stream = svc
            .get(Request::new(GetRequest {
                bucket: "docs".into(),
                key: "guide.txt".into(),
                version_id: Some("v1".into()),
            }))
            .await
            .expect("get should succeed")
            .into_inner();

        let first = stream.next().await.expect("meta chunk").expect("meta ok");
        match first.payload {
            Some(get_response::Payload::Meta(meta)) => {
                assert_eq!(meta.size, 11);
                assert_eq!(meta.etag.as_deref(), Some("etag-1"));
            }
            other => panic!("unexpected first payload: {other:?}"),
        }

        let second = stream.next().await.expect("data chunk").expect("data ok");
        match second.payload {
            Some(get_response::Payload::Data(bytes)) => assert_eq!(bytes, b"hello world"),
            other => panic!("unexpected second payload: {other:?}"),
        }
    }

    #[tokio::test]
    async fn staging_keys_are_listed() {
        let svc = CacheServiceImpl::new(&test_config())
            .await
            .expect("service init");
        let key = CacheKey::new("docs".into(), "draft.txt".into(), None);
        let xattrs = CacheXattrs {
            bucket: "docs".into(),
            key: "draft.txt".into(),
            version_id: None,
            size: 5,
            expire_at: 0,
            cached_at: now_unix(),
            checksum: None,
            content_type: None,
            etag: Some("etag-2".into()),
            category: CacheCategory::Staging,
        };
        svc.put_bytes(key, b"draft".to_vec(), xattrs)
            .await
            .expect("put staging should succeed");

        let listed = svc
            .list_staging_keys(Request::new(ListStagingKeysRequest {
                limit: 10,
                after: None,
            }))
            .await
            .expect("list staging should succeed")
            .into_inner();

        assert_eq!(listed.entries.len(), 1);
        assert_eq!(listed.entries[0].bucket, "docs");
        assert_eq!(listed.entries[0].key, "draft.txt");
    }
}
