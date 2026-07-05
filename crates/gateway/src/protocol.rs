//! S3 冷归档协议适配层
//!
//! 职责:
//!   - StorageClass 映射: 所有对象写入即 ColdPending
//!   - RestoreObject 请求解析 (Days, Tier)
//!   - x-amz-restore 响应头生成
//!   - 错误码映射 (InvalidObjectState, RestoreAlreadyInProgress 等)
//!   - GET 行为控制 (冷对象需先 Restore)
use coldstore_proto::common;
use std::collections::HashMap;

const MAX_RESTORE_DAYS: u32 = 365;

/// S3 错误码
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S3ErrorCode {
    InvalidObjectState,
    InvalidArgument,
    RestoreAlreadyInProgress,
    GlacierExpeditedRetrievalNotAvailable,
    SlowDown,
    ServiceUnavailable,
    TooManyRequests,
    NoSuchKey,
    NoSuchBucket,
    PreconditionFailed,
    NotModified,
    NotImplemented,
}

impl S3ErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            S3ErrorCode::InvalidObjectState => "InvalidObjectState",
            S3ErrorCode::InvalidArgument => "InvalidArgument",
            S3ErrorCode::RestoreAlreadyInProgress => "RestoreAlreadyInProgress",
            S3ErrorCode::GlacierExpeditedRetrievalNotAvailable => {
                "GlacierExpeditedRetrievalNotAvailable"
            }
            S3ErrorCode::SlowDown => "SlowDown",
            S3ErrorCode::ServiceUnavailable => "ServiceUnavailable",
            S3ErrorCode::TooManyRequests => "TooManyRequests",
            S3ErrorCode::NoSuchKey => "NoSuchKey",
            S3ErrorCode::NoSuchBucket => "NoSuchBucket",
            S3ErrorCode::PreconditionFailed => "PreconditionFailed",
            S3ErrorCode::NotModified => "NotModified",
            S3ErrorCode::NotImplemented => "NotImplemented",
        }
    }

    pub fn http_status(&self) -> u16 {
        match self {
            S3ErrorCode::InvalidObjectState => 403,
            S3ErrorCode::InvalidArgument => 400,
            S3ErrorCode::RestoreAlreadyInProgress => 409,
            S3ErrorCode::GlacierExpeditedRetrievalNotAvailable => 503,
            S3ErrorCode::SlowDown => 503,
            S3ErrorCode::ServiceUnavailable => 503,
            S3ErrorCode::TooManyRequests => 429,
            S3ErrorCode::NoSuchKey => 404,
            S3ErrorCode::NoSuchBucket => 404,
            S3ErrorCode::PreconditionFailed => 412,
            S3ErrorCode::NotModified => 304,
            S3ErrorCode::NotImplemented => 501,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3ErrorResponse<'a> {
    pub code: S3ErrorCode,
    pub message: &'a str,
    pub resource: &'a str,
}

impl<'a> S3ErrorResponse<'a> {
    pub fn to_xml(&self) -> String {
        let message = escape_xml(self.message);
        let resource = escape_xml(self.resource);
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>{}</Code><Message>{}</Message><Resource>{}</Resource></Error>",
            self.code.as_str(),
            message,
            resource,
        )
    }
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// 生成 x-amz-restore 响应头
pub fn format_restore_header(ongoing: bool, expiry_date: Option<&str>) -> String {
    if ongoing {
        "ongoing-request=\"true\"".to_string()
    } else if let Some(date) = expiry_date {
        format!("ongoing-request=\"false\", expiry-date=\"{date}\"")
    } else {
        "ongoing-request=\"false\"".to_string()
    }
}

pub fn is_restore_request(query: Option<&str>) -> bool {
    query
        .unwrap_or_default()
        .split('&')
        .any(|item| item == "restore" || item.starts_with("restore="))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreRequest {
    pub days: u32,
    pub tier: common::RestoreTier,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreParseError {
    InvalidBody,
    InvalidDays,
    InvalidTier,
}

fn find_xml_value(source: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let open_pos = source.find(&open)?;
    let value_start = open_pos + open.len();
    let value_end_rel = source[value_start..].find(&close)?;
    let value_end = value_start + value_end_rel;
    Some(source[value_start..value_end].trim().to_string())
}

fn parse_tier(raw: &str) -> Option<common::RestoreTier> {
    match raw.trim().to_lowercase().as_str() {
        "expedited" => Some(common::RestoreTier::Expedited),
        "standard" => Some(common::RestoreTier::Standard),
        "bulk" => Some(common::RestoreTier::Bulk),
        _ => None,
    }
}

pub fn parse_restore_request(
    query: &HashMap<String, String>,
    body: &[u8],
) -> Result<RestoreRequest, RestoreParseError> {
    let body_xml = if body.is_empty() {
        None
    } else {
        Some(
            std::str::from_utf8(body)
                .map_err(|_| RestoreParseError::InvalidBody)?
                .to_string(),
        )
    };

    let days = match body_xml
        .as_deref()
        .and_then(|body| find_xml_value(body, "Days"))
    {
        Some(days_raw) => {
            let days = days_raw
                .parse::<u32>()
                .map_err(|_| RestoreParseError::InvalidDays)?;
            if days == 0 || days > MAX_RESTORE_DAYS {
                return Err(RestoreParseError::InvalidDays);
            }
            days
        }
        None => {
            let days = query
                .get("days")
                .map(|days| days.parse::<u32>())
                .transpose()
                .map_err(|_| RestoreParseError::InvalidDays)?
                .unwrap_or(1);
            if days == 0 || days > MAX_RESTORE_DAYS {
                return Err(RestoreParseError::InvalidDays);
            }
            days
        }
    };

    let tier = match body_xml
        .as_deref()
        .and_then(|body| find_xml_value(body, "Tier"))
    {
        Some(raw_tier) => parse_tier(&raw_tier).ok_or(RestoreParseError::InvalidTier)?,
        None => {
            let raw_tier = query.get("tier").map(String::as_str).unwrap_or("Standard");
            parse_tier(raw_tier).ok_or(RestoreParseError::InvalidTier)?
        }
    };

    Ok(RestoreRequest { days, tier })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn restore_header_formats_completed_state() {
        let value = format_restore_header(false, Some("Fri, 28 Feb 2025 12:00:00 GMT"));
        assert!(value.contains("ongoing-request=\"false\""));
        assert!(value.contains("expiry-date=\"Fri, 28 Feb 2025 12:00:00 GMT\""));
    }

    #[test]
    fn restore_query_is_detected() {
        assert!(is_restore_request(Some("restore")));
        assert!(is_restore_request(Some("foo=bar&restore=true")));
        assert!(!is_restore_request(Some("foo=bar")));
    }

    #[test]
    fn restore_request_parse_xml_body() {
        let mut query = HashMap::new();
        query.insert("restore".into(), "true".into());
        let request = parse_restore_request(
            &query,
            b"<RestoreRequest><Days>10</Days><GlacierJobParameters><Tier>Expedited</Tier></GlacierJobParameters></RestoreRequest>",
        )
        .expect("parse restore request");
        assert_eq!(request.days, 10);
        assert_eq!(request.tier, common::RestoreTier::Expedited);
    }

    #[test]
    fn restore_request_parse_xml_body_case_insensitive_tier() {
        let mut query = HashMap::new();
        query.insert("restore".into(), "true".into());
        let request = parse_restore_request(
            &query,
            b"<RestoreRequest><Days>10</Days><GlacierJobParameters><Tier>bulk</Tier></GlacierJobParameters></RestoreRequest>",
        )
        .expect("parse restore request");
        assert_eq!(request.tier, common::RestoreTier::Bulk);
    }

    #[test]
    fn restore_request_rejects_too_many_days() {
        let mut query = HashMap::new();
        query.insert("restore".into(), "true".into());
        assert_eq!(
            parse_restore_request(&query, b"<RestoreRequest><Days>366</Days></RestoreRequest>")
                .unwrap_err(),
            RestoreParseError::InvalidDays
        );
    }

    #[test]
    fn restore_request_fallback_to_query_parameters() {
        let mut query = HashMap::new();
        query.insert("restore".into(), "true".into());
        query.insert("days".into(), "3".into());
        query.insert("tier".into(), "Bulk".into());
        let request = parse_restore_request(&query, b"").expect("parse restore request");
        assert_eq!(request.days, 3);
        assert_eq!(request.tier, common::RestoreTier::Bulk);
    }

    #[test]
    fn restore_request_treats_invalid_tier_as_error() {
        let mut query = HashMap::new();
        query.insert("restore".into(), "true".into());
        query.insert("tier".into(), "Turbo".into());
        assert_eq!(
            parse_restore_request(&query, b"").unwrap_err(),
            RestoreParseError::InvalidTier
        );
    }

    #[test]
    fn s3_error_xml_contains_code_and_resource() {
        let xml = S3ErrorResponse {
            code: S3ErrorCode::NotImplemented,
            message: "operation is not implemented",
            resource: "/docs/readme.txt",
        }
        .to_xml();
        assert!(xml.contains("<Code>NotImplemented</Code>"));
        assert!(xml.contains("<Resource>/docs/readme.txt</Resource>"));
    }
}
