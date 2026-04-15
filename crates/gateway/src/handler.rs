use crate::protocol::{format_restore_header, is_restore_request, S3ErrorCode, S3ErrorResponse};
use crate::{DownloadedObject, GatewayState};
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header::HeaderName, HeaderValue, StatusCode};
use axum::response::Response;
use axum::{routing::get, Router};
use std::collections::HashMap;
use std::sync::Arc;

pub fn router(state: Arc<GatewayState>) -> Router {
    build_router().with_state(state)
}

fn build_router() -> Router<Arc<GatewayState>> {
    Router::new()
        .route("/health", get(health))
        .route("/", get(list_buckets))
        .route(
            "/:bucket",
            get(list_objects)
                .put(create_bucket)
                .delete(delete_bucket)
                .head(head_bucket),
        )
        .route(
            "/:bucket/*key",
            get(get_object)
                .put(put_object_placeholder)
                .delete(delete_object_placeholder)
                .head(head_object)
                .post(post_object_placeholder),
        )
}

#[cfg(test)]
fn test_router(state: Arc<GatewayState>) -> Router {
    build_router().with_state(state)
}

async fn health() -> &'static str {
    "OK"
}

async fn list_buckets(State(state): State<Arc<GatewayState>>) -> Response {
    match state.backend.list_buckets().await {
        Ok(response) => list_buckets_xml_response(&response),
        Err(status) => grpc_status_to_s3_response(status, "/"),
    }
}

async fn create_bucket(
    State(state): State<Arc<GatewayState>>,
    Path(bucket): Path<String>,
) -> Response {
    match state.backend.create_bucket(&bucket).await {
        Ok(()) => empty_response(StatusCode::OK),
        Err(status) => grpc_status_to_s3_response(status, &format!("/{bucket}")),
    }
}

async fn delete_bucket(
    State(state): State<Arc<GatewayState>>,
    Path(bucket): Path<String>,
) -> Response {
    match state.backend.delete_bucket(&bucket).await {
        Ok(()) => empty_response(StatusCode::NO_CONTENT),
        Err(status) => grpc_status_to_s3_response(status, &format!("/{bucket}")),
    }
}

async fn head_bucket(
    State(state): State<Arc<GatewayState>>,
    Path(bucket): Path<String>,
) -> Response {
    match state.backend.head_bucket(&bucket).await {
        Ok(()) => empty_response(StatusCode::OK),
        Err(status) => grpc_status_to_s3_response(status, &format!("/{bucket}")),
    }
}

async fn list_objects(
    State(state): State<Arc<GatewayState>>,
    Path(bucket): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let prefix = query.get("prefix").map(String::as_str);
    let marker = query.get("marker").map(String::as_str);
    let delimiter = query.get("delimiter").map(String::as_str);
    let max_keys = query
        .get("max-keys")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(1000);
    match state
        .backend
        .list_objects(&bucket, prefix, marker, delimiter, max_keys)
        .await
    {
        Ok(response) => list_objects_xml_response(&response),
        Err(status) => grpc_status_to_s3_response(status, &format!("/{bucket}")),
    }
}

async fn get_object(
    State(state): State<Arc<GatewayState>>,
    Path((bucket, key)): Path<(String, String)>,
) -> Response {
    match state.backend.get_object(&bucket, &key).await {
        Ok(object) => get_object_success_response(object),
        Err(status) => grpc_status_to_s3_response(status, &format!("/{bucket}/{key}")),
    }
}

async fn put_object_placeholder(Path((bucket, key)): Path<(String, String)>) -> Response {
    not_implemented_response("PutObject", &format!("/{bucket}/{key}"))
}

async fn delete_object_placeholder(Path((bucket, key)): Path<(String, String)>) -> Response {
    not_implemented_response("DeleteObject", &format!("/{bucket}/{key}"))
}

async fn head_object(
    State(state): State<Arc<GatewayState>>,
    Path((bucket, key)): Path<(String, String)>,
) -> Response {
    match state.backend.head_object(&bucket, &key).await {
        Ok(head) => head_object_success_response(head),
        Err(status) => grpc_status_to_s3_response(status, &format!("/{bucket}/{key}")),
    }
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

fn list_buckets_xml_response(
    response: &coldstore_proto::scheduler::ListBucketsResponse,
) -> Response {
    let buckets_xml = response
        .buckets
        .iter()
        .map(|bucket| format!("<Bucket><Name>{}</Name></Bucket>", bucket.name))
        .collect::<Vec<_>>()
        .join("");
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListAllMyBucketsResult><Buckets>{}</Buckets></ListAllMyBucketsResult>",
        buckets_xml
    );
    xml_response(StatusCode::OK, body)
}

fn list_objects_xml_response(
    response: &coldstore_proto::scheduler::ListObjectsResponse,
) -> Response {
    let contents = response
        .contents
        .iter()
        .map(|entry| {
            format!(
                "<Contents><Key>{}</Key><ETag>{}</ETag><Size>{}</Size><StorageClass>{}</StorageClass></Contents>",
                entry.key, entry.etag, entry.size, entry.storage_class
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListBucketResult><Name>{}</Name>{}</ListBucketResult>",
        response.bucket, contents
    );
    xml_response(StatusCode::OK, body)
}

fn get_object_success_response(object: DownloadedObject) -> Response {
    let mut response = Response::new(Body::from(object.body));
    *response.status_mut() = StatusCode::OK;
    apply_object_headers(response.headers_mut(), &object.head);
    response
}

fn head_object_success_response(head: coldstore_proto::scheduler::HeadObjectResponse) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::OK;
    apply_object_headers(response.headers_mut(), &head);
    response
}

fn apply_object_headers(
    headers: &mut axum::http::HeaderMap,
    head: &coldstore_proto::scheduler::HeadObjectResponse,
) {
    headers.insert(
        axum::http::header::CONTENT_LENGTH,
        HeaderValue::from_str(&head.content_length.to_string()).unwrap(),
    );
    if let Some(content_type) = &head.content_type {
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_str(content_type).unwrap(),
        );
    }
    headers.insert(
        axum::http::header::ETAG,
        HeaderValue::from_str(&head.etag).unwrap(),
    );
    if let Some(restore_info) = &head.restore_info {
        headers.insert(
            HeaderName::from_static("x-amz-restore"),
            HeaderValue::from_str(&normalize_restore_info(restore_info)).unwrap(),
        );
    }
}

fn normalize_restore_info(restore_info: &str) -> String {
    if let Some(expiry) = restore_info.strip_prefix("ongoing-request=\"false\", expiry-ts=\"") {
        format_restore_header(false, Some(expiry.trim_end_matches('"')))
    } else if restore_info == "ongoing-request=\"false\"" {
        format_restore_header(false, None)
    } else {
        format_restore_header(true, None)
    }
}

fn not_implemented_response(operation: &str, resource: &str) -> Response {
    let message =
        format!("{operation} route skeleton is present but backend integration is not implemented");
    let body = S3ErrorResponse {
        code: S3ErrorCode::NotImplemented,
        message: &message,
        resource,
    }
    .to_xml();
    s3_xml_response(StatusCode::NOT_IMPLEMENTED, body)
}

fn bad_request_response(operation: &str, resource: &str) -> Response {
    let message = format!("unsupported POST action for {operation}");
    let body = S3ErrorResponse {
        code: S3ErrorCode::NotImplemented,
        message: &message,
        resource,
    }
    .to_xml();
    s3_xml_response(StatusCode::BAD_REQUEST, body)
}

fn grpc_status_to_s3_response(status: tonic::Status, resource: &str) -> Response {
    let (code, http_status) = match status.code() {
        tonic::Code::AlreadyExists => (S3ErrorCode::NotImplemented, StatusCode::CONFLICT),
        tonic::Code::NotFound => {
            let code = if resource.matches('/').count() > 1 {
                S3ErrorCode::NoSuchKey
            } else {
                S3ErrorCode::NoSuchBucket
            };
            (code, StatusCode::NOT_FOUND)
        }
        tonic::Code::Unimplemented => (S3ErrorCode::NotImplemented, StatusCode::NOT_IMPLEMENTED),
        _ => (S3ErrorCode::NotImplemented, StatusCode::BAD_GATEWAY),
    };
    let body = S3ErrorResponse {
        code,
        message: status.message(),
        resource,
    }
    .to_xml();
    s3_xml_response(http_status, body)
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

fn xml_response(status: StatusCode, body: String) -> Response {
    s3_xml_response(status, body)
}

fn empty_response(status: StatusCode) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GatewayBackend;
    use axum::body::to_bytes;
    use axum::http::Request;
    use coldstore_proto::scheduler::{
        BucketEntry, HeadObjectResponse, ListBucketsResponse, ListObjectsResponse, ObjectEntry,
    };
    use tower::util::ServiceExt;

    struct MockGatewayBackend;

    #[tonic::async_trait]
    impl GatewayBackend for MockGatewayBackend {
        async fn list_buckets(&self) -> std::result::Result<ListBucketsResponse, tonic::Status> {
            Ok(ListBucketsResponse {
                buckets: vec![BucketEntry {
                    name: "docs".into(),
                    creation_date: None,
                }],
            })
        }

        async fn create_bucket(&self, bucket: &str) -> std::result::Result<(), tonic::Status> {
            if bucket == "existing" {
                Err(tonic::Status::already_exists("bucket already exists"))
            } else {
                Ok(())
            }
        }

        async fn delete_bucket(&self, bucket: &str) -> std::result::Result<(), tonic::Status> {
            if bucket == "docs" {
                Ok(())
            } else {
                Err(tonic::Status::not_found("bucket missing"))
            }
        }

        async fn head_bucket(&self, bucket: &str) -> std::result::Result<(), tonic::Status> {
            if bucket == "docs" {
                Ok(())
            } else {
                Err(tonic::Status::not_found("bucket missing"))
            }
        }

        async fn list_objects(
            &self,
            bucket: &str,
            _prefix: Option<&str>,
            _marker: Option<&str>,
            _delimiter: Option<&str>,
            _max_keys: u32,
        ) -> std::result::Result<ListObjectsResponse, tonic::Status> {
            if bucket != "docs" {
                return Err(tonic::Status::not_found("bucket missing"));
            }
            Ok(ListObjectsResponse {
                bucket: "docs".into(),
                prefix: None,
                marker: None,
                next_marker: None,
                max_keys: 1000,
                is_truncated: false,
                contents: vec![ObjectEntry {
                    key: "readme.txt".into(),
                    last_modified: None,
                    etag: "etag-1".into(),
                    size: 42,
                    storage_class: "COLD".into(),
                }],
                common_prefixes: vec![],
            })
        }

        async fn head_object(
            &self,
            bucket: &str,
            key: &str,
        ) -> std::result::Result<HeadObjectResponse, tonic::Status> {
            if bucket == "docs" && key == "readme.txt" {
                Ok(HeadObjectResponse {
                    content_length: 42,
                    content_type: Some("text/plain".into()),
                    etag: "etag-1".into(),
                    storage_class: 2,
                    restore_info: Some("ongoing-request=\"false\", expiry-ts=\"123\"".into()),
                    last_modified: None,
                })
            } else {
                Err(tonic::Status::not_found("object missing"))
            }
        }

        async fn get_object(
            &self,
            bucket: &str,
            key: &str,
        ) -> std::result::Result<DownloadedObject, tonic::Status> {
            Ok(DownloadedObject {
                head: self.head_object(bucket, key).await?,
                body: b"hello world".to_vec(),
            })
        }
    }

    fn state() -> Arc<GatewayState> {
        Arc::new(GatewayState {
            backend: Arc::new(MockGatewayBackend),
        })
    }

    #[tokio::test]
    async fn health_route_returns_ok() {
        let response = test_router(state())
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
    async fn list_buckets_route_returns_xml_from_backend() {
        let response = test_router(state())
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8(body.to_vec())
            .unwrap()
            .contains("<Name>docs</Name>"));
    }

    #[tokio::test]
    async fn list_objects_route_returns_xml_from_backend() {
        let response = test_router(state())
            .oneshot(Request::builder().uri("/docs").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("<ListBucketResult>"));
        assert!(text.contains("<Key>readme.txt</Key>"));
    }

    #[tokio::test]
    async fn create_bucket_route_uses_backend() {
        let response = test_router(state())
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/new-bucket")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn delete_bucket_route_uses_backend() {
        let response = test_router(state())
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/docs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn get_object_route_returns_body_from_backend() {
        let response = test_router(state())
            .oneshot(
                Request::builder()
                    .uri("/docs/readme.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), b"hello world");
    }

    #[tokio::test]
    async fn restore_post_route_returns_not_implemented_xml() {
        let response = test_router(state())
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
        assert!(String::from_utf8(body.to_vec())
            .unwrap()
            .contains("RestoreObject"));
    }

    #[tokio::test]
    async fn head_bucket_route_uses_backend() {
        let response = test_router(state())
            .oneshot(
                Request::builder()
                    .method("HEAD")
                    .uri("/docs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn head_object_route_sets_restore_header() {
        let response = test_router(state())
            .oneshot(
                Request::builder()
                    .method("HEAD")
                    .uri("/docs/readme.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["etag"], "etag-1");
        assert_eq!(
            response.headers()["x-amz-restore"],
            "ongoing-request=\"false\", expiry-date=\"123\""
        );
    }
}
