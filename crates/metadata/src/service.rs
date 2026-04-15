use anyhow::Result;
use coldstore_common::config::MetadataConfig;
use coldstore_proto::common;
use coldstore_proto::metadata::metadata_service_server::MetadataService;
use coldstore_proto::metadata::*;
use prost_types::Timestamp;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tonic::{Request, Response, Status};

#[derive(Debug, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
struct ObjectKey {
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

#[derive(Default)]
struct MetadataState {
    objects: HashMap<ObjectKey, common::ObjectMetadata>,
    buckets: HashMap<String, common::BucketInfo>,
    archive_bundles: HashMap<String, common::ArchiveBundle>,
    archive_tasks: HashMap<String, common::ArchiveTask>,
    recall_tasks: HashMap<String, common::RecallTask>,
    tapes: HashMap<String, common::TapeInfo>,
    scheduler_workers: HashMap<u64, common::SchedulerWorkerInfo>,
    cache_workers: HashMap<u64, common::CacheWorkerInfo>,
    tape_workers: HashMap<u64, common::TapeWorkerInfo>,
}

pub struct MetadataServiceImpl {
    config: MetadataConfig,
    state: Arc<RwLock<MetadataState>>,
}

impl MetadataServiceImpl {
    pub async fn new(config: &MetadataConfig) -> Result<Self> {
        Ok(Self {
            config: config.clone(),
            state: Arc::new(RwLock::new(MetadataState::default())),
        })
    }

    fn metadata_nodes(&self) -> Vec<common::MetadataNodeInfo> {
        self.config
            .cluster
            .split(',')
            .filter_map(|entry| {
                let (node_id, addr) = entry.split_once(':')?;
                let node_id = node_id.parse::<u64>().ok()?;
                Some(common::MetadataNodeInfo {
                    node_id,
                    addr: addr.to_string(),
                    raft_role: if node_id == self.config.node_id {
                        "LeaderCandidate".into()
                    } else {
                        "Follower".into()
                    },
                    last_heartbeat: Some(now_timestamp()),
                    status: common::NodeStatus::NodeOnline as i32,
                })
            })
            .collect()
    }
}

#[tonic::async_trait]
impl MetadataService for MetadataServiceImpl {
    async fn put_object(
        &self,
        request: Request<common::ObjectMetadata>,
    ) -> std::result::Result<Response<()>, Status> {
        let mut object = request.into_inner();
        let mut state = self.state.write().await;
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
        refresh_bucket_stats(&mut state, &object.bucket);
        Ok(Response::new(()))
    }

    async fn get_object(
        &self,
        request: Request<GetObjectRequest>,
    ) -> std::result::Result<Response<common::ObjectMetadata>, Status> {
        let request = request.into_inner();
        let state = self.state.read().await;
        let object = find_object(&state, &request.bucket, &request.key, None)?;
        Ok(Response::new(object))
    }

    async fn get_object_version(
        &self,
        request: Request<GetObjectVersionRequest>,
    ) -> std::result::Result<Response<common::ObjectMetadata>, Status> {
        let request = request.into_inner();
        let state = self.state.read().await;
        let object = find_object(
            &state,
            &request.bucket,
            &request.key,
            Some(request.version_id.as_str()),
        )?;
        Ok(Response::new(object))
    }

    async fn delete_object(
        &self,
        request: Request<DeleteObjectRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        let request = request.into_inner();
        let mut state = self.state.write().await;
        let removed = state.objects.remove(&ObjectKey::new(
            request.bucket.clone(),
            request.key.clone(),
            None,
        ));
        if removed.is_none() {
            return Err(Status::not_found("object not found"));
        }
        refresh_bucket_stats(&mut state, &request.bucket);
        Ok(Response::new(()))
    }

    async fn head_object(
        &self,
        request: Request<HeadObjectRequest>,
    ) -> std::result::Result<Response<common::ObjectMetadata>, Status> {
        let request = request.into_inner();
        let state = self.state.read().await;
        let object = find_object(&state, &request.bucket, &request.key, None)?;
        Ok(Response::new(object))
    }

    async fn list_objects(
        &self,
        request: Request<ListObjectsRequest>,
    ) -> std::result::Result<Response<ListObjectsResponse>, Status> {
        let request = request.into_inner();
        let state = self.state.read().await;
        if !state.buckets.contains_key(&request.bucket) {
            return Err(Status::not_found(format!(
                "bucket not found: {}",
                request.bucket
            )));
        }

        let prefix = request.prefix.unwrap_or_default();
        let marker = request.marker.unwrap_or_default();
        let limit = if request.max_keys == 0 {
            usize::MAX
        } else {
            request.max_keys as usize
        };

        let mut objects: Vec<_> = state
            .objects
            .values()
            .filter(|object| object.bucket == request.bucket)
            .filter(|object| object.key.starts_with(&prefix))
            .filter(|object| object.key > marker)
            .cloned()
            .collect();
        objects.sort_by(|a, b| {
            a.key
                .cmp(&b.key)
                .then_with(|| a.version_id.cmp(&b.version_id))
        });

        let is_truncated = objects.len() > limit;
        let next_marker = if is_truncated {
            objects.get(limit - 1).map(|object| object.key.clone())
        } else {
            None
        };

        Ok(Response::new(ListObjectsResponse {
            objects: objects.into_iter().take(limit).collect(),
            next_marker,
            is_truncated,
        }))
    }

    async fn update_storage_class(
        &self,
        request: Request<UpdateStorageClassRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        let request = request.into_inner();
        let mut state = self.state.write().await;
        let object = find_object_mut(&mut state, &request.bucket, &request.key, None)?;
        object.storage_class = request.storage_class;
        object.updated_at = Some(now_timestamp());
        Ok(Response::new(()))
    }

    async fn update_archive_location(
        &self,
        request: Request<UpdateArchiveLocationRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        let request = request.into_inner();
        let mut state = self.state.write().await;
        let object = find_object_mut(&mut state, &request.bucket, &request.key, None)?;
        object.archive_id = Some(request.archive_id);
        object.tape_id = Some(request.tape_id);
        object.tape_set = request.tape_set;
        object.tape_block_offset = Some(request.tape_block_offset);
        object.updated_at = Some(now_timestamp());
        Ok(Response::new(()))
    }

    async fn update_restore_status(
        &self,
        request: Request<UpdateRestoreStatusRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        let request = request.into_inner();
        let mut state = self.state.write().await;
        let object = find_object_mut(&mut state, &request.bucket, &request.key, None)?;
        validate_restore_transition(object.restore_status, request.status)?;
        object.restore_status = Some(request.status);
        object.restore_expire_at = request.expire_at;
        object.updated_at = Some(now_timestamp());
        Ok(Response::new(()))
    }

    async fn scan_cold_pending(
        &self,
        request: Request<ScanColdPendingRequest>,
    ) -> std::result::Result<Response<ScanColdPendingResponse>, Status> {
        let request = request.into_inner();
        let limit = if request.limit == 0 {
            usize::MAX
        } else {
            request.limit as usize
        };
        let state = self.state.read().await;
        let mut objects: Vec<_> = state
            .objects
            .values()
            .filter(|object| object.storage_class == common::StorageClass::ColdPending as i32)
            .cloned()
            .collect();
        objects.sort_by(|a, b| a.bucket.cmp(&b.bucket).then_with(|| a.key.cmp(&b.key)));
        Ok(Response::new(ScanColdPendingResponse {
            objects: objects.into_iter().take(limit).collect(),
        }))
    }

    async fn create_bucket(
        &self,
        request: Request<common::BucketInfo>,
    ) -> std::result::Result<Response<()>, Status> {
        let mut bucket = request.into_inner();
        let mut state = self.state.write().await;
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
        Ok(Response::new(()))
    }

    async fn get_bucket(
        &self,
        request: Request<GetBucketRequest>,
    ) -> std::result::Result<Response<common::BucketInfo>, Status> {
        let request = request.into_inner();
        let state = self.state.read().await;
        let bucket = state
            .buckets
            .get(&request.name)
            .cloned()
            .ok_or_else(|| Status::not_found(format!("bucket not found: {}", request.name)))?;
        Ok(Response::new(bucket))
    }

    async fn delete_bucket(
        &self,
        request: Request<DeleteBucketRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        let request = request.into_inner();
        let mut state = self.state.write().await;
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
        Ok(Response::new(()))
    }

    async fn list_buckets(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<ListBucketsResponse>, Status> {
        let state = self.state.read().await;
        let mut buckets: Vec<_> = state.buckets.values().cloned().collect();
        buckets.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Response::new(ListBucketsResponse { buckets }))
    }

    async fn put_archive_bundle(
        &self,
        request: Request<common::ArchiveBundle>,
    ) -> std::result::Result<Response<()>, Status> {
        let mut bundle = request.into_inner();
        if bundle.created_at.is_none() {
            bundle.created_at = Some(now_timestamp());
        }
        let mut state = self.state.write().await;
        state.archive_bundles.insert(bundle.id.clone(), bundle);
        Ok(Response::new(()))
    }

    async fn get_archive_bundle(
        &self,
        request: Request<GetArchiveBundleRequest>,
    ) -> std::result::Result<Response<common::ArchiveBundle>, Status> {
        let request = request.into_inner();
        let state = self.state.read().await;
        let bundle = state
            .archive_bundles
            .get(&request.id)
            .cloned()
            .ok_or_else(|| Status::not_found("archive bundle not found"))?;
        Ok(Response::new(bundle))
    }

    async fn update_archive_bundle_status(
        &self,
        request: Request<UpdateArchiveBundleStatusRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        let request = request.into_inner();
        let mut state = self.state.write().await;
        let bundle = state
            .archive_bundles
            .get_mut(&request.id)
            .ok_or_else(|| Status::not_found("archive bundle not found"))?;
        validate_archive_bundle_transition(bundle.status, request.status)?;
        bundle.status = request.status;
        if request.status == common::ArchiveBundleStatus::BundleCompleted as i32 {
            bundle.completed_at = Some(now_timestamp());
        }
        Ok(Response::new(()))
    }

    async fn list_bundles_by_tape(
        &self,
        request: Request<ListBundlesByTapeRequest>,
    ) -> std::result::Result<Response<ListBundlesByTapeResponse>, Status> {
        let request = request.into_inner();
        let state = self.state.read().await;
        let bundle_ids = state
            .archive_bundles
            .values()
            .filter(|bundle| bundle.tape_id == request.tape_id)
            .map(|bundle| bundle.id.clone())
            .collect();
        Ok(Response::new(ListBundlesByTapeResponse { bundle_ids }))
    }

    async fn put_archive_task(
        &self,
        request: Request<common::ArchiveTask>,
    ) -> std::result::Result<Response<()>, Status> {
        let mut task = request.into_inner();
        if task.created_at.is_none() {
            task.created_at = Some(now_timestamp());
        }
        let mut state = self.state.write().await;
        state.archive_tasks.insert(task.id.clone(), task);
        Ok(Response::new(()))
    }

    async fn get_archive_task(
        &self,
        request: Request<GetArchiveTaskRequest>,
    ) -> std::result::Result<Response<common::ArchiveTask>, Status> {
        let request = request.into_inner();
        let state = self.state.read().await;
        let task = state
            .archive_tasks
            .get(&request.id)
            .cloned()
            .ok_or_else(|| Status::not_found("archive task not found"))?;
        Ok(Response::new(task))
    }

    async fn update_archive_task(
        &self,
        request: Request<common::ArchiveTask>,
    ) -> std::result::Result<Response<()>, Status> {
        let task = request.into_inner();
        let mut state = self.state.write().await;
        let current = state
            .archive_tasks
            .get(&task.id)
            .ok_or_else(|| Status::not_found("archive task not found"))?;
        validate_archive_task_transition(current.status, task.status)?;
        state.archive_tasks.insert(task.id.clone(), task);
        Ok(Response::new(()))
    }

    async fn list_pending_archive_tasks(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<ListArchiveTasksResponse>, Status> {
        let state = self.state.read().await;
        let tasks = state
            .archive_tasks
            .values()
            .filter(|task| {
                matches!(
                    common::ArchiveTaskStatus::try_from(task.status),
                    Ok(common::ArchiveTaskStatus::ArchiveTaskPending)
                        | Ok(common::ArchiveTaskStatus::ArchiveTaskInProgress)
                )
            })
            .cloned()
            .collect();
        Ok(Response::new(ListArchiveTasksResponse { tasks }))
    }

    async fn put_recall_task(
        &self,
        request: Request<common::RecallTask>,
    ) -> std::result::Result<Response<()>, Status> {
        let mut task = request.into_inner();
        if task.created_at.is_none() {
            task.created_at = Some(now_timestamp());
        }
        let mut state = self.state.write().await;
        state.recall_tasks.insert(task.id.clone(), task);
        Ok(Response::new(()))
    }

    async fn get_recall_task(
        &self,
        request: Request<GetRecallTaskRequest>,
    ) -> std::result::Result<Response<common::RecallTask>, Status> {
        let request = request.into_inner();
        let state = self.state.read().await;
        let task = state
            .recall_tasks
            .get(&request.id)
            .cloned()
            .ok_or_else(|| Status::not_found("recall task not found"))?;
        Ok(Response::new(task))
    }

    async fn update_recall_task(
        &self,
        request: Request<common::RecallTask>,
    ) -> std::result::Result<Response<()>, Status> {
        let task = request.into_inner();
        let mut state = self.state.write().await;
        let current = state
            .recall_tasks
            .get(&task.id)
            .ok_or_else(|| Status::not_found("recall task not found"))?;
        validate_restore_transition(Some(current.status), task.status)?;
        state.recall_tasks.insert(task.id.clone(), task);
        Ok(Response::new(()))
    }

    async fn list_pending_recall_tasks(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<ListRecallTasksResponse>, Status> {
        let state = self.state.read().await;
        let tasks = state
            .recall_tasks
            .values()
            .filter(|task| is_pending_restore_status(task.status))
            .cloned()
            .collect();
        Ok(Response::new(ListRecallTasksResponse { tasks }))
    }

    async fn list_recall_tasks_by_tape(
        &self,
        request: Request<ListRecallTasksByTapeRequest>,
    ) -> std::result::Result<Response<ListRecallTasksResponse>, Status> {
        let request = request.into_inner();
        let state = self.state.read().await;
        let tasks = state
            .recall_tasks
            .values()
            .filter(|task| task.tape_id == request.tape_id)
            .cloned()
            .collect();
        Ok(Response::new(ListRecallTasksResponse { tasks }))
    }

    async fn find_active_recall(
        &self,
        request: Request<FindActiveRecallRequest>,
    ) -> std::result::Result<Response<FindActiveRecallResponse>, Status> {
        let request = request.into_inner();
        let state = self.state.read().await;
        let task = state
            .recall_tasks
            .values()
            .find(|task| {
                task.bucket == request.bucket
                    && task.key == request.key
                    && is_active_restore_status(task.status)
            })
            .cloned();
        Ok(Response::new(FindActiveRecallResponse { task }))
    }

    async fn put_tape(
        &self,
        request: Request<common::TapeInfo>,
    ) -> std::result::Result<Response<()>, Status> {
        let mut tape = request.into_inner();
        if tape.registered_at.is_none() {
            tape.registered_at = Some(now_timestamp());
        }
        let mut state = self.state.write().await;
        state.tapes.insert(tape.id.clone(), tape);
        Ok(Response::new(()))
    }

    async fn get_tape(
        &self,
        request: Request<GetTapeRequest>,
    ) -> std::result::Result<Response<common::TapeInfo>, Status> {
        let request = request.into_inner();
        let state = self.state.read().await;
        let tape = state
            .tapes
            .get(&request.tape_id)
            .cloned()
            .ok_or_else(|| Status::not_found("tape not found"))?;
        Ok(Response::new(tape))
    }

    async fn update_tape(
        &self,
        request: Request<common::TapeInfo>,
    ) -> std::result::Result<Response<()>, Status> {
        let tape = request.into_inner();
        let mut state = self.state.write().await;
        state.tapes.insert(tape.id.clone(), tape);
        Ok(Response::new(()))
    }

    async fn list_tapes(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<ListTapesResponse>, Status> {
        let state = self.state.read().await;
        let tapes = state.tapes.values().cloned().collect();
        Ok(Response::new(ListTapesResponse { tapes }))
    }

    async fn list_tapes_by_status(
        &self,
        request: Request<ListTapesByStatusRequest>,
    ) -> std::result::Result<Response<ListTapesResponse>, Status> {
        let request = request.into_inner();
        let state = self.state.read().await;
        let tapes = state
            .tapes
            .values()
            .filter(|tape| tape.status == request.status)
            .cloned()
            .collect();
        Ok(Response::new(ListTapesResponse { tapes }))
    }

    async fn get_cluster_info(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<common::ClusterInfo>, Status> {
        let state = self.state.read().await;
        Ok(Response::new(common::ClusterInfo {
            cluster_id: "coldstore-phase1".into(),
            metadata_nodes: self.metadata_nodes(),
            scheduler_workers: state.scheduler_workers.values().cloned().collect(),
            cache_workers: state.cache_workers.values().cloned().collect(),
            tape_workers: state.tape_workers.values().cloned().collect(),
            leader_id: Some(self.config.node_id),
            term: 1,
            committed_index: state.objects.len() as u64,
        }))
    }

    async fn register_scheduler_worker(
        &self,
        request: Request<common::SchedulerWorkerInfo>,
    ) -> std::result::Result<Response<()>, Status> {
        let mut worker = request.into_inner();
        worker.last_heartbeat = Some(now_timestamp());
        let mut state = self.state.write().await;
        state.scheduler_workers.insert(worker.node_id, worker);
        Ok(Response::new(()))
    }

    async fn deregister_scheduler_worker(
        &self,
        request: Request<DeregisterWorkerRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        let request = request.into_inner();
        let mut state = self.state.write().await;
        state.scheduler_workers.remove(&request.node_id);
        Ok(Response::new(()))
    }

    async fn list_online_scheduler_workers(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<ListSchedulerWorkersResponse>, Status> {
        let state = self.state.read().await;
        let workers = state
            .scheduler_workers
            .values()
            .filter(|worker| worker.status == common::NodeStatus::NodeOnline as i32)
            .cloned()
            .collect();
        Ok(Response::new(ListSchedulerWorkersResponse { workers }))
    }

    async fn register_cache_worker(
        &self,
        request: Request<common::CacheWorkerInfo>,
    ) -> std::result::Result<Response<()>, Status> {
        let mut worker = request.into_inner();
        worker.last_heartbeat = Some(now_timestamp());
        let mut state = self.state.write().await;
        state.cache_workers.insert(worker.node_id, worker);
        Ok(Response::new(()))
    }

    async fn deregister_cache_worker(
        &self,
        request: Request<DeregisterWorkerRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        let request = request.into_inner();
        let mut state = self.state.write().await;
        state.cache_workers.remove(&request.node_id);
        Ok(Response::new(()))
    }

    async fn list_online_cache_workers(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<ListCacheWorkersResponse>, Status> {
        let state = self.state.read().await;
        let workers = state
            .cache_workers
            .values()
            .filter(|worker| worker.status == common::NodeStatus::NodeOnline as i32)
            .cloned()
            .collect();
        Ok(Response::new(ListCacheWorkersResponse { workers }))
    }

    async fn register_tape_worker(
        &self,
        request: Request<common::TapeWorkerInfo>,
    ) -> std::result::Result<Response<()>, Status> {
        let mut worker = request.into_inner();
        worker.last_heartbeat = Some(now_timestamp());
        let mut state = self.state.write().await;
        state.tape_workers.insert(worker.node_id, worker);
        Ok(Response::new(()))
    }

    async fn deregister_tape_worker(
        &self,
        request: Request<DeregisterWorkerRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        let request = request.into_inner();
        let mut state = self.state.write().await;
        state.tape_workers.remove(&request.node_id);
        Ok(Response::new(()))
    }

    async fn list_online_tape_workers(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<ListTapeWorkersResponse>, Status> {
        let state = self.state.read().await;
        let workers = state
            .tape_workers
            .values()
            .filter(|worker| worker.status == common::NodeStatus::NodeOnline as i32)
            .cloned()
            .collect();
        Ok(Response::new(ListTapeWorkersResponse { workers }))
    }

    async fn update_worker_status(
        &self,
        request: Request<UpdateWorkerStatusRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        let request = request.into_inner();
        let mut state = self.state.write().await;
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
        Ok(Response::new(()))
    }

    async fn drain_worker(
        &self,
        request: Request<DrainWorkerRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        let request = request.into_inner();
        self.update_worker_status(Request::new(UpdateWorkerStatusRequest {
            worker_type: request.worker_type,
            node_id: request.node_id,
            status: common::NodeStatus::NodeDraining as i32,
        }))
        .await?;
        Ok(Response::new(()))
    }

    async fn heartbeat(
        &self,
        request: Request<HeartbeatRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        let request = request.into_inner();
        let now = Some(now_timestamp());
        let mut state = self.state.write().await;

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

        Ok(Response::new(()))
    }
}

fn now_timestamp() -> Timestamp {
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
fn find_object(
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

fn is_pending_restore_status(status: i32) -> bool {
    matches!(
        common::RestoreStatus::try_from(status),
        Ok(common::RestoreStatus::RestorePending)
            | Ok(common::RestoreStatus::RestoreWaitingForMedia)
            | Ok(common::RestoreStatus::RestoreInProgress)
    )
}

fn is_active_restore_status(status: i32) -> bool {
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
    use coldstore_proto::common::{BucketInfo, ObjectMetadata};

    fn test_bucket(name: &str) -> BucketInfo {
        BucketInfo {
            name: name.into(),
            created_at: Some(Timestamp {
                seconds: 1,
                nanos: 0,
            }),
            owner: Some("tester".into()),
            versioning_enabled: false,
            object_count: 0,
            total_size: 0,
        }
    }

    fn test_object(bucket: &str, key: &str) -> ObjectMetadata {
        ObjectMetadata {
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
            created_at: Some(Timestamp {
                seconds: 1,
                nanos: 0,
            }),
            updated_at: Some(Timestamp {
                seconds: 1,
                nanos: 0,
            }),
        }
    }

    #[tokio::test]
    async fn create_and_get_bucket_round_trip() {
        let svc = MetadataServiceImpl::new(&MetadataConfig::default())
            .await
            .expect("service init");

        let bucket = test_bucket("docs");
        svc.create_bucket(Request::new(bucket.clone()))
            .await
            .expect("create bucket should succeed");

        let got = svc
            .get_bucket(Request::new(GetBucketRequest {
                name: bucket.name.clone(),
            }))
            .await
            .expect("get bucket should succeed")
            .into_inner();

        assert_eq!(got.name, bucket.name);
        assert_eq!(got.owner, bucket.owner);
    }

    #[tokio::test]
    async fn object_lifecycle_and_bucket_stats_work() {
        let svc = MetadataServiceImpl::new(&MetadataConfig::default())
            .await
            .expect("service init");
        svc.create_bucket(Request::new(test_bucket("docs")))
            .await
            .expect("create bucket");

        svc.put_object(Request::new(test_object("docs", "readme.txt")))
            .await
            .expect("put object");
        svc.update_storage_class(Request::new(UpdateStorageClassRequest {
            bucket: "docs".into(),
            key: "readme.txt".into(),
            storage_class: common::StorageClass::Cold as i32,
        }))
        .await
        .expect("update storage class");
        svc.update_restore_status(Request::new(UpdateRestoreStatusRequest {
            bucket: "docs".into(),
            key: "readme.txt".into(),
            status: common::RestoreStatus::RestoreInProgress as i32,
            expire_at: None,
        }))
        .await
        .expect("update restore status");

        let listed = svc
            .list_objects(Request::new(ListObjectsRequest {
                bucket: "docs".into(),
                prefix: None,
                marker: None,
                max_keys: 100,
            }))
            .await
            .expect("list objects")
            .into_inner();
        assert_eq!(listed.objects.len(), 1);
        assert_eq!(
            listed.objects[0].storage_class,
            common::StorageClass::Cold as i32
        );
        assert_eq!(
            listed.objects[0].restore_status,
            Some(common::RestoreStatus::RestoreInProgress as i32)
        );

        let bucket = svc
            .get_bucket(Request::new(GetBucketRequest {
                name: "docs".into(),
            }))
            .await
            .expect("get bucket")
            .into_inner();
        assert_eq!(bucket.object_count, 1);
        assert_eq!(bucket.total_size, 5);
    }

    #[tokio::test]
    async fn worker_registration_and_heartbeat_update_cluster_state() {
        let svc = MetadataServiceImpl::new(&MetadataConfig::default())
            .await
            .expect("service init");
        svc.register_scheduler_worker(Request::new(common::SchedulerWorkerInfo {
            node_id: 7,
            addr: "127.0.0.1:22001".into(),
            status: common::NodeStatus::NodeOnline as i32,
            last_heartbeat: None,
            is_active: true,
            pending_archive_tasks: 0,
            pending_recall_tasks: 0,
            active_jobs: 0,
            paired_cache_worker_id: 0,
        }))
        .await
        .expect("register worker");

        svc.heartbeat(Request::new(HeartbeatRequest {
            worker_type: common::WorkerType::WorkerScheduler as i32,
            node_id: 7,
            payload: Some(heartbeat_request::Payload::Scheduler(SchedulerHeartbeat {
                pending_archive_tasks: 3,
                pending_recall_tasks: 2,
                active_jobs: 1,
            })),
        }))
        .await
        .expect("heartbeat");

        let cluster = svc
            .get_cluster_info(Request::new(()))
            .await
            .expect("cluster info")
            .into_inner();
        assert_eq!(cluster.scheduler_workers.len(), 1);
        assert_eq!(cluster.scheduler_workers[0].pending_archive_tasks, 3);
        assert_eq!(cluster.scheduler_workers[0].pending_recall_tasks, 2);
        assert_eq!(cluster.scheduler_workers[0].active_jobs, 1);
    }
}
