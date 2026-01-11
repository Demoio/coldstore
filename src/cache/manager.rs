use crate::error::Result;
use lru::LruCache;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct CacheManager {
    cache: Arc<RwLock<LruCache<String, CachedObject>>>,
    base_path: PathBuf,
    max_size_bytes: u64,
    ttl_secs: u64,
}

struct CachedObject {
    path: PathBuf,
    size: u64,
    cached_at: chrono::DateTime<chrono::Utc>,
}

impl CacheManager {
    pub fn new(base_path: PathBuf, max_size_gb: u64, ttl_secs: u64) -> Result<Self> {
        // 创建缓存目录
        std::fs::create_dir_all(&base_path)?;

        // 估算最大缓存项数（假设平均对象大小 10MB）
        let estimated_max_items =
            (max_size_gb * 1024 * 1024 * 1024 / (10 * 1024 * 1024)).max(1000) as usize;

        let cache = Arc::new(RwLock::new(LruCache::new(
            NonZeroUsize::new(estimated_max_items).unwrap(),
        )));

        Ok(Self {
            cache,
            base_path,
            max_size_bytes: max_size_gb * 1024 * 1024 * 1024,
            ttl_secs,
        })
    }

    /// 获取缓存对象
    pub async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let mut cache = self.cache.write().await;

        if let Some(cached) = cache.get(key) {
            // 检查是否过期
            let now = chrono::Utc::now();
            let age = now.signed_duration_since(cached.cached_at);

            if age.num_seconds() < self.ttl_secs as i64 {
                // 从文件系统读取
                let data = std::fs::read(&cached.path)?;
                return Ok(Some(data));
            } else {
                // 过期，移除
                cache.pop(key);
            }
        }

        Ok(None)
    }

    /// 写入缓存
    pub async fn put(&self, key: &str, data: Vec<u8>) -> Result<()> {
        // TODO: 实现缓存写入逻辑
        // 1. 检查缓存大小限制
        // 2. 写入文件系统
        // 3. 更新 LRU 缓存索引
        // 4. 如果超过限制，执行淘汰

        let file_path = self.base_path.join(format!("{}.cache", key));
        std::fs::write(&file_path, &data)?;

        let cached = CachedObject {
            path: file_path,
            size: data.len() as u64,
            cached_at: chrono::Utc::now(),
        };

        let mut cache = self.cache.write().await;
        cache.put(key.to_string(), cached);

        Ok(())
    }

    /// 删除缓存
    pub async fn evict(&self, key: &str) -> Result<()> {
        let mut cache = self.cache.write().await;

        if let Some(cached) = cache.pop(key) {
            let _ = std::fs::remove_file(&cached.path);
        }

        Ok(())
    }

    /// 清理过期缓存
    pub async fn cleanup_expired(&self) -> Result<usize> {
        // TODO: 实现过期缓存清理
        let mut cache = self.cache.write().await;
        let now = chrono::Utc::now();
        let mut removed = 0;

        // 注意：LRU 缓存不支持直接遍历，需要特殊处理
        // 这里简化处理，实际应该维护一个过期时间索引

        Ok(removed)
    }
}
