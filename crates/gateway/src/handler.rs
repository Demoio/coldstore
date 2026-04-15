use crate::protocol::{is_restore_request, S3ErrorCode, S3ErrorResponse};
use crate::GatewayState;
use axum::body::Body;
use axum::extract::{Path, Query};
use axum::http::{HeaderValue, StatusCode};
use axum::response::Response;
use axum::{
    routing::{get, put},
    Router,
};
use std::collections::HashMap;
use std::sync::Arc;

pub fn router(state: Arc<GatewayState>) -> Router {
    build_router().with_state(state)
}

fn build_router() -> Router<Arc<GatewayState>> {
    Router::new()
        .route("/health", get(health))
        .route("/", get(list_buckets_placeholder))
        .route(
            "/:bucket",
            put(create_bucket_placeholder)
                .delete(delete_bucket_placeholder)
                .head(head_bucket_placeholder),
        )
        .route(
            "/:bucket/*key",
            get(get_object_placeholder)
                .put(put_object_placeholder)
                .delete(delete_object_placeholder)
                .head(head_object_placeholder)
                .post(post_object_placeholder),
        )
}

#[cfg(test)]
fn test_router() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/", get(list_buckets_placeholder))
        .route(
            "/:bucket",
            put(create_bucket_placeholder)
                .delete(delete_bucket_placeholder)
                .head(head_bucket_placeholder),
        )
        .route(
            "/:bucket/*key",
            get(get_object_placeholder)
                .put(put_object_placeholder)
                .delete(delete_object_placeholder)
                .head(head_object_placeholder)
                .post(post_object_placeholder),
        )
}

async fn health() -> &'static str {
    "OK"
}

async fn list_buckets_placeholder() -> Response {
    not_implemented_response("ListBuckets", "/")
}

async fn create_bucket_placeholder(Path(bucket): Path<String>) -> Response {
    not_implemented_response("CreateBucket", &format!("/{bucket}"))
}

async fn delete_bucket_placeholder(Path(bucket): Path<String>) -> Response {
    not_implemented_response("DeleteBucket", &format!("/{bucket}"))
}

async fn head_bucket_placeholder(Path(bucket): Path<String>) -> Response {
    not_implemented_response("HeadBucket", &format!("/{bucket}"))
}

async fn get_object_placeholder(Path((bucket, key)): Path<(String, String)>) -> Response {
    not_implemented_response("GetObject", &format!("/{bucket}/{key}"))
}

async fn put_object_placeholder(Path((bucket, key)): Path<(String, String)>) -> Response {
    not_implemented_response("PutObject", &format!("/{bucket}/{key}"))
}

async fn delete_object_placeholder(Path((bucket, key)): Path<(String, String)>) -> Response {
    not_implemented_response("DeleteObject", &format!("/{bucket}/{key}"))
}

async fn head_object_placeholder(Path((bucket, key)): Path<(String, String)>) -> Response {
    not_implemented_response("HeadObject", &format!("/{bucket}/{key}"))
}

async fn post_object_placeholder(
    Path((bucket, key)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let raw_query = if query.is_empty() {
        None
    } else {
        Some(
            query
                .iter()
                .map(|(k, v)| {
                    if v.is_empty() {
                        k.clone()
                    } else {
                        format!("{k}={v}")
                    }
                })
                .collect::<Vec<_>>()
                .join("&"),
        )
    };
    let resource = format!("/{bucket}/{key}");
    if is_restore_request(raw_query.as_deref()) {
        not_implemented_response("RestoreObject", &resource)
    } else {
        bad_request_response("UnsupportedPostAction", &resource)
    }
}

fn not_implemented_response(operation: &str, resource: &str) -> Response {
    let body = S3ErrorResponse {
        code: S3ErrorCode::NotImplemented,
        message: &format!(
            "{operation} route skeleton is present but backend integration is not implemented"
        ),
        resource,
    }
    .to_xml();
    s3_xml_response(StatusCode::NOT_IMPLEMENTED, body)
}

fn bad_request_response(operation: &str, resource: &str) -> Response {
    let body = S3ErrorResponse {
        code: S3ErrorCode::NotImplemented,
        message: &format!("unsupported POST action for {operation}"),
        resource,
    }
    .to_xml();
    s3_xml_response(StatusCode::BAD_REQUEST, body)
}

fn s3_xml_response(status: StatusCode, body: String) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/xml"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::Request;
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn health_route_returns_ok() {
        let response = test_router()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn list_buckets_route_returns_not_implemented_xml() {
        let response = test_router()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("<Code>NotImplemented</Code>"));
        assert!(body.contains("ListBuckets"));
    }

    #[tokio::test]
    async fn restore_post_route_returns_not_implemented_xml() {
        let response = test_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/docs/readme.txt?restore=true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("RestoreObject"));
    }

    #[tokio::test]
    async fn head_bucket_route_returns_not_implemented() {
        let response = test_router()
            .oneshot(
                Request::builder()
                    .method("HEAD")
                    .uri("/docs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    }
}
