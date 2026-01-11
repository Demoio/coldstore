use crate::error::Result;
use crate::metadata::backend::MetadataBackend;
use crate::models::ObjectMetadata;

pub struct MetadataService {
    backend: MetadataBackend,
}

impl MetadataService {
    pub fn new(backend: MetadataBackend) -> Self {
        Self { backend }
    }

    /// 获取对象元数据
    pub async fn get_object(&self, bucket: &str, key: &str) -> Result<ObjectMetadata> {
        self.backend.get_object(bucket, key).await
    }

    /// 创建或更新对象元数据
    pub async fn put_object(&self, metadata: ObjectMetadata) -> Result<()> {
        self.backend.put_object(metadata).await
    }

    /// 删除对象元数据
    pub async fn delete_object(&self, bucket: &str, key: &str) -> Result<()> {
        self.backend.delete_object(bucket, key).await
    }

    /// 列出对象
    pub async fn list_objects(
        &self,
        bucket: &str,
        prefix: Option<&str>,
        max_keys: Option<usize>,
    ) -> Result<Vec<ObjectMetadata>> {
        self.backend.list_objects(bucket, prefix, max_keys).await
    }

    /// 更新对象存储类别
    pub async fn update_storage_class(
        &self,
        bucket: &str,
        key: &str,
        storage_class: crate::models::StorageClass,
    ) -> Result<()> {
        self.backend
            .update_storage_class(bucket, key, storage_class)
            .await
    }
}
