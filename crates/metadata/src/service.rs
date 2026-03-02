use anyhow::Result;
use coldstore_common::config::MetadataConfig;
use coldstore_proto::common;
use coldstore_proto::metadata::metadata_service_server::MetadataService;
use coldstore_proto::metadata::*;
use tonic::{Request, Response, Status};

pub struct MetadataServiceImpl {
    _config: MetadataConfig,
    // TODO: Phase 2 — Raft + RocksDB
    // raft: Arc<Raft<ColdStoreTypeConfig>>,
    // store: Arc<RocksStore>,
}

impl MetadataServiceImpl {
    pub async fn new(config: &MetadataConfig) -> Result<Self> {
        // TODO: 初始化 Raft 集群和 RocksDB
        Ok(Self {
            _config: config.clone(),
        })
    }
}

#[tonic::async_trait]
impl MetadataService for MetadataServiceImpl {
    // ── ObjectApi ──

    async fn put_object(
        &self,
        _request: Request<common::ObjectMetadata>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    async fn get_object(
        &self,
        _request: Request<GetObjectRequest>,
    ) -> std::result::Result<Response<common::ObjectMetadata>, Status> {
        todo!()
    }

    async fn get_object_version(
        &self,
        _request: Request<GetObjectVersionRequest>,
    ) -> std::result::Result<Response<common::ObjectMetadata>, Status> {
        todo!()
    }

    async fn delete_object(
        &self,
        _request: Request<DeleteObjectRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    async fn head_object(
        &self,
        _request: Request<HeadObjectRequest>,
    ) -> std::result::Result<Response<common::ObjectMetadata>, Status> {
        todo!()
    }

    async fn list_objects(
        &self,
        _request: Request<ListObjectsRequest>,
    ) -> std::result::Result<Response<ListObjectsResponse>, Status> {
        todo!()
    }

    async fn update_storage_class(
        &self,
        _request: Request<UpdateStorageClassRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    async fn update_archive_location(
        &self,
        _request: Request<UpdateArchiveLocationRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    async fn update_restore_status(
        &self,
        _request: Request<UpdateRestoreStatusRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    async fn scan_cold_pending(
        &self,
        _request: Request<ScanColdPendingRequest>,
    ) -> std::result::Result<Response<ScanColdPendingResponse>, Status> {
        todo!()
    }

    // ── BucketApi ──

    async fn create_bucket(
        &self,
        _request: Request<common::BucketInfo>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    async fn get_bucket(
        &self,
        _request: Request<GetBucketRequest>,
    ) -> std::result::Result<Response<common::BucketInfo>, Status> {
        todo!()
    }

    async fn delete_bucket(
        &self,
        _request: Request<DeleteBucketRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    async fn list_buckets(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<ListBucketsResponse>, Status> {
        todo!()
    }

    // ── ArchiveApi ──

    async fn put_archive_bundle(
        &self,
        _request: Request<common::ArchiveBundle>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    async fn get_archive_bundle(
        &self,
        _request: Request<GetArchiveBundleRequest>,
    ) -> std::result::Result<Response<common::ArchiveBundle>, Status> {
        todo!()
    }

    async fn update_archive_bundle_status(
        &self,
        _request: Request<UpdateArchiveBundleStatusRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    async fn list_bundles_by_tape(
        &self,
        _request: Request<ListBundlesByTapeRequest>,
    ) -> std::result::Result<Response<ListBundlesByTapeResponse>, Status> {
        todo!()
    }

    async fn put_archive_task(
        &self,
        _request: Request<common::ArchiveTask>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    async fn get_archive_task(
        &self,
        _request: Request<GetArchiveTaskRequest>,
    ) -> std::result::Result<Response<common::ArchiveTask>, Status> {
        todo!()
    }

    async fn update_archive_task(
        &self,
        _request: Request<common::ArchiveTask>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    async fn list_pending_archive_tasks(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<ListArchiveTasksResponse>, Status> {
        todo!()
    }

    // ── RecallApi ──

    async fn put_recall_task(
        &self,
        _request: Request<common::RecallTask>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    async fn get_recall_task(
        &self,
        _request: Request<GetRecallTaskRequest>,
    ) -> std::result::Result<Response<common::RecallTask>, Status> {
        todo!()
    }

    async fn update_recall_task(
        &self,
        _request: Request<common::RecallTask>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    async fn list_pending_recall_tasks(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<ListRecallTasksResponse>, Status> {
        todo!()
    }

    async fn list_recall_tasks_by_tape(
        &self,
        _request: Request<ListRecallTasksByTapeRequest>,
    ) -> std::result::Result<Response<ListRecallTasksResponse>, Status> {
        todo!()
    }

    async fn find_active_recall(
        &self,
        _request: Request<FindActiveRecallRequest>,
    ) -> std::result::Result<Response<FindActiveRecallResponse>, Status> {
        todo!()
    }

    // ── TapeApi ──

    async fn put_tape(
        &self,
        _request: Request<common::TapeInfo>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    async fn get_tape(
        &self,
        _request: Request<GetTapeRequest>,
    ) -> std::result::Result<Response<common::TapeInfo>, Status> {
        todo!()
    }

    async fn update_tape(
        &self,
        _request: Request<common::TapeInfo>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    async fn list_tapes(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<ListTapesResponse>, Status> {
        todo!()
    }

    async fn list_tapes_by_status(
        &self,
        _request: Request<ListTapesByStatusRequest>,
    ) -> std::result::Result<Response<ListTapesResponse>, Status> {
        todo!()
    }

    // ── ClusterApi ──

    async fn get_cluster_info(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<common::ClusterInfo>, Status> {
        todo!()
    }

    async fn register_scheduler_worker(
        &self,
        _request: Request<common::SchedulerWorkerInfo>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    async fn deregister_scheduler_worker(
        &self,
        _request: Request<DeregisterWorkerRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    async fn list_online_scheduler_workers(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<ListSchedulerWorkersResponse>, Status> {
        todo!()
    }

    async fn register_cache_worker(
        &self,
        _request: Request<common::CacheWorkerInfo>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    async fn deregister_cache_worker(
        &self,
        _request: Request<DeregisterWorkerRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    async fn list_online_cache_workers(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<ListCacheWorkersResponse>, Status> {
        todo!()
    }

    async fn register_tape_worker(
        &self,
        _request: Request<common::TapeWorkerInfo>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    async fn deregister_tape_worker(
        &self,
        _request: Request<DeregisterWorkerRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    async fn list_online_tape_workers(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<ListTapeWorkersResponse>, Status> {
        todo!()
    }

    async fn update_worker_status(
        &self,
        _request: Request<UpdateWorkerStatusRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    async fn drain_worker(
        &self,
        _request: Request<DrainWorkerRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    // ── HeartbeatApi ──

    async fn heartbeat(
        &self,
        _request: Request<HeartbeatRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }
}
