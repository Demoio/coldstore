use crate::protocol::{
    format_restore_header, is_restore_request, parse_restore_request, S3ErrorCode, S3ErrorResponse,
};
use crate::GatewayState;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::Response;
use axum::{routing::get, Router};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;

const DEFAULT_LIST_MAX_KEYS: u32 = 1000;
const MAX_LIST_MAX_KEYS: u32 = 1000;

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
                .put(put_object)
                .delete(delete_object)
                .head(head_object)
                .post(post_object),
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
    let max_keys = match parse_list_max_keys(query.get("max-keys").map(String::as_str)) {
        Ok(max_keys) => max_keys,
        Err(response) => return *response,
    };

    match state
        .backend
        .list_objects(&bucket, prefix, marker, delimiter, max_keys)
        .await
    {
        Ok(response) => list_objects_xml_response(&response),
        Err(status) => grpc_status_to_s3_response(status, &format!("/{bucket}")),
    }
}

fn parse_list_max_keys(value: Option<&str>) -> std::result::Result<u32, Box<Response>> {
    let raw = value.unwrap_or("");
    let max_keys = if raw.is_empty() {
        DEFAULT_LIST_MAX_KEYS
    } else {
        raw.parse::<u32>().map_err(|_| {
            Box::new(bad_request_response_with_code(
                S3ErrorCode::InvalidArgument,
                "max-keys must be an integer between 1 and 1000",
                "/",
            ))
        })?
    };

    if !(1..=MAX_LIST_MAX_KEYS).contains(&max_keys) {
        return Err(Box::new(bad_request_response_with_code(
            S3ErrorCode::InvalidArgument,
            "max-keys must be an integer between 1 and 1000",
            "/",
        )));
    }

    Ok(max_keys)
}

async fn get_object(
    State(state): State<Arc<GatewayState>>,
    Path((bucket, key)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let resource = format!("/{bucket}/{key}");

    if let Some(response) =
        match check_read_preconditions(&state, &bucket, &key, &headers, &resource).await {
            Ok(response) => response,
            Err(status) => return grpc_status_to_s3_response(status, &resource),
        }
    {
        return response;
    }

    let object = match state.backend.get_object(&bucket, &key).await {
        Ok(object) => object,
        Err(status) => return grpc_status_to_s3_response(status, &resource),
    };

    let object_size = object.body.len();
    let mut response_body = object.body;
    let mut status_code = StatusCode::OK;

    if let Some(range_value) = headers.get(header::RANGE) {
        let (start, end) = match range_value.to_str() {
            Ok(raw_range) => match parse_range_header(raw_range, object_size) {
                Ok(Some(range)) => range,
                Ok(None) => {
                    return bad_request_response_with_code(
                        S3ErrorCode::InvalidArgument,
                        "invalid Range header",
                        &resource,
                    )
                }
                Err(response) => return *response,
            },
            Err(_) => {
                return bad_request_response_with_code(
                    S3ErrorCode::InvalidArgument,
                    "invalid Range header",
                    &resource,
                )
            }
        };

        let range_response_length = end.saturating_sub(start).saturating_add(1);
        response_body = response_body[start..=end].to_vec();
        status_code = StatusCode::PARTIAL_CONTENT;

        let mut response = Response::new(Body::from(response_body));
        *response.status_mut() = status_code;
        apply_object_headers(response.headers_mut(), &object.head);
        response.headers_mut().insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&range_response_length.to_string()).unwrap(),
        );
        response
            .headers_mut()
            .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        response.headers_mut().insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{object_size}"))
                .unwrap_or_else(|_| HeaderValue::from_static("bytes */*")),
        );
        return response;
    }

    let mut response = Response::new(Body::from(response_body));
    *response.status_mut() = status_code;
    response
        .headers_mut()
        .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    apply_object_headers(response.headers_mut(), &object.head);
    response
}

fn parse_range_header(
    raw_range: &str,
    body_length: usize,
) -> std::result::Result<Option<(usize, usize)>, Box<Response>> {
    parse_byte_range(raw_range, body_length).map_err(|err| match err {
        ByteRangeParseError::Malformed => Box::new(bad_request_response_with_code(
            S3ErrorCode::InvalidArgument,
            "invalid range syntax",
            "Range",
        )),
        ByteRangeParseError::Unsatisfiable => {
            Box::new(range_not_satisfiable_response(body_length as u64))
        }
    })
}

fn parse_byte_range(
    raw_range: &str,
    object_size: usize,
) -> std::result::Result<Option<(usize, usize)>, ByteRangeParseError> {
    let raw = raw_range.trim();
    let spec = raw
        .split_once('=')
        .and_then(|(unit, spec)| {
            if unit.eq_ignore_ascii_case("bytes") {
                Some(spec)
            } else {
                None
            }
        })
        .ok_or(ByteRangeParseError::Malformed)?;
    if spec.is_empty() {
        return Err(ByteRangeParseError::Malformed);
    }

    if object_size == 0 {
        return Err(ByteRangeParseError::Unsatisfiable);
    }
    if spec.contains(',') {
        return Err(ByteRangeParseError::Malformed);
    }

    let (start_raw, end_raw) = spec.split_once('-').ok_or(ByteRangeParseError::Malformed)?;

    if start_raw.is_empty() {
        let suffix_len = end_raw
            .parse::<usize>()
            .map_err(|_| ByteRangeParseError::Malformed)?;
        if suffix_len == 0 {
            return Err(ByteRangeParseError::Malformed);
        }

        let start = object_size.saturating_sub(suffix_len);
        return Ok(Some((start, object_size.saturating_sub(1))));
    }

    let start = start_raw
        .parse::<usize>()
        .map_err(|_| ByteRangeParseError::Malformed)?;

    if start >= object_size {
        return Err(ByteRangeParseError::Unsatisfiable);
    }

    let mut end = if end_raw.is_empty() {
        object_size.saturating_sub(1)
    } else {
        end_raw
            .parse::<usize>()
            .map_err(|_| ByteRangeParseError::Malformed)?
    };

    if end < start {
        return Err(ByteRangeParseError::Unsatisfiable);
    }
    if end >= object_size {
        end = object_size.saturating_sub(1);
    }

    Ok(Some((start, end)))
}

enum ByteRangeParseError {
    Malformed,
    Unsatisfiable,
}

async fn check_read_preconditions(
    state: &Arc<GatewayState>,
    bucket: &str,
    key: &str,
    headers: &HeaderMap,
    resource: &str,
) -> std::result::Result<Option<Response>, tonic::Status> {
    let if_match = headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok());
    let if_none_match = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok());
    let if_modified_since = match headers
        .get(header::IF_MODIFIED_SINCE)
        .and_then(|value| value.to_str().ok())
    {
        Some(value) => parse_http_datetime(value)
            .map_err(|_| tonic::Status::invalid_argument("invalid If-Modified-Since header"))?,
        None => None,
    };

    let if_unmodified_since = match headers
        .get(header::IF_UNMODIFIED_SINCE)
        .and_then(|value| value.to_str().ok())
    {
        Some(value) => parse_http_datetime(value)
            .map_err(|_| tonic::Status::invalid_argument("invalid If-Unmodified-Since header"))?,
        None => None,
    };

    if if_match.is_none()
        && if_none_match.is_none()
        && if_modified_since.is_none()
        && if_unmodified_since.is_none()
    {
        return Ok(None);
    }

    let head = state.backend.head_object(bucket, key).await?;
    let object_etag = head.etag.clone();
    let object_modified_at = head
        .last_modified
        .as_ref()
        .map(timestamp_from_prost_timestamp);

    if let Some(value) = if_match {
        if !etag_header_matches(value, &object_etag) {
            return Ok(Some(precondition_failed_response("If-Match", resource)));
        }
    }

    if let Some(unmodified_since) = if_unmodified_since {
        let Some(object_modified_at) = object_modified_at else {
            return Err(tonic::Status::failed_precondition(
                "If-Unmodified-Since requires Last-Modified",
            ));
        };

        if object_modified_at > unmodified_since {
            return Ok(Some(precondition_failed_response(
                "If-Unmodified-Since failed",
                resource,
            )));
        }
    }

    if let Some(value) = if_none_match {
        if etag_header_matches(value, &object_etag) {
            return Ok(Some(not_modified_response(resource, &object_etag)));
        }
    }

    if let Some(modified_since) = if_modified_since {
        let Some(object_modified_at) = object_modified_at else {
            return Err(tonic::Status::failed_precondition(
                "If-Modified-Since requires Last-Modified",
            ));
        };

        if object_modified_at <= modified_since {
            return Ok(Some(not_modified_response(resource, &object_etag)));
        }
    }

    Ok(None)
}

fn parse_http_datetime(value: &str) -> std::result::Result<Option<i64>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }

    const RFC_1123_HTTP_DATE: &str = "%a, %d %b %Y %H:%M:%S GMT";
    const RFC_850_HTTP_DATE: &str = "%A, %d-%b-%y %H:%M:%S GMT";
    const ANSI_C_DATE: &str = "%a %b %e %H:%M:%S %Y";

    for format in [RFC_1123_HTTP_DATE, RFC_850_HTTP_DATE, ANSI_C_DATE] {
        if let Ok(time) = DateTime::parse_from_str(value, format) {
            return Ok(Some(time.timestamp()));
        }
    }

    DateTime::parse_from_rfc2822(value)
        .map(|time| Some(time.timestamp()))
        .map_err(|_| "unable to parse HTTP date".into())
}

fn timestamp_from_prost_timestamp(timestamp: &prost_types::Timestamp) -> i64 {
    timestamp.seconds
}

fn etag_header_matches(header_value: &str, etag: &str) -> bool {
    header_value
        .split(',')
        .map(str::trim)
        .map(|token| token.trim_matches('"'))
        .any(|token| token == "*" || token == etag || token == format!("W/\"{etag}\""))
}

fn not_modified_response(resource: &str, etag: &str) -> Response {
    let mut response = empty_response(StatusCode::NOT_MODIFIED);
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(etag).unwrap_or_else(|_| HeaderValue::from_static("\"\"")),
    );
    response.headers_mut().insert(
        HeaderName::from_static("x-amz-request-id"),
        HeaderValue::from_str(resource).unwrap_or_else(|_| HeaderValue::from_static("-")),
    );
    response
}

fn precondition_failed_response(message: &str, resource: &str) -> Response {
    let body = S3ErrorResponse {
        code: S3ErrorCode::PreconditionFailed,
        message,
        resource,
    }
    .to_xml();
    s3_xml_response(StatusCode::PRECONDITION_FAILED, body)
}

fn range_not_satisfiable_response(body_size: u64) -> Response {
    let mut response = empty_response(StatusCode::RANGE_NOT_SATISFIABLE);
    response.headers_mut().insert(
        header::CONTENT_RANGE,
        HeaderValue::from_str(&format!("bytes */{body_size}")).unwrap(),
    );
    response
}

fn list_buckets_xml_response(
    response: &coldstore_proto::scheduler::ListBucketsResponse,
) -> Response {
    let buckets_xml = response
        .buckets
        .iter()
        .map(|bucket| format!("<Bucket><Name>{}</Name></Bucket>", escape_xml(&bucket.name)))
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
    let mut contents = String::new();
    for entry in &response.contents {
        contents.push_str(&format!(
            "<Contents><Key>{}</Key><ETag>{}</ETag><Size>{}</Size><StorageClass>{}</StorageClass></Contents>",
            escape_xml(&entry.key),
            escape_xml(&entry.etag),
            entry.size,
            escape_xml(&entry.storage_class)
        ));
    }

    let mut common_prefixes = String::new();
    for common_prefix in &response.common_prefixes {
        common_prefixes.push_str(&format!(
            "<CommonPrefixes><Prefix>{}</Prefix></CommonPrefixes>",
            escape_xml(&common_prefix.prefix)
        ));
    }

    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListBucketResult><Name>{}</Name><Prefix>{}</Prefix><Marker>{}</Marker><NextMarker>{}</NextMarker><MaxKeys>{}</MaxKeys><IsTruncated>{}</IsTruncated>{}{}</ListBucketResult>",
        escape_xml(&response.bucket),
        escape_xml(&response.prefix.clone().unwrap_or_default()),
        escape_xml(&response.marker.clone().unwrap_or_default()),
        escape_xml(&response.next_marker.clone().unwrap_or_default()),
        response.max_keys,
        response.is_truncated,
        contents,
        common_prefixes,
    );
    xml_response(StatusCode::OK, body)
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn put_object_success_response(
    put_response: coldstore_proto::scheduler::PutObjectResponse,
) -> Response {
    let mut response = empty_response(StatusCode::OK);
    response.headers_mut().insert(
        axum::http::header::ETAG,
        HeaderValue::from_str(&put_response.etag).unwrap(),
    );
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
        HeaderValue::from_str(&head.content_length.to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("0")),
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

    if let Some(last_modified) = &head.last_modified {
        if let Some(last_modified) =
            DateTime::from_timestamp(last_modified.seconds, 0).map(|time| time.with_timezone(&Utc))
        {
            headers.insert(
                header::LAST_MODIFIED,
                HeaderValue::from_str(&format_http_date(last_modified))
                    .unwrap_or_else(|_| HeaderValue::from_static("")),
            );
        }
    }
}

fn format_http_date(time: DateTime<Utc>) -> String {
    time.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

fn normalize_restore_info(restore_info: &str) -> String {
    let restore_info = restore_info.trim();
    if let Some(expiry) = restore_info.strip_prefix("ongoing-request=\"false\", expiry-date=\"") {
        format_restore_header(false, Some(expiry.trim_end_matches('"')))
    } else if let Some(expiry) =
        restore_info.strip_prefix("ongoing-request=\"false\", expiry-ts=\"")
    {
        format_restore_header(false, Some(expiry.trim_end_matches('"')))
    } else if restore_info.starts_with("ongoing-request=\"false\"") {
        format_restore_header(false, None)
    } else {
        format_restore_header(true, None)
    }
}

fn bad_request_response(operation: &str, resource: &str) -> Response {
    bad_request_response_with_code(
        S3ErrorCode::NotImplemented,
        &format!("unsupported POST action for {operation}"),
        resource,
    )
}

fn bad_request_response_with_code(code: S3ErrorCode, message: &str, resource: &str) -> Response {
    let body = S3ErrorResponse {
        code,
        message,
        resource,
    }
    .to_xml();
    s3_xml_response(StatusCode::BAD_REQUEST, body)
}

fn grpc_status_to_s3_response(status: tonic::Status, resource: &str) -> Response {
    let status_message = status.message();
    let (code, http_status, retry_after) = match status.code() {
        tonic::Code::AlreadyExists => (
            S3ErrorCode::RestoreAlreadyInProgress,
            StatusCode::CONFLICT,
            None,
        ),
        tonic::Code::NotFound => {
            let code = if resource.matches('/').count() > 1 {
                S3ErrorCode::NoSuchKey
            } else {
                S3ErrorCode::NoSuchBucket
            };
            (code, StatusCode::NOT_FOUND, None)
        }
        tonic::Code::InvalidArgument => {
            (S3ErrorCode::InvalidArgument, StatusCode::BAD_REQUEST, None)
        }
        tonic::Code::FailedPrecondition => {
            (S3ErrorCode::InvalidObjectState, StatusCode::FORBIDDEN, None)
        }
        tonic::Code::Unavailable
            if status_message
                .to_lowercase()
                .contains("glacier expedited retrieval is not available") =>
        {
            (
                S3ErrorCode::GlacierExpeditedRetrievalNotAvailable,
                StatusCode::SERVICE_UNAVAILABLE,
                Some(1),
            )
        }
        tonic::Code::Unavailable => (
            S3ErrorCode::ServiceUnavailable,
            StatusCode::SERVICE_UNAVAILABLE,
            Some(1),
        ),
        tonic::Code::ResourceExhausted => match cache_reject_status_hint(status_message) {
            Some(CacheRejectStatus::Backpressure) => (
                S3ErrorCode::SlowDown,
                StatusCode::SERVICE_UNAVAILABLE,
                Some(1),
            ),
            Some(CacheRejectStatus::RateLimited) => (
                S3ErrorCode::SlowDown,
                StatusCode::SERVICE_UNAVAILABLE,
                Some(1),
            ),
            Some(CacheRejectStatus::InvalidRequest) => {
                (S3ErrorCode::InvalidArgument, StatusCode::BAD_REQUEST, None)
            }
            None => (
                S3ErrorCode::SlowDown,
                StatusCode::SERVICE_UNAVAILABLE,
                Some(1),
            ),
        },
        tonic::Code::Unimplemented => (
            S3ErrorCode::NotImplemented,
            StatusCode::NOT_IMPLEMENTED,
            None,
        ),
        _ => (S3ErrorCode::NotImplemented, StatusCode::BAD_GATEWAY, None),
    };
    let mut response = {
        let body = S3ErrorResponse {
            code,
            message: status.message(),
            resource,
        }
        .to_xml();
        s3_xml_response(http_status, body)
    };

    if let Some(seconds) = retry_after {
        response.headers_mut().insert(
            header::RETRY_AFTER,
            HeaderValue::from_str(&seconds.to_string())
                .unwrap_or_else(|_| HeaderValue::from_static("1")),
        );
    }

    response
}

enum CacheRejectStatus {
    Backpressure,
    RateLimited,
    InvalidRequest,
}

fn cache_reject_status_hint(message: &str) -> Option<CacheRejectStatus> {
    let message = message.trim();
    if !message.starts_with("capacity_reject:") {
        return None;
    }
    let reason = message["capacity_reject:".len()..]
        .split(':')
        .next()
        .unwrap_or_default();
    match reason {
        "incoming_larger_than_capacity" => Some(CacheRejectStatus::InvalidRequest),
        "client_rate_limited" | "concurrency_limit_exceeded" | "request_rate_exceeded" => {
            Some(CacheRejectStatus::RateLimited)
        }
        "staging_budget_exceeded"
        | "restored_budget_exceeded"
        | "global_capacity_exceeded"
        | "low_watermark_exceeded"
        | "no_eviction_candidate"
        | "zero_capacity" => Some(CacheRejectStatus::Backpressure),
        _ => None,
    }
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

async fn put_object(
    State(state): State<Arc<GatewayState>>,
    Path((bucket, key)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    match state
        .backend
        .put_object(&bucket, &key, body.to_vec(), content_type)
        .await
    {
        Ok(response) => put_object_success_response(response),
        Err(status) => grpc_status_to_s3_response(status, &format!("/{bucket}/{key}")),
    }
}

async fn delete_object(
    State(state): State<Arc<GatewayState>>,
    Path((bucket, key)): Path<(String, String)>,
) -> Response {
    match state.backend.delete_object(&bucket, &key).await {
        Ok(()) => empty_response(StatusCode::NO_CONTENT),
        Err(status) => grpc_status_to_s3_response(status, &format!("/{bucket}/{key}")),
    }
}

async fn head_object(
    State(state): State<Arc<GatewayState>>,
    Path((bucket, key)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let resource = format!("/{bucket}/{key}");

    if let Some(response) =
        match check_read_preconditions(&state, &bucket, &key, &headers, &resource).await {
            Ok(response) => response,
            Err(status) => return grpc_status_to_s3_response(status, &resource),
        }
    {
        return response;
    }

    match state.backend.head_object(&bucket, &key).await {
        Ok(head) => head_object_success_response(head),
        Err(status) => grpc_status_to_s3_response(status, &resource),
    }
}

async fn post_object(
    State(state): State<Arc<GatewayState>>,
    Path((bucket, key)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
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
    if !is_restore_request(raw_query.as_deref()) {
        return bad_request_response("UnsupportedPostAction", &resource);
    }

    let restore_request = match parse_restore_request(&query, body.as_ref()) {
        Ok(request) => request,
        Err(_) => {
            return bad_request_response_with_code(
                S3ErrorCode::InvalidArgument,
                "invalid restore request payload",
                &resource,
            );
        }
    };

    match state
        .backend
        .restore_object(&bucket, &key, restore_request.days, restore_request.tier)
        .await
    {
        Ok(response) => {
            let status = match response.status_code {
                200 => StatusCode::OK,
                202 => StatusCode::ACCEPTED,
                409 => StatusCode::CONFLICT,
                _ => StatusCode::ACCEPTED,
            };
            empty_response(status)
        }
        Err(status) => grpc_status_to_s3_response(status, &resource),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DownloadedObject, GatewayBackend};
    use axum::body::to_bytes;
    use axum::http::Request;
    use coldstore_proto::scheduler::{
        BucketEntry, HeadObjectResponse, ListBucketsResponse, ListObjectsResponse, ObjectEntry,
        PutObjectResponse, RestoreObjectResponse,
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

        async fn put_object(
            &self,
            _bucket: &str,
            _key: &str,
            _body: Vec<u8>,
            _content_type: Option<String>,
        ) -> std::result::Result<PutObjectResponse, tonic::Status> {
            Ok(PutObjectResponse {
                etag: "etag-put".into(),
                version_id: "v1".into(),
            })
        }

        async fn get_object(
            &self,
            bucket: &str,
            key: &str,
        ) -> std::result::Result<DownloadedObject, tonic::Status> {
            Ok(DownloadedObject {
                head: self.head_object(bucket, key).await?,
                body: if key == "empty.txt" {
                    Vec::new()
                } else {
                    b"hello world".to_vec()
                },
            })
        }

        async fn delete_object(
            &self,
            bucket: &str,
            key: &str,
        ) -> std::result::Result<(), tonic::Status> {
            if bucket == "docs" && key == "readme.txt" {
                Ok(())
            } else {
                Err(tonic::Status::not_found("object missing"))
            }
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
            } else if bucket == "docs" && key == "empty.txt" {
                Ok(HeadObjectResponse {
                    content_length: 0,
                    content_type: Some("text/plain".into()),
                    etag: "etag-empty".into(),
                    storage_class: 2,
                    restore_info: None,
                    last_modified: None,
                })
            } else {
                Err(tonic::Status::not_found("object missing"))
            }
        }

        async fn restore_object(
            &self,
            bucket: &str,
            key: &str,
            _days: u32,
            _tier: coldstore_proto::common::RestoreTier,
        ) -> std::result::Result<RestoreObjectResponse, tonic::Status> {
            if bucket == "docs" && key == "readme.txt" {
                Ok(RestoreObjectResponse { status_code: 202 })
            } else if bucket == "docs" && key == "pending.txt" {
                Ok(RestoreObjectResponse { status_code: 409 })
            } else {
                Err(tonic::Status::not_found("object missing"))
            }
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
    async fn list_objects_route_rejects_invalid_max_keys() {
        let response = test_router(state())
            .oneshot(
                Request::builder()
                    .uri("/docs?max-keys=abc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8(body.to_vec())
            .unwrap()
            .contains("<Code>InvalidArgument</Code>"));
    }

    #[tokio::test]
    async fn list_objects_route_rejects_invalid_max_keys_range() {
        let response = test_router(state())
            .oneshot(
                Request::builder()
                    .uri("/docs?max-keys=0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
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
    async fn put_object_route_uses_backend() {
        let response = test_router(state())
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/docs/readme.txt")
                    .header("content-type", "text/plain")
                    .body(Body::from("hello world"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["etag"], "etag-put");
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
    async fn get_object_route_supports_single_range() {
        let response = test_router(state())
            .oneshot(
                Request::builder()
                    .uri("/docs/readme.txt")
                    .header("range", "bytes=1-3")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), b"ell");
    }

    #[tokio::test]
    async fn get_object_route_range_on_large_offsets() {
        let response = test_router(state())
            .oneshot(
                Request::builder()
                    .uri("/docs/readme.txt")
                    .header("range", "bytes=-3")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), b"rld");
    }

    #[tokio::test]
    async fn get_object_route_range_on_empty_object_is_not_satisfiable() {
        let response = test_router(state())
            .oneshot(
                Request::builder()
                    .uri("/docs/empty.txt")
                    .header("range", "bytes=0-0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .unwrap(),
            "bytes */0"
        );
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

    #[tokio::test]
    async fn delete_object_route_uses_backend() {
        let response = test_router(state())
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/docs/readme.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn restore_post_route_uses_backend() {
        let response = test_router(state())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/docs/readme.txt?restore=true")
                    .body(Body::from(
                        "<RestoreRequest><Days>2</Days><GlacierJobParameters><Tier>Bulk</Tier></GlacierJobParameters></RestoreRequest>",
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn restore_post_route_rejects_invalid_body() {
        let response = test_router(state())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/docs/readme.txt?restore=true")
                    .body(Body::from(
                        "<RestoreRequest><Days>0</Days><GlacierJobParameters><Tier>Bulk</Tier></GlacierJobParameters></RestoreRequest>",
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8(body.to_vec())
            .unwrap()
            .contains("<Code>InvalidArgument</Code>"));
    }

    #[tokio::test]
    async fn restore_post_route_returns_conflict_for_in_progress_restore() {
        let response = test_router(state())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/docs/pending.txt?restore=true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
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
    async fn failed_precondition_maps_to_invalid_object_state() {
        let response = grpc_status_to_s3_response(
            tonic::Status::failed_precondition("object must be restored"),
            "/docs/readme.txt",
        );
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8(body.to_vec())
            .unwrap()
            .contains("<Code>InvalidObjectState</Code>"));
    }

    #[tokio::test]
    async fn resource_exhausted_maps_to_s3_slow_down_with_retry_after() {
        let response = grpc_status_to_s3_response(
            tonic::Status::resource_exhausted("cache quota exceeded"),
            "/docs/readme.txt",
        );
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("1")
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8(body.to_vec())
            .unwrap()
            .contains("<Code>SlowDown</Code>"));
    }

    #[tokio::test]
    async fn resource_exhausted_maps_staging_budget_pressure_to_slow_down() {
        let response = grpc_status_to_s3_response(
            tonic::Status::resource_exhausted(
                "capacity_reject:staging_budget_exceeded: not enough staging cache budget",
            ),
            "/docs/readme.txt",
        );
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("1")
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8(body.to_vec())
            .unwrap()
            .contains("<Code>SlowDown</Code>"));
    }

    #[tokio::test]
    async fn unavailable_maps_to_glacier_expedited_retrieval_not_available() {
        let response = grpc_status_to_s3_response(
            tonic::Status::unavailable(
                "glacier expedited retrieval is not available in this environment",
            ),
            "/docs/readme.txt",
        );
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8(body.to_vec())
            .unwrap()
            .contains("<Code>GlacierExpeditedRetrievalNotAvailable</Code>"));
    }

    #[tokio::test]
    async fn unavailable_maps_to_service_unavailable_with_retry_after() {
        let response = grpc_status_to_s3_response(
            tonic::Status::unavailable("scheduler down"),
            "/docs/readme.txt",
        );
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8(body.to_vec())
            .unwrap()
            .contains("<Code>ServiceUnavailable</Code>"));
    }

    #[tokio::test]
    async fn rate_limited_resource_exhausted_maps_to_s3_slow_down_with_retry_after() {
        let response = grpc_status_to_s3_response(
            tonic::Status::resource_exhausted(
                "capacity_reject:concurrency_limit_exceeded: client throttled",
            ),
            "/docs/readme.txt",
        );
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("1")
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8(body.to_vec())
            .unwrap()
            .contains("<Code>SlowDown</Code>"));
    }

    #[tokio::test]
    async fn s3_error_response_escapes_backend_message_and_resource() {
        let response = grpc_status_to_s3_response(
            tonic::Status::invalid_argument("bad <tag> & \"quote\""),
            "/docs/a&b<raw>.txt",
        );
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("bad &lt;tag&gt; &amp; &quot;quote&quot;"));
        assert!(body.contains("/docs/a&amp;b&lt;raw&gt;.txt"));
    }

    #[tokio::test]
    async fn get_with_if_none_match_returns_not_modified() {
        let response = test_router(state())
            .oneshot(
                Request::builder()
                    .uri("/docs/readme.txt")
                    .header("if-none-match", "etag-1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    }

    #[tokio::test]
    async fn get_with_if_match_returns_not_precondition_failed() {
        let response = test_router(state())
            .oneshot(
                Request::builder()
                    .uri("/docs/readme.txt")
                    .header("if-match", "etag-other")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);
    }
}
