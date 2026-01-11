use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 存储类别
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StorageClass {
    Hot,
    Warm,
    Cold,
    ColdPending,
}

/// 解冻状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RestoreStatus {
    Pending,
    InProgress,
    Completed,
    Expired,
    Failed,
}

/// 磁带状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TapeStatus {
    Online,
    Offline,
    Unknown,
    Error,
}

/// 对象元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectMetadata {
    pub bucket: String,
    pub object_key: String,
    pub version: Option<String>,
    pub storage_class: StorageClass,
    pub archive_id: Option<Uuid>,
    pub tape_id: Option<String>,
    pub tape_set: Option<Vec<String>>,
    pub checksum: String,
    pub size: u64,
    pub restore_status: Option<RestoreStatus>,
    pub restore_expire_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 归档包（Archive Bundle）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveBundle {
    pub id: Uuid,
    pub tape_id: String,
    pub object_keys: Vec<String>,
    pub total_size: u64,
    pub created_at: DateTime<Utc>,
    pub status: ArchiveBundleStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArchiveBundleStatus {
    Pending,
    Writing,
    Completed,
    Failed,
}

/// 磁带信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TapeInfo {
    pub id: String,
    pub format: String, // e.g., "LTO-9", "LTO-10"
    pub status: TapeStatus,
    pub location: Option<String>,
    pub capacity_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
    pub archive_bundles: Vec<Uuid>,
    pub last_verified_at: Option<DateTime<Utc>>,
}

/// 取回任务
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallTask {
    pub id: Uuid,
    pub object_key: String,
    pub bucket: String,
    pub archive_id: Uuid,
    pub tape_id: String,
    pub status: RestoreStatus,
    pub priority: u32,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

/// 归档任务
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveTask {
    pub id: Uuid,
    pub object_keys: Vec<String>,
    pub archive_bundle_id: Uuid,
    pub status: ArchiveTaskStatus,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArchiveTaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}
