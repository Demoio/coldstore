use anyhow::Result;
use coldstore_proto::common;
use coldstore_proto::metadata::*;
use prost::Message;
use prost_types::Timestamp;
use std::collections::HashMap;
use std::path::Path;
use tonic::Status;

use crate::command::MetadataCommand;

#[derive(Debug, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub(crate) struct ObjectKey {
    bucket: String,
    key: String,
    version_id: Option<String>,
}

impl ObjectKey {
    fn new(bucket: String, key: String, version_id: Option<String>) -> Self {
        Self {
            bucket,
            key,
            version_id,
        }
    }
}

#[derive(Default, Clone, Debug)]
pub struct MetadataState {
    pub(crate) objects: HashMap<ObjectKey, common::ObjectMetadata>,
    pub(crate) buckets: HashMap<String, common::BucketInfo>,
    pub(crate) archive_bundles: HashMap<String, common::ArchiveBundle>,
    pub(crate) archive_tasks: HashMap<String, common::ArchiveTask>,
    pub(crate) recall_tasks: HashMap<String, common::RecallTask>,
    pub(crate) tapes: HashMap<String, common::TapeInfo>,
    pub(crate) scheduler_workers: HashMap<u64, common::SchedulerWorkerInfo>,
    pub(crate) cache_workers: HashMap<u64, common::CacheWorkerInfo>,
    pub(crate) tape_workers: HashMap<u64, common::TapeWorkerInfo>,
}

impl MetadataState {
    pub fn bucket(&self, name: &str) -> Option<&common::BucketInfo> {
        self.buckets.get(name)
    }

    pub fn objects(&self) -> impl Iterator<Item = &common::ObjectMetadata> {
        self.objects.values()
    }

    pub fn bucket_count(&self) -> usize {
        self.buckets.len()
    }

    pub fn object_count(&self) -> usize {
        self.objects.len()
    }
}

#[derive(Default, Clone, Debug)]
pub struct MetadataStateMachine {
    state: MetadataState,
}

impl MetadataStateMachine {
    pub fn new(state: MetadataState) -> Self {
        Self { state }
    }

    #[allow(clippy::result_large_err)]
    pub fn apply(&mut self, command: MetadataCommand) -> std::result::Result<(), Status> {
        apply_command(&mut self.state, command)
    }

    pub fn state(&self) -> &MetadataState {
        &self.state
    }

    pub fn into_state(self) -> MetadataState {
        self.state
    }

    pub fn encode_snapshot(&self) -> Vec<u8> {
        encode_snapshot(&self.state)
    }

    pub fn decode_snapshot(bytes: &[u8]) -> Result<Self> {
        decode_snapshot(bytes).map(Self::new)
    }
}

#[allow(clippy::result_large_err)]
pub(crate) fn apply_command(
    state: &mut MetadataState,
    command: MetadataCommand,
) -> std::result::Result<(), Status> {
    match command {
        MetadataCommand::PutObject(mut object) => {
            if !state.buckets.contains_key(&object.bucket) {
                return Err(Status::not_found(format!(
                    "bucket not found: {}",
                    object.bucket
                )));
            }
            let now = now_timestamp();
            if object.created_at.is_none() {
                object.created_at = Some(now);
            }
            object.updated_at = Some(now);
            let key = ObjectKey::new(
                object.bucket.clone(),
                object.key.clone(),
                object.version_id.clone(),
            );
            state.objects.insert(key, object.clone());
            refresh_bucket_stats(state, &object.bucket);
        }
        MetadataCommand::DeleteObject(request) => {
            let removed = state.objects.remove(&ObjectKey::new(
                request.bucket.clone(),
                request.key.clone(),
                None,
            ));
            if removed.is_none() {
                return Err(Status::not_found("object not found"));
            }
            refresh_bucket_stats(state, &request.bucket);
        }
        MetadataCommand::UpdateStorageClass(request) => {
            let object = find_object_mut(state, &request.bucket, &request.key, None)?;
            object.storage_class = request.storage_class;
            object.updated_at = Some(now_timestamp());
        }
        MetadataCommand::UpdateArchiveLocation(request) => {
            let object = find_object_mut(state, &request.bucket, &request.key, None)?;
            object.archive_id = Some(request.archive_id);
            object.tape_id = Some(request.tape_id);
            object.tape_set = request.tape_set;
            object.tape_block_offset = Some(request.tape_block_offset);
            object.updated_at = Some(now_timestamp());
        }
        MetadataCommand::UpdateRestoreStatus(request) => {
            let object = find_object_mut(state, &request.bucket, &request.key, None)?;
            validate_restore_transition(object.restore_status, request.status)?;
            object.restore_status = Some(request.status);
            object.restore_expire_at = request.expire_at;
            object.updated_at = Some(now_timestamp());
        }
        MetadataCommand::CreateBucket(mut bucket) => {
            if state.buckets.contains_key(&bucket.name) {
                return Err(Status::already_exists(format!(
                    "bucket already exists: {}",
                    bucket.name
                )));
            }
            if bucket.created_at.is_none() {
                bucket.created_at = Some(now_timestamp());
            }
            state.buckets.insert(bucket.name.clone(), bucket);
        }
        MetadataCommand::DeleteBucket(request) => {
            let has_objects = state
                .objects
                .values()
                .any(|object| object.bucket == request.name);
            if has_objects {
                return Err(Status::failed_precondition("bucket is not empty"));
            }
            state
                .buckets
                .remove(&request.name)
                .ok_or_else(|| Status::not_found(format!("bucket not found: {}", request.name)))?;
        }
        MetadataCommand::PutArchiveBundle(mut bundle) => {
            if bundle.created_at.is_none() {
                bundle.created_at = Some(now_timestamp());
            }
            state.archive_bundles.insert(bundle.id.clone(), bundle);
        }
        MetadataCommand::UpdateArchiveBundleStatus(request) => {
            let bundle = state
                .archive_bundles
                .get_mut(&request.id)
                .ok_or_else(|| Status::not_found("archive bundle not found"))?;
            validate_archive_bundle_transition(bundle.status, request.status)?;
            bundle.status = request.status;
            if request.status == common::ArchiveBundleStatus::BundleCompleted as i32 {
                bundle.completed_at = Some(now_timestamp());
            }
        }
        MetadataCommand::PutArchiveTask(mut task) => {
            if task.created_at.is_none() {
                task.created_at = Some(now_timestamp());
            }
            state.archive_tasks.insert(task.id.clone(), task);
        }
        MetadataCommand::UpdateArchiveTask(task) => {
            let current = state
                .archive_tasks
                .get(&task.id)
                .ok_or_else(|| Status::not_found("archive task not found"))?;
            validate_archive_task_transition(current.status, task.status)?;
            state.archive_tasks.insert(task.id.clone(), task);
        }
        MetadataCommand::PutRecallTask(mut task) => {
            if task.created_at.is_none() {
                task.created_at = Some(now_timestamp());
            }
            state.recall_tasks.insert(task.id.clone(), task);
        }
        MetadataCommand::UpdateRecallTask(task) => {
            let current = state
                .recall_tasks
                .get(&task.id)
                .ok_or_else(|| Status::not_found("recall task not found"))?;
            validate_restore_transition(Some(current.status), task.status)?;
            state.recall_tasks.insert(task.id.clone(), task);
        }
        MetadataCommand::PutTape(mut tape) => {
            if tape.registered_at.is_none() {
                tape.registered_at = Some(now_timestamp());
            }
            state.tapes.insert(tape.id.clone(), tape);
        }
        MetadataCommand::UpdateTape(tape) => {
            state.tapes.insert(tape.id.clone(), tape);
        }
        MetadataCommand::RegisterSchedulerWorker(mut worker) => {
            worker.last_heartbeat = Some(now_timestamp());
            state.scheduler_workers.insert(worker.node_id, worker);
        }
        MetadataCommand::DeregisterSchedulerWorker(request) => {
            state.scheduler_workers.remove(&request.node_id);
        }
        MetadataCommand::RegisterCacheWorker(mut worker) => {
            worker.last_heartbeat = Some(now_timestamp());
            state.cache_workers.insert(worker.node_id, worker);
        }
        MetadataCommand::DeregisterCacheWorker(request) => {
            state.cache_workers.remove(&request.node_id);
        }
        MetadataCommand::RegisterTapeWorker(mut worker) => {
            worker.last_heartbeat = Some(now_timestamp());
            state.tape_workers.insert(worker.node_id, worker);
        }
        MetadataCommand::DeregisterTapeWorker(request) => {
            state.tape_workers.remove(&request.node_id);
        }
        MetadataCommand::UpdateWorkerStatus(request) => {
            match common::WorkerType::try_from(request.worker_type) {
                Ok(common::WorkerType::WorkerScheduler) => state
                    .scheduler_workers
                    .get_mut(&request.node_id)
                    .map(|worker| worker.status = request.status),
                Ok(common::WorkerType::WorkerCache) => state
                    .cache_workers
                    .get_mut(&request.node_id)
                    .map(|worker| worker.status = request.status),
                Ok(common::WorkerType::WorkerTape) => state
                    .tape_workers
                    .get_mut(&request.node_id)
                    .map(|worker| worker.status = request.status),
                _ => None,
            }
            .ok_or_else(|| Status::not_found("worker not found"))?;
        }
        MetadataCommand::Heartbeat(request) => {
            let now = Some(now_timestamp());
            match common::WorkerType::try_from(request.worker_type) {
                Ok(common::WorkerType::WorkerScheduler) => {
                    let worker = state
                        .scheduler_workers
                        .get_mut(&request.node_id)
                        .ok_or_else(|| Status::not_found("scheduler worker not found"))?;
                    worker.last_heartbeat = now;
                    if let Some(heartbeat_request::Payload::Scheduler(payload)) = request.payload {
                        worker.pending_archive_tasks = payload.pending_archive_tasks;
                        worker.pending_recall_tasks = payload.pending_recall_tasks;
                        worker.active_jobs = payload.active_jobs;
                    }
                }
                Ok(common::WorkerType::WorkerCache) => {
                    let worker = state
                        .cache_workers
                        .get_mut(&request.node_id)
                        .ok_or_else(|| Status::not_found("cache worker not found"))?;
                    worker.last_heartbeat = now;
                    if let Some(heartbeat_request::Payload::Cache(payload)) = request.payload {
                        worker.used_capacity = payload.used_capacity;
                        worker.blob_count = payload.blob_count;
                    }
                }
                Ok(common::WorkerType::WorkerTape) => {
                    let worker = state
                        .tape_workers
                        .get_mut(&request.node_id)
                        .ok_or_else(|| Status::not_found("tape worker not found"))?;
                    worker.last_heartbeat = now;
                    if let Some(heartbeat_request::Payload::Tape(payload)) = request.payload {
                        worker.drives = payload.drives;
                    }
                }
                _ => return Err(Status::invalid_argument("unknown worker type")),
            }
        }
    }

    Ok(())
}
const SNAPSHOT_MAGIC: &[u8] = b"COLDMETA2\n";
const MAX_SNAPSHOT_MESSAGES_PER_SECTION: u64 = 1_000_000;
const MAX_SNAPSHOT_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

pub(crate) async fn load_snapshot(path: &Path) -> Result<MetadataState> {
    let bytes = tokio::fs::read(path).await?;
    decode_snapshot(&bytes)
}

pub(crate) async fn save_snapshot(path: &Path, state: &MetadataState) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp_path = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    tokio::fs::write(&tmp_path, encode_snapshot(state)).await?;
    tokio::fs::rename(&tmp_path, path).await?;
    Ok(())
}

pub(crate) fn encode_snapshot(state: &MetadataState) -> Vec<u8> {
    let mut out = SNAPSHOT_MAGIC.to_vec();
    write_messages(&mut out, state.objects.values());
    write_messages(&mut out, state.buckets.values());
    write_messages(&mut out, state.archive_bundles.values());
    write_messages(&mut out, state.archive_tasks.values());
    write_messages(&mut out, state.recall_tasks.values());
    write_messages(&mut out, state.tapes.values());
    write_messages(&mut out, state.scheduler_workers.values());
    write_messages(&mut out, state.cache_workers.values());
    write_messages(&mut out, state.tape_workers.values());
    out
}

pub(crate) fn decode_snapshot(bytes: &[u8]) -> Result<MetadataState> {
    anyhow::ensure!(
        bytes.starts_with(SNAPSHOT_MAGIC),
        "invalid metadata snapshot magic"
    );
    let mut cursor = &bytes[SNAPSHOT_MAGIC.len()..];
    let objects = read_messages::<common::ObjectMetadata>(&mut cursor)?;
    let buckets = read_messages::<common::BucketInfo>(&mut cursor)?;
    let archive_bundles = read_messages::<common::ArchiveBundle>(&mut cursor)?;
    let archive_tasks = read_messages::<common::ArchiveTask>(&mut cursor)?;
    let recall_tasks = read_messages::<common::RecallTask>(&mut cursor)?;
    let tapes = read_messages::<common::TapeInfo>(&mut cursor)?;
    let scheduler_workers = read_messages::<common::SchedulerWorkerInfo>(&mut cursor)?;
    let cache_workers = read_messages::<common::CacheWorkerInfo>(&mut cursor)?;
    let tape_workers = read_messages::<common::TapeWorkerInfo>(&mut cursor)?;
    anyhow::ensure!(cursor.is_empty(), "trailing bytes in metadata snapshot");

    Ok(MetadataState {
        objects: objects
            .into_iter()
            .map(|object| {
                (
                    ObjectKey::new(
                        object.bucket.clone(),
                        object.key.clone(),
                        object.version_id.clone(),
                    ),
                    object,
                )
            })
            .collect(),
        buckets: buckets
            .into_iter()
            .map(|bucket| (bucket.name.clone(), bucket))
            .collect(),
        archive_bundles: archive_bundles
            .into_iter()
            .map(|bundle| (bundle.id.clone(), bundle))
            .collect(),
        archive_tasks: archive_tasks
            .into_iter()
            .map(|task| (task.id.clone(), task))
            .collect(),
        recall_tasks: recall_tasks
            .into_iter()
            .map(|task| (task.id.clone(), task))
            .collect(),
        tapes: tapes
            .into_iter()
            .map(|tape| (tape.id.clone(), tape))
            .collect(),
        scheduler_workers: scheduler_workers
            .into_iter()
            .map(|worker| (worker.node_id, worker))
            .collect(),
        cache_workers: cache_workers
            .into_iter()
            .map(|worker| (worker.node_id, worker))
            .collect(),
        tape_workers: tape_workers
            .into_iter()
            .map(|worker| (worker.node_id, worker))
            .collect(),
    })
}

fn write_messages<'a, M, I>(out: &mut Vec<u8>, messages: I)
where
    M: Message + 'a,
    I: IntoIterator<Item = &'a M>,
{
    let encoded: Vec<Vec<u8>> = messages
        .into_iter()
        .map(|message| message.encode_to_vec())
        .collect();
    out.extend_from_slice(&(encoded.len() as u64).to_le_bytes());
    for message in encoded {
        out.extend_from_slice(&(message.len() as u64).to_le_bytes());
        out.extend_from_slice(&message);
    }
}

fn read_messages<M>(cursor: &mut &[u8]) -> Result<Vec<M>>
where
    M: Message + Default,
{
    let count = read_u64(cursor)?;
    anyhow::ensure!(
        count <= MAX_SNAPSHOT_MESSAGES_PER_SECTION,
        "metadata snapshot section contains too many messages"
    );
    let mut messages = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let len = read_u64(cursor)? as usize;
        anyhow::ensure!(
            len <= MAX_SNAPSHOT_MESSAGE_BYTES,
            "metadata snapshot message is too large"
        );
        anyhow::ensure!(cursor.len() >= len, "truncated metadata snapshot message");
        let (message, rest) = cursor.split_at(len);
        messages.push(M::decode(message)?);
        *cursor = rest;
    }
    Ok(messages)
}

fn read_u64(cursor: &mut &[u8]) -> Result<u64> {
    anyhow::ensure!(cursor.len() >= 8, "truncated metadata snapshot header");
    let (bytes, rest) = cursor.split_at(8);
    *cursor = rest;
    Ok(u64::from_le_bytes(bytes.try_into()?))
}
pub(crate) fn now_timestamp() -> Timestamp {
    Timestamp {
        seconds: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_secs() as i64,
        nanos: 0,
    }
}

fn refresh_bucket_stats(state: &mut MetadataState, bucket_name: &str) {
    if let Some(bucket) = state.buckets.get_mut(bucket_name) {
        let mut object_count = 0_u64;
        let mut total_size = 0_u64;
        for object in state
            .objects
            .values()
            .filter(|object| object.bucket == bucket_name)
        {
            object_count += 1;
            total_size += object.size;
        }
        bucket.object_count = object_count;
        bucket.total_size = total_size;
    }
}

#[allow(clippy::result_large_err)]
pub(crate) fn find_object(
    state: &MetadataState,
    bucket: &str,
    key: &str,
    version_id: Option<&str>,
) -> Result<common::ObjectMetadata, Status> {
    if let Some(version_id) = version_id {
        state
            .objects
            .get(&ObjectKey::new(
                bucket.to_string(),
                key.to_string(),
                Some(version_id.to_string()),
            ))
            .cloned()
            .ok_or_else(|| Status::not_found("object version not found"))
    } else {
        state
            .objects
            .iter()
            .filter(|(candidate, _)| candidate.bucket == bucket && candidate.key == key)
            .max_by(|(_, left), (_, right)| {
                timestamp_sort_key(&left.updated_at).cmp(&timestamp_sort_key(&right.updated_at))
            })
            .map(|(_, object)| object.clone())
            .ok_or_else(|| Status::not_found("object not found"))
    }
}

#[allow(clippy::result_large_err)]
fn find_object_mut<'a>(
    state: &'a mut MetadataState,
    bucket: &str,
    key: &str,
    version_id: Option<&str>,
) -> Result<&'a mut common::ObjectMetadata, Status> {
    if let Some(version_id) = version_id {
        state
            .objects
            .get_mut(&ObjectKey::new(
                bucket.to_string(),
                key.to_string(),
                Some(version_id.to_string()),
            ))
            .ok_or_else(|| Status::not_found("object version not found"))
    } else {
        let selected = state
            .objects
            .keys()
            .filter(|candidate| candidate.bucket == bucket && candidate.key == key)
            .max_by(|left, right| {
                let left_ts = state
                    .objects
                    .get(*left)
                    .map(|obj| timestamp_sort_key(&obj.updated_at))
                    .unwrap_or_default();
                let right_ts = state
                    .objects
                    .get(*right)
                    .map(|obj| timestamp_sort_key(&obj.updated_at))
                    .unwrap_or_default();
                left_ts.cmp(&right_ts)
            })
            .cloned()
            .ok_or_else(|| Status::not_found("object not found"))?;
        state
            .objects
            .get_mut(&selected)
            .ok_or_else(|| Status::not_found("object not found"))
    }
}

fn timestamp_sort_key(ts: &Option<Timestamp>) -> (i64, i32) {
    ts.as_ref()
        .map(|ts| (ts.seconds, ts.nanos))
        .unwrap_or_default()
}

#[allow(clippy::result_large_err)]
fn validate_restore_transition(current: Option<i32>, next: i32) -> Result<(), Status> {
    let current = current.and_then(|value| common::RestoreStatus::try_from(value).ok());
    let next = common::RestoreStatus::try_from(next)
        .map_err(|_| Status::invalid_argument("invalid restore status"))?;
    let valid = match current {
        None => true,
        Some(common::RestoreStatus::RestorePending) => matches!(
            next,
            common::RestoreStatus::RestoreInProgress
                | common::RestoreStatus::RestoreWaitingForMedia
                | common::RestoreStatus::RestoreFailed
        ),
        Some(common::RestoreStatus::RestoreWaitingForMedia) => matches!(
            next,
            common::RestoreStatus::RestorePending | common::RestoreStatus::RestoreFailed
        ),
        Some(common::RestoreStatus::RestoreInProgress) => {
            matches!(
                next,
                common::RestoreStatus::RestoreCompleted | common::RestoreStatus::RestoreFailed
            )
        }
        Some(common::RestoreStatus::RestoreCompleted) => {
            matches!(next, common::RestoreStatus::RestoreExpired)
        }
        Some(common::RestoreStatus::RestoreExpired | common::RestoreStatus::RestoreFailed) => false,
        Some(common::RestoreStatus::Unspecified) => true,
    };

    if valid {
        Ok(())
    } else {
        Err(Status::failed_precondition(
            "invalid restore state transition",
        ))
    }
}

#[allow(clippy::result_large_err)]
fn validate_archive_bundle_transition(current: i32, next: i32) -> Result<(), Status> {
    let current = common::ArchiveBundleStatus::try_from(current)
        .map_err(|_| Status::invalid_argument("invalid archive bundle status"))?;
    let next = common::ArchiveBundleStatus::try_from(next)
        .map_err(|_| Status::invalid_argument("invalid archive bundle status"))?;
    let valid = match current {
        common::ArchiveBundleStatus::BundlePending => {
            matches!(next, common::ArchiveBundleStatus::BundleWriting)
        }
        common::ArchiveBundleStatus::BundleWriting => matches!(
            next,
            common::ArchiveBundleStatus::BundleCompleted
                | common::ArchiveBundleStatus::BundleFailed
        ),
        common::ArchiveBundleStatus::BundleFailed => {
            matches!(next, common::ArchiveBundleStatus::BundlePending)
        }
        common::ArchiveBundleStatus::BundleCompleted => false,
        common::ArchiveBundleStatus::Unspecified => true,
    };
    if valid {
        Ok(())
    } else {
        Err(Status::failed_precondition(
            "invalid archive bundle state transition",
        ))
    }
}

#[allow(clippy::result_large_err)]
fn validate_archive_task_transition(current: i32, next: i32) -> Result<(), Status> {
    let current = common::ArchiveTaskStatus::try_from(current)
        .map_err(|_| Status::invalid_argument("invalid archive task status"))?;
    let next = common::ArchiveTaskStatus::try_from(next)
        .map_err(|_| Status::invalid_argument("invalid archive task status"))?;
    let valid = match current {
        common::ArchiveTaskStatus::ArchiveTaskPending => {
            matches!(next, common::ArchiveTaskStatus::ArchiveTaskInProgress)
        }
        common::ArchiveTaskStatus::ArchiveTaskInProgress => matches!(
            next,
            common::ArchiveTaskStatus::ArchiveTaskCompleted
                | common::ArchiveTaskStatus::ArchiveTaskFailed
        ),
        common::ArchiveTaskStatus::ArchiveTaskFailed => {
            matches!(next, common::ArchiveTaskStatus::ArchiveTaskPending)
        }
        common::ArchiveTaskStatus::ArchiveTaskCompleted => false,
        common::ArchiveTaskStatus::Unspecified => true,
    };
    if valid {
        Ok(())
    } else {
        Err(Status::failed_precondition(
            "invalid archive task state transition",
        ))
    }
}

pub(crate) fn is_pending_restore_status(status: i32) -> bool {
    matches!(
        common::RestoreStatus::try_from(status),
        Ok(common::RestoreStatus::RestorePending)
            | Ok(common::RestoreStatus::RestoreWaitingForMedia)
            | Ok(common::RestoreStatus::RestoreInProgress)
    )
}

pub(crate) fn is_active_restore_status(status: i32) -> bool {
    matches!(
        common::RestoreStatus::try_from(status),
        Ok(common::RestoreStatus::RestorePending)
            | Ok(common::RestoreStatus::RestoreWaitingForMedia)
            | Ok(common::RestoreStatus::RestoreInProgress)
            | Ok(common::RestoreStatus::RestoreCompleted)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_snapshot_rejects_excessive_section_count_before_allocating() {
        let mut bytes = SNAPSHOT_MAGIC.to_vec();
        bytes.extend_from_slice(&(MAX_SNAPSHOT_MESSAGES_PER_SECTION + 1).to_le_bytes());

        let err = MetadataStateMachine::decode_snapshot(&bytes).expect_err("oversized count fails");
        assert!(err
            .to_string()
            .contains("metadata snapshot section contains too many messages"));
    }

    #[test]
    fn decode_snapshot_rejects_excessive_message_size() {
        let mut bytes = SNAPSHOT_MAGIC.to_vec();
        bytes.extend_from_slice(&1_u64.to_le_bytes());
        bytes.extend_from_slice(&((MAX_SNAPSHOT_MESSAGE_BYTES as u64) + 1).to_le_bytes());

        let err =
            MetadataStateMachine::decode_snapshot(&bytes).expect_err("oversized message fails");
        assert!(err
            .to_string()
            .contains("metadata snapshot message is too large"));
    }
}
