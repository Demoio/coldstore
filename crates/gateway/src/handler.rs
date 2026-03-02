use crate::GatewayState;
use axum::{routing::get, Router};
use std::sync::Arc;

pub fn router(state: Arc<GatewayState>) -> Router {
    Router::new()
        .route("/health", get(health))
        // TODO: S3 API 路由
        // PUT /{bucket}/{key}       → PutObject
        // GET /{bucket}/{key}       → GetObject
        // HEAD /{bucket}/{key}      → HeadObject
        // DELETE /{bucket}/{key}    → DeleteObject
        // POST /{bucket}/{key}?restore → RestoreObject
        // GET /{bucket}             → ListObjects
        // PUT /{bucket}             → CreateBucket
        // DELETE /{bucket}          → DeleteBucket
        // HEAD /{bucket}            → HeadBucket
        // GET /                     → ListBuckets
        .with_state(state)
}

async fn health() -> &'static str {
    "OK"
}
