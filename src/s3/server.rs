use crate::error::Result;
use crate::s3::handler::S3Handler;
use axum::Router;

pub struct S3Server {
    handler: S3Handler,
}

impl S3Server {
    pub fn new(handler: S3Handler) -> Self {
        Self { handler }
    }

    pub fn router(&self) -> Router {
        Router::new()
            // TODO: 添加 S3 API 路由
            // .route("/:bucket/:object", get(get_object).put(put_object))
            // .route("/:bucket", get(list_objects))
            // .route("/:bucket/:object?restore", post(restore_object))
            .route("/health", axum::routing::get(|| async { "OK" }))
    }

    pub async fn start(&self, addr: &str) -> Result<()> {
        let app = self.router();
        let listener = tokio::net::TcpListener::bind(addr).await?;

        tracing::info!("S3 服务器启动在 {}", addr);
        axum::serve(listener, app).await?;

        Ok(())
    }
}
