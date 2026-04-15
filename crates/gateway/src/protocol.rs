//! S3 冷归档协议适配层
//!
//! 职责:
//!   - StorageClass 映射: 所有对象写入即 ColdPending
//!   - RestoreObject 请求解析 (Days, Tier)
//!   - x-amz-restore 响应头生成
//!   - 错误码映射 (InvalidObjectState, RestoreAlreadyInProgress 等)
//!   - GET 行为控制 (冷对象需先 Restore)

/// S3 错误码
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S3ErrorCode {
    InvalidObjectState,
    RestoreAlreadyInProgress,
    GlacierExpeditedRetrievalNotAvailable,
    NoSuchKey,
    NoSuchBucket,
    NotImplemented,
}

impl S3ErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            S3ErrorCode::InvalidObjectState => "InvalidObjectState",
            S3ErrorCode::RestoreAlreadyInProgress => "RestoreAlreadyInProgress",
            S3ErrorCode::GlacierExpeditedRetrievalNotAvailable => {
                "GlacierExpeditedRetrievalNotAvailable"
            }
            S3ErrorCode::NoSuchKey => "NoSuchKey",
            S3ErrorCode::NoSuchBucket => "NoSuchBucket",
            S3ErrorCode::NotImplemented => "NotImplemented",
        }
    }

    pub fn http_status(&self) -> u16 {
        match self {
            S3ErrorCode::InvalidObjectState => 403,
            S3ErrorCode::RestoreAlreadyInProgress => 409,
            S3ErrorCode::GlacierExpeditedRetrievalNotAvailable => 503,
            S3ErrorCode::NoSuchKey => 404,
            S3ErrorCode::NoSuchBucket => 404,
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
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>{}</Code><Message>{}</Message><Resource>{}</Resource></Error>",
            self.code.as_str(),
            self.message,
            self.resource,
        )
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
