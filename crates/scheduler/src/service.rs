use crate::SchedulerState;
use coldstore_proto::scheduler::scheduler_service_server::SchedulerService;
use coldstore_proto::scheduler::*;
use std::sync::Arc;
use tonic::{Request, Response, Status, Streaming};

pub struct SchedulerServiceImpl {
    _state: Arc<SchedulerState>,
}

impl SchedulerServiceImpl {
    pub fn new(state: Arc<SchedulerState>) -> Self {
        Self { _state: state }
    }
}

fn phase1_unimplemented(op: &str) -> Status {
    Status::unimplemented(format!(
        "{op} is not implemented in phase-1 safe mode; use unit-tested metadata/cache services only"
    ))
}

#[tonic::async_trait]
impl SchedulerService for SchedulerServiceImpl {
    async fn put_object(
        &self,
        _request: Request<Streaming<PutObjectRequest>>,
    ) -> std::result::Result<Response<PutObjectResponse>, Status> {
        Err(phase1_unimplemented("scheduler.put_object"))
    }

    type GetObjectStream =
        tokio_stream::wrappers::ReceiverStream<Result<GetObjectResponse, Status>>;

    async fn get_object(
        &self,
        _request: Request<GetObjectRequest>,
    ) -> std::result::Result<Response<Self::GetObjectStream>, Status> {
        Err(phase1_unimplemented("scheduler.get_object"))
    }

    async fn head_object(
        &self,
        _request: Request<HeadObjectRequest>,
    ) -> std::result::Result<Response<HeadObjectResponse>, Status> {
        Err(phase1_unimplemented("scheduler.head_object"))
    }

    async fn delete_object(
        &self,
        _request: Request<DeleteObjectRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        Err(phase1_unimplemented("scheduler.delete_object"))
    }

    async fn restore_object(
        &self,
        _request: Request<RestoreObjectRequest>,
    ) -> std::result::Result<Response<RestoreObjectResponse>, Status> {
        Err(phase1_unimplemented("scheduler.restore_object"))
    }

    async fn list_objects(
        &self,
        _request: Request<ListObjectsRequest>,
    ) -> std::result::Result<Response<ListObjectsResponse>, Status> {
        Err(phase1_unimplemented("scheduler.list_objects"))
    }

    async fn create_bucket(
        &self,
        _request: Request<CreateBucketRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        Err(phase1_unimplemented("scheduler.create_bucket"))
    }

    async fn delete_bucket(
        &self,
        _request: Request<DeleteBucketRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        Err(phase1_unimplemented("scheduler.delete_bucket"))
    }

    async fn head_bucket(
        &self,
        _request: Request<HeadBucketRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        Err(phase1_unimplemented("scheduler.head_bucket"))
    }

    async fn list_buckets(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<ListBucketsResponse>, Status> {
        Err(phase1_unimplemented("scheduler.list_buckets"))
    }
}
