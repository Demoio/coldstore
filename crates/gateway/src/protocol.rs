//! S3 冷归档协议适配层
//!
//! 职责:
//!   - StorageClass 映射: 所有对象写入即 ColdPending
//!   - RestoreObject 请求解析 (Days, Tier)
//!   - x-amz-restore 响应头生成
//!   - 错误码映射 (InvalidObjectState, RestoreAlreadyInProgress 等)
//!   - GET 行为控制 (冷对象需先 Restore)

/// S3 错误码
pub enum S3ErrorCode {
    InvalidObjectState,
    RestoreAlreadyInProgress,
    GlacierExpeditedRetrievalNotAvailable,
    NoSuchKey,
    NoSuchBucket,
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
        }
    }

    pub fn http_status(&self) -> u16 {
        match self {
            S3ErrorCode::InvalidObjectState => 403,
            S3ErrorCode::RestoreAlreadyInProgress => 409,
            S3ErrorCode::GlacierExpeditedRetrievalNotAvailable => 503,
            S3ErrorCode::NoSuchKey => 404,
            S3ErrorCode::NoSuchBucket => 404,
        }
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
