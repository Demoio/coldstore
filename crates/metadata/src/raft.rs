//! Feature-gated Raft-like metadata backend abstraction.
//!
//! In this phase, this module provides strict proposal semantics and a pluggable
//! backend model:
//! - local in-memory apply path for deterministic single-process behavior
//! - optional persistent local path (rocksdb-backed storage module) for production
//!   readiness preparation and recovery tests

use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;
use tonic::Status;

use crate::command::MetadataCommand;
use crate::state_machine::{apply_command, MetadataState};

#[cfg(feature = "metadata-raft-rocksdb")]
use crate::raft_storage::LocalSingleNodeMetadataRaft;

pub type ColdStoreNodeId = u64;
pub type ColdStoreNode = openraft::BasicNode;

enum RaftBackendMode {
    Memory,
    #[cfg(feature = "metadata-raft-rocksdb")]
    Persistent {
        storage: Arc<LocalSingleNodeMetadataRaft>,
    },
}

pub struct RaftMetadataBackend {
    inner: RaftBackendMode,
    proposed_commands: RwLock<u64>,
}

impl Default for RaftMetadataBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl RaftMetadataBackend {
    pub fn new() -> Self {
        Self {
            inner: RaftBackendMode::Memory,
            proposed_commands: RwLock::new(0),
        }
    }

    #[cfg(feature = "metadata-raft-rocksdb")]
    pub fn new_with_storage(storage_path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let storage = Arc::new(LocalSingleNodeMetadataRaft::open(storage_path)?);
        let committed = storage.applied_log_count()?;
        Ok(Self {
            inner: RaftBackendMode::Persistent { storage },
            proposed_commands: RwLock::new(committed),
        })
    }

    pub async fn proposed_commands(&self) -> u64 {
        *self.proposed_commands.read().await
    }

    pub async fn cluster_term_and_leader(&self) -> (u64, Option<ColdStoreNodeId>) {
        match &self.inner {
            RaftBackendMode::Memory => (1, None),
            #[cfg(feature = "metadata-raft-rocksdb")]
            RaftBackendMode::Persistent { storage } => storage
                .load_vote()
                .unwrap_or(None)
                .map(|(term, node_id)| (term, Some(node_id)))
                .unwrap_or((0, None)),
        }
    }

    pub fn mode_label(&self) -> &'static str {
        self.mode()
    }

    #[cfg(feature = "metadata-raft-rocksdb")]
    pub fn mode(&self) -> &'static str {
        match self.inner {
            RaftBackendMode::Memory => "local_memory",
            RaftBackendMode::Persistent { .. } => "persistent_local",
        }
    }

    #[cfg(not(feature = "metadata-raft-rocksdb"))]
    pub fn mode(&self) -> &'static str {
        "local_memory"
    }

    pub async fn bootstrapped_state(&self) -> anyhow::Result<Option<MetadataState>> {
        match &self.inner {
            RaftBackendMode::Memory => Ok(None),
            #[cfg(feature = "metadata-raft-rocksdb")]
            RaftBackendMode::Persistent { storage } => Ok(Some(storage.snapshot()?)),
        }
    }

    /// Propose a metadata command for the active backend and apply it through the
    /// local state machine view.
    pub async fn propose_local_apply(
        &self,
        state: &Arc<RwLock<MetadataState>>,
        command: MetadataCommand,
    ) -> std::result::Result<(), Status> {
        // Always apply command locally first for deterministic state progression.
        {
            let mut guard = state.write().await;
            apply_command(&mut guard, command.clone())?;
        }

        if let Some(next_committed_index) = self.persist_if_needed(&command).await? {
            let mut counter = self.proposed_commands.write().await;
            *counter = next_committed_index;
            return Ok(());
        }

        {
            let mut counter = self.proposed_commands.write().await;
            *counter = counter.saturating_add(1);
        }

        Ok(())
    }

    pub async fn bootstrap_state_if_empty(
        &self,
        state: &MetadataState,
    ) -> std::result::Result<(), Status> {
        if state.is_empty() {
            return Ok(());
        }

        match &self.inner {
            RaftBackendMode::Memory => Ok(()),
            #[cfg(feature = "metadata-raft-rocksdb")]
            RaftBackendMode::Persistent { storage } => {
                let existing = storage
                    .load_state_machine_snapshot()
                    .map_err(|err| Status::internal(err.to_string()))?;
                if existing.is_some() {
                    return Ok(());
                }

                storage
                    .save_state_machine_snapshot(state)
                    .map_err(|err| Status::internal(err.to_string()))
            }
        }
    }

    async fn persist_if_needed(
        &self,
        command: &MetadataCommand,
    ) -> std::result::Result<Option<u64>, Status> {
        match &self.inner {
            RaftBackendMode::Memory => Ok(None),
            #[cfg(feature = "metadata-raft-rocksdb")]
            RaftBackendMode::Persistent { storage } => {
                let next = storage
                    .propose(command.clone())
                    .map_err(|err| Status::internal(err.to_string()))?;
                Ok(Some(next))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coldstore_proto::common;

    fn test_bucket(name: &str) -> common::BucketInfo {
        common::BucketInfo {
            name: name.into(),
            created_at: None,
            owner: Some("tester".into()),
            versioning_enabled: false,
            object_count: 0,
            total_size: 0,
        }
    }

    #[tokio::test]
    async fn raft_backend_proposes_commands_through_state_machine_apply() {
        let state = Arc::new(RwLock::new(MetadataState::default()));
        let backend = RaftMetadataBackend::new();

        backend
            .propose_local_apply(&state, MetadataCommand::CreateBucket(test_bucket("docs")))
            .await
            .expect("command should apply");

        assert_eq!(backend.proposed_commands().await, 1);
        assert!(state.read().await.buckets.contains_key("docs"));
        assert_eq!(backend.mode(), "local_memory");
    }

    #[cfg(feature = "metadata-raft-rocksdb")]
    #[tokio::test]
    async fn raft_backend_bootstraps_from_persistent_storage() {
        use std::path::PathBuf;

        let dir = std::env::temp_dir().join(format!(
            "coldstore-metadata-raft-backend-{}",
            uuid::Uuid::new_v4()
        ));
        let storage = crate::raft_storage::LocalSingleNodeMetadataRaft::open(&dir)
            .expect("open local single node raft storage");

        storage
            .propose(MetadataCommand::CreateBucket(test_bucket("docs")))
            .expect("persist bootstrap snapshot command");
        let _ = storage.applied_log_count().expect("applied count");
        drop(storage);

        let backend = RaftMetadataBackend::new_with_storage(PathBuf::from(&dir))
            .expect("backend from storage");
        let restored = backend
            .bootstrapped_state()
            .await
            .expect("read bootstrap state")
            .expect("bootstrap exists");

        assert!(restored.bucket("docs").is_some());
        assert_eq!(backend.mode(), "persistent_local");
        let _ = std::fs::remove_dir_all(dir);
    }
}
