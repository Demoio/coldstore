#[cfg(feature = "metadata-raft-rocksdb")]
use anyhow::Result;
#[cfg(feature = "metadata-raft-rocksdb")]
use rocksdb::{Options, DB};
#[cfg(feature = "metadata-raft-rocksdb")]
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;
use tonic::Status;

use crate::service::{apply_command, MetadataCommand, MetadataState};
#[cfg(feature = "metadata-raft-rocksdb")]
use crate::service::{decode_snapshot, encode_snapshot};

pub type ColdStoreNodeId = u64;
pub type ColdStoreNode = openraft::BasicNode;

#[cfg(feature = "metadata-raft-rocksdb")]
#[allow(dead_code)]
const SNAPSHOT_KEY: &[u8] = b"metadata:snapshot:latest";

#[derive(Debug, Default)]
pub(crate) struct RaftMetadataBackend {
    proposed_commands: RwLock<u64>,
}

impl RaftMetadataBackend {
    #[allow(dead_code)]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    #[allow(dead_code)]
    pub(crate) async fn proposed_commands(&self) -> u64 {
        *self.proposed_commands.read().await
    }

    pub(crate) async fn propose_local_apply(
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

#[cfg(feature = "metadata-raft-rocksdb")]
#[allow(dead_code)]
pub(crate) struct RocksDbSnapshotStore {
    db: DB,
}

#[cfg(feature = "metadata-raft-rocksdb")]
#[allow(dead_code)]
impl RocksDbSnapshotStore {
    pub(crate) fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut options = Options::default();
        options.create_if_missing(true);
        Ok(Self {
            db: DB::open(&options, path)?,
        })
    }

    pub(crate) fn save_state_machine_snapshot(&self, state: &MetadataState) -> Result<()> {
        self.db.put(SNAPSHOT_KEY, encode_snapshot(state))?;
        Ok(())
    }

    pub(crate) fn load_state_machine_snapshot(&self) -> Result<Option<MetadataState>> {
        self.db
            .get(SNAPSHOT_KEY)?
            .map(|bytes| decode_snapshot(&bytes))
            .transpose()
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

    #[cfg(feature = "metadata-raft-rocksdb")]
    #[test]
    fn rocksdb_snapshot_store_round_trips_metadata_state() {
        let dir = std::env::temp_dir().join(format!(
            "coldstore-metadata-raft-snapshot-{}",
            uuid::Uuid::new_v4()
        ));
        let store = RocksDbSnapshotStore::open(&dir).expect("open rocksdb snapshot store");
        let mut state = MetadataState::default();
        apply_command(
            &mut state,
            MetadataCommand::CreateBucket(test_bucket("docs")),
        )
        .expect("create bucket");

        store
            .save_state_machine_snapshot(&state)
            .expect("save snapshot");
        let loaded = store
            .load_state_machine_snapshot()
            .expect("load snapshot")
            .expect("snapshot exists");

        assert!(loaded.buckets.contains_key("docs"));
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }
}
