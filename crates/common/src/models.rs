use serde::{Deserialize, Serialize};

/// 存储类别（纯冷归档：仅 ColdPending 和 Cold）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StorageClass {
    ColdPending,
    Cold,
}

/// 解冻状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RestoreStatus {
    Pending,
    WaitingForMedia,
    InProgress,
    Completed,
    Expired,
    Failed,
}

/// 解冻优先级
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RestoreTier {
    Expedited,
    Standard,
    Bulk,
}

/// 磁带状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TapeStatus {
    Online,
    Offline,
    Error,
    Retired,
    Unknown,
}

/// 驱动状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DriveStatus {
    Idle,
    InUse,
    Loading,
    Unloading,
    Error,
    Offline,
}

/// 节点状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeStatus {
    Online,
    Offline,
    Draining,
    Maintenance,
}

/// Worker 类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerType {
    Scheduler,
    Cache,
    Tape,
}

/// 归档包状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArchiveBundleStatus {
    Pending,
    Writing,
    Completed,
    Failed,
}

/// 归档任务状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArchiveTaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

// ---------------------------------------------------------------------------
//  状态流转校验
// ---------------------------------------------------------------------------

impl RestoreStatus {
    pub fn can_transition_to(&self, target: &RestoreStatus) -> bool {
        matches!(
            (self, target),
            (RestoreStatus::Pending, RestoreStatus::InProgress)
                | (RestoreStatus::Pending, RestoreStatus::WaitingForMedia)
                | (RestoreStatus::Pending, RestoreStatus::Failed)
                | (RestoreStatus::WaitingForMedia, RestoreStatus::Pending)
                | (RestoreStatus::WaitingForMedia, RestoreStatus::Failed)
                | (RestoreStatus::InProgress, RestoreStatus::Completed)
                | (RestoreStatus::InProgress, RestoreStatus::Failed)
                | (RestoreStatus::Completed, RestoreStatus::Expired)
        )
    }
}

impl ArchiveBundleStatus {
    pub fn can_transition_to(&self, target: &ArchiveBundleStatus) -> bool {
        matches!(
            (self, target),
            (ArchiveBundleStatus::Pending, ArchiveBundleStatus::Writing)
                | (ArchiveBundleStatus::Writing, ArchiveBundleStatus::Completed)
                | (ArchiveBundleStatus::Writing, ArchiveBundleStatus::Failed)
                | (ArchiveBundleStatus::Failed, ArchiveBundleStatus::Pending)
        )
    }
}

impl ArchiveTaskStatus {
    pub fn can_transition_to(&self, target: &ArchiveTaskStatus) -> bool {
        matches!(
            (self, target),
            (ArchiveTaskStatus::Pending, ArchiveTaskStatus::InProgress)
                | (ArchiveTaskStatus::InProgress, ArchiveTaskStatus::Completed)
                | (ArchiveTaskStatus::InProgress, ArchiveTaskStatus::Failed)
                | (ArchiveTaskStatus::Failed, ArchiveTaskStatus::Pending)
        )
    }
}
