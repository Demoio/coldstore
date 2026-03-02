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

#[tonic::async_trait]
impl SchedulerService for SchedulerServiceImpl {
    async fn put_object(
        &self,
        _request: Request<Streaming<PutObjectRequest>>,
    ) -> std::result::Result<Response<PutObjectResponse>, Status> {
        // 1. 从 stream 接收元数据和数据
        // 2. 写入 Metadata (PutObject, storage_class = ColdPending)
        // 3. 将数据暂存到 Cache Worker (PutStaging)
        // 4. 返回 ETag
        todo!()
    }

    type GetObjectStream =
        tokio_stream::wrappers::ReceiverStream<Result<GetObjectResponse, Status>>;

    async fn get_object(
        &self,
        _request: Request<GetObjectRequest>,
    ) -> std::result::Result<Response<Self::GetObjectStream>, Status> {
        // 1. 查询 Metadata (HeadObject)
        // 2. 检查 storage_class 和 restore_status
        // 3. 若 Cold + Completed: 从 Cache Worker 读取数据
        // 4. 若 Cold + 未解冻: 返回 InvalidObjectState
        // 5. Stream 返回数据
        todo!()
    }

    async fn head_object(
        &self,
        _request: Request<HeadObjectRequest>,
    ) -> std::result::Result<Response<HeadObjectResponse>, Status> {
        // 查询 Metadata, 生成 x-amz-restore 头信息
        todo!()
    }

    async fn delete_object(
        &self,
        _request: Request<DeleteObjectRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        // 1. 删除 Metadata 中的对象
        // 2. 清理 Cache Worker 中的缓存/暂存数据
        todo!()
    }

    async fn restore_object(
        &self,
        _request: Request<RestoreObjectRequest>,
    ) -> std::result::Result<Response<RestoreObjectResponse>, Status> {
        // 1. 检查对象是否为 Cold 状态
        // 2. 线性读检查是否已有进行中的 Restore
        // 3. 创建 RecallTask 写入 Metadata
        // 4. 返回 202 Accepted
        todo!()
    }

    async fn list_objects(
        &self,
        _request: Request<ListObjectsRequest>,
    ) -> std::result::Result<Response<ListObjectsResponse>, Status> {
        todo!()
    }

    async fn create_bucket(
        &self,
        _request: Request<CreateBucketRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    async fn delete_bucket(
        &self,
        _request: Request<DeleteBucketRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    async fn head_bucket(
        &self,
        _request: Request<HeadBucketRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        todo!()
    }

    async fn list_buckets(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<ListBucketsResponse>, Status> {
        todo!()
    }
}
