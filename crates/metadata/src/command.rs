use coldstore_proto::common;
use coldstore_proto::metadata::*;

#[derive(Debug, Clone)]
pub enum MetadataCommand {
    PutObject(common::ObjectMetadata),
    DeleteObject(DeleteObjectRequest),
    UpdateStorageClass(UpdateStorageClassRequest),
    UpdateArchiveLocation(UpdateArchiveLocationRequest),
    UpdateRestoreStatus(UpdateRestoreStatusRequest),
    CreateBucket(common::BucketInfo),
    DeleteBucket(DeleteBucketRequest),
    PutArchiveBundle(common::ArchiveBundle),
    UpdateArchiveBundleStatus(UpdateArchiveBundleStatusRequest),
    PutArchiveTask(common::ArchiveTask),
    UpdateArchiveTask(common::ArchiveTask),
    PutRecallTask(common::RecallTask),
    UpdateRecallTask(common::RecallTask),
    PutTape(common::TapeInfo),
    UpdateTape(common::TapeInfo),
    RegisterSchedulerWorker(common::SchedulerWorkerInfo),
    DeregisterSchedulerWorker(DeregisterWorkerRequest),
    RegisterCacheWorker(common::CacheWorkerInfo),
    DeregisterCacheWorker(DeregisterWorkerRequest),
    RegisterTapeWorker(common::TapeWorkerInfo),
    DeregisterTapeWorker(DeregisterWorkerRequest),
    UpdateWorkerStatus(UpdateWorkerStatusRequest),
    Heartbeat(HeartbeatRequest),
}
