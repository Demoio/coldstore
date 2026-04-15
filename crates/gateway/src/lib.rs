pub mod handler;
pub mod protocol;

use anyhow::Result;
use coldstore_common::config::GatewayConfig;
use coldstore_proto::scheduler::scheduler_service_client::SchedulerServiceClient;
use coldstore_proto::scheduler::{
    HeadBucketRequest, HeadObjectRequest, HeadObjectResponse, ListBucketsResponse,
};
use std::sync::Arc;
use tonic::transport::Channel;
use tracing::info;

#[tonic::async_trait]
pub trait GatewayBackend: Send + Sync + 'static {
    async fn list_buckets(&self) -> std::result::Result<ListBucketsResponse, tonic::Status>;
    async fn head_bucket(&self, bucket: &str) -> std::result::Result<(), tonic::Status>;
    async fn head_object(
        &self,
        bucket: &str,
        key: &str,
    ) -> std::result::Result<HeadObjectResponse, tonic::Status>;
}

pub struct GrpcGatewayBackend {
    scheduler_addr: String,
}

impl GrpcGatewayBackend {
    pub fn new(scheduler_addr: String) -> Self {
        Self { scheduler_addr }
    }

    async fn connect(&self) -> std::result::Result<SchedulerServiceClient<Channel>, tonic::Status> {
        SchedulerServiceClient::connect(self.scheduler_addr.clone())
            .await
            .map_err(|err| tonic::Status::unavailable(err.to_string()))
    }
}

#[tonic::async_trait]
impl GatewayBackend for GrpcGatewayBackend {
    async fn list_buckets(&self) -> std::result::Result<ListBucketsResponse, tonic::Status> {
        let mut client = self.connect().await?;
        client.list_buckets(()).await.map(|r| r.into_inner())
    }

    async fn head_bucket(&self, bucket: &str) -> std::result::Result<(), tonic::Status> {
        let mut client = self.connect().await?;
        client
            .head_bucket(HeadBucketRequest {
                bucket: bucket.to_string(),
            })
            .await
            .map(|_| ())
    }

    async fn head_object(
        &self,
        bucket: &str,
        key: &str,
    ) -> std::result::Result<HeadObjectResponse, tonic::Status> {
        let mut client = self.connect().await?;
        client
            .head_object(HeadObjectRequest {
                bucket: bucket.to_string(),
                key: key.to_string(),
                version_id: None,
            })
            .await
            .map(|r| r.into_inner())
    }
}

pub struct GatewayState {
    pub backend: Arc<dyn GatewayBackend>,
}

pub async fn run(config: GatewayConfig) -> Result<()> {
    let scheduler_addr = format!("http://{}", &config.scheduler_addrs[0]);
    let state = Arc::new(GatewayState {
        backend: Arc::new(GrpcGatewayBackend::new(scheduler_addr)),
    });

    let app = handler::router(state);

    let listener = tokio::net::TcpListener::bind(&config.listen).await?;
    info!("S3 Gateway 启动在 {}", config.listen);
    axum::serve(listener, app).await?;

    Ok(())
}
