use anyhow::Result;

/// 缓存后端抽象 trait
///
/// 当前实现: HddBackend (机械硬盘)
/// 目标实现: SpdkBlobBackend (SPDK Blobstore on NVMe)
#[tonic::async_trait]
pub trait CacheBackend: Send + Sync + 'static {
    /// 写入对象数据，返回内部存储 ID
    async fn write(&self, cache_key: &str, data: &[u8], xattrs: &CacheXattrs) -> Result<u64>;

    /// 读取对象数据
    async fn read(&self, storage_id: u64) -> Result<Vec<u8>>;

    /// 删除对象
    async fn delete(&self, storage_id: u64) -> Result<()>;

    /// 读取 xattrs
    async fn read_xattrs(&self, storage_id: u64) -> Result<CacheXattrs>;

    /// 列出所有存储对象（启动时重建索引用）
    async fn list_all(&self) -> Result<Vec<(u64, CacheXattrs)>>;

    /// 可用容量
    async fn available_bytes(&self) -> Result<u64>;
}

/// 缓存对象扩展属性
#[derive(Debug, Clone)]
pub struct CacheXattrs {
    pub bucket: String,
    pub key: String,
    pub version_id: Option<String>,
    pub size: u64,
    pub expire_at: i64,
    pub cached_at: i64,
    pub checksum: Option<String>,
    pub content_type: Option<String>,
    pub etag: Option<String>,
    /// staging = 暂存数据 (PutObject), restored = 解冻数据
    pub category: CacheCategory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheCategory {
    Staging,
    Restored,
}
