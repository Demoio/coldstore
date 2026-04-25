//! RocksDB-backed metadata persistence primitives for the feature-gated Raft path.
//!
//! This module intentionally separates Raft vote records, command-log records,
//! and metadata state-machine snapshots into distinct key spaces. The current
//! `LocalSingleNodeMetadataRaft` is a local safety harness for single-process
//! tests; it does not start an OpenRaft runtime, does not provide networking,
//! and does not yet replay log records after restart.

use anyhow::Result;
use rocksdb::{Direction, IteratorMode, Options, DB};
use std::path::Path;
use std::sync::Mutex;

use crate::command::MetadataCommand;
use crate::raft::ColdStoreNodeId;
use crate::state_machine::{MetadataState, MetadataStateMachine};

const SNAPSHOT_KEY: &[u8] = b"metadata:snapshot:latest";
const VOTE_KEY: &[u8] = b"raft:vote";
const LOG_KEY_PREFIX: &[u8] = b"raft:log:";

pub struct RocksDbRaftStorage {
    db: DB,
}

impl RocksDbRaftStorage {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut options = Options::default();
        options.create_if_missing(true);
        Ok(Self {
            db: DB::open(&options, path)?,
        })
    }

    pub fn save_vote(&self, term: u64, node_id: ColdStoreNodeId) -> Result<()> {
        let mut bytes = Vec::with_capacity(16);
        bytes.extend_from_slice(&term.to_le_bytes());
        bytes.extend_from_slice(&node_id.to_le_bytes());
        self.db.put(VOTE_KEY, bytes)?;
        Ok(())
    }

    pub fn load_vote(&self) -> Result<Option<(u64, ColdStoreNodeId)>> {
        self.db
            .get(VOTE_KEY)?
            .map(|bytes| {
                anyhow::ensure!(bytes.len() == 16, "invalid raft vote record");
                let (term, node_id) = bytes.split_at(8);
                Ok((
                    u64::from_le_bytes(term.try_into()?),
                    u64::from_le_bytes(node_id.try_into()?),
                ))
            })
            .transpose()
    }

    pub fn append_log_entry(&self, index: u64, term: u64, command: &MetadataCommand) -> Result<()> {
        let mut bytes = term.to_le_bytes().to_vec();
        bytes.extend_from_slice(format!("{command:?}").as_bytes());
        self.db.put(log_key(index), bytes)?;
        Ok(())
    }

    pub fn log_entry_count(&self) -> Result<u64> {
        let count = self
            .db
            .iterator(IteratorMode::From(LOG_KEY_PREFIX, Direction::Forward))
            .take_while(|entry| match entry {
                Ok((key, _)) => key.starts_with(LOG_KEY_PREFIX),
                Err(_) => true,
            })
            .count();
        Ok(count as u64)
    }

    pub fn save_state_machine_snapshot(&self, state: &MetadataState) -> Result<()> {
        let state_machine = MetadataStateMachine::new(state.clone());
        self.db.put(SNAPSHOT_KEY, state_machine.encode_snapshot())?;
        Ok(())
    }

    pub fn load_state_machine_snapshot(&self) -> Result<Option<MetadataState>> {
        self.db
            .get(SNAPSHOT_KEY)?
            .map(|bytes| {
                MetadataStateMachine::decode_snapshot(&bytes).map(MetadataStateMachine::into_state)
            })
            .transpose()
    }
}

pub struct LocalSingleNodeMetadataRaft {
    storage: RocksDbRaftStorage,
    state_machine: Mutex<MetadataStateMachine>,
}

impl LocalSingleNodeMetadataRaft {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let storage = RocksDbRaftStorage::open(path)?;
        let state = storage
            .load_state_machine_snapshot()?
            .map(MetadataStateMachine::new)
            .unwrap_or_default();
        Ok(Self {
            storage,
            state_machine: Mutex::new(state),
        })
    }

    pub fn propose(&self, command: MetadataCommand) -> Result<()> {
        let index = self.storage.log_entry_count()? + 1;
        self.storage.append_log_entry(index, 1, &command)?;
        let mut state_machine = self
            .state_machine
            .lock()
            .expect("local single-node metadata raft state lock poisoned");
        state_machine
            .apply(command)
            .map_err(|status| anyhow::anyhow!(status.to_string()))?;
        self.storage
            .save_state_machine_snapshot(state_machine.state())?;
        Ok(())
    }

    pub fn applied_log_count(&self) -> Result<u64> {
        self.storage.log_entry_count()
    }

    pub fn snapshot(&self) -> Result<MetadataState> {
        Ok(self
            .storage
            .load_state_machine_snapshot()?
            .unwrap_or_else(|| {
                self.state_machine
                    .lock()
                    .expect("local single-node metadata raft state lock poisoned")
                    .state()
                    .clone()
            }))
    }
}

fn log_key(index: u64) -> Vec<u8> {
    let mut key = LOG_KEY_PREFIX.to_vec();
    key.extend_from_slice(&index.to_be_bytes());
    key
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

    fn test_object(bucket: &str, key: &str) -> common::ObjectMetadata {
        common::ObjectMetadata {
            bucket: bucket.into(),
            key: key.into(),
            version_id: None,
            size: 5,
            checksum: "sum".into(),
            content_type: Some("text/plain".into()),
            etag: Some("etag".into()),
            storage_class: common::StorageClass::ColdPending as i32,
            archive_id: None,
            tape_id: None,
            tape_set: vec![],
            tape_block_offset: None,
            restore_status: Some(common::RestoreStatus::RestorePending as i32),
            restore_expire_at: None,
            created_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn rocksdb_storage_separates_log_vote_and_state_machine_snapshot() {
        let dir = std::env::temp_dir().join(format!(
            "coldstore-metadata-raft-storage-{}",
            uuid::Uuid::new_v4()
        ));
        let storage = RocksDbRaftStorage::open(&dir).expect("open rocksdb raft storage");
        storage.save_vote(3, 7).expect("save vote");
        storage
            .append_log_entry(1, 3, &MetadataCommand::CreateBucket(test_bucket("docs")))
            .expect("append command log");

        let mut state = MetadataStateMachine::default();
        state
            .apply(MetadataCommand::CreateBucket(test_bucket("docs")))
            .expect("apply command");
        storage
            .save_state_machine_snapshot(state.state())
            .expect("save snapshot");

        assert_eq!(storage.load_vote().expect("load vote"), Some((3, 7)));
        assert_eq!(storage.log_entry_count().expect("count logs"), 1);
        assert!(storage
            .load_state_machine_snapshot()
            .expect("load snapshot")
            .expect("snapshot exists")
            .bucket("docs")
            .is_some());
        drop(storage);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn local_single_node_raft_reopens_state_machine_from_rocksdb_snapshot() {
        let dir = std::env::temp_dir().join(format!(
            "coldstore-metadata-single-node-raft-{}",
            uuid::Uuid::new_v4()
        ));
        let node = LocalSingleNodeMetadataRaft::open(&dir).expect("open single node raft");
        node.propose(MetadataCommand::CreateBucket(test_bucket("docs")))
            .expect("propose create bucket");
        node.propose(MetadataCommand::PutObject(test_object(
            "docs",
            "readme.txt",
        )))
        .expect("propose put object");
        assert_eq!(node.applied_log_count().expect("applied count"), 2);
        drop(node);

        let reopened = LocalSingleNodeMetadataRaft::open(&dir).expect("reopen single node raft");
        let state = reopened.snapshot().expect("snapshot");
        assert!(state.bucket("docs").is_some());
        assert!(state.objects().any(|object| object.key == "readme.txt"));
        drop(reopened);
        let _ = std::fs::remove_dir_all(dir);
    }
}
