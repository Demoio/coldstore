//! Feature-gated local propose backend for metadata Raft integration.
//!
//! This is not a complete OpenRaft runtime. It provides an explicit, opt-in
//! proposal boundary so `MetadataServiceImpl` can route writes through the same
//! command/state-machine path that a real Raft state machine will use later.

use std::sync::Arc;
use tokio::sync::RwLock;
use tonic::Status;

use crate::command::MetadataCommand;
use crate::state_machine::{apply_command, MetadataState};

pub type ColdStoreNodeId = u64;
pub type ColdStoreNode = openraft::BasicNode;

#[derive(Debug, Default)]
pub struct RaftMetadataBackend {
    proposed_commands: RwLock<u64>,
}

impl RaftMetadataBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn proposed_commands(&self) -> u64 {
        *self.proposed_commands.read().await
    }

    pub async fn propose_local_apply(
        &self,
        state: &Arc<RwLock<MetadataState>>,
        command: MetadataCommand,
    ) -> std::result::Result<(), Status> {
        let mut guard = state.write().await;
        apply_command(&mut guard, command)?;
        *self.proposed_commands.write().await += 1;
        Ok(())
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
    async fn raft_backend_proposes_commands_through_state_machine_apply_path() {
        let state = Arc::new(RwLock::new(MetadataState::default()));
        let backend = RaftMetadataBackend::new();

        backend
            .propose_local_apply(&state, MetadataCommand::CreateBucket(test_bucket("docs")))
            .await
            .expect("command should apply");

        assert_eq!(backend.proposed_commands().await, 1);
        assert!(state.read().await.buckets.contains_key("docs"));
    }
}
