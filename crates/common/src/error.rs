use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Error, Debug)]
pub enum Error {
    #[error("配置错误: {0}")]
    Config(#[from] config::ConfigError),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("序列化错误: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("gRPC 错误: {0}")]
    Grpc(#[from] tonic::Status),

    #[error("gRPC 传输错误: {0}")]
    Transport(#[from] tonic::transport::Error),

    #[error("S3 协议错误: {code}: {message}")]
    S3Protocol { code: String, message: String },

    #[error("对象不存在: {bucket}/{key}")]
    ObjectNotFound { bucket: String, key: String },

    #[error("桶不存在: {0}")]
    BucketNotFound(String),

    #[error("对象状态无效: {0}")]
    InvalidObjectState(String),

    #[error("磁带离线: {0}")]
    TapeOffline(String),

    #[error("驱动不可用: {0}")]
    DriveUnavailable(String),

    #[error("缓存未命中: {bucket}/{key}")]
    CacheMiss { bucket: String, key: String },

    #[error("容量不足: {0}")]
    InsufficientCapacity(String),

    #[error("状态流转无效: {from:?} -> {to:?}")]
    InvalidStateTransition { from: String, to: String },

    #[error("内部错误: {0}")]
    Internal(String),
}

impl From<Error> for tonic::Status {
    fn from(err: Error) -> Self {
        match &err {
            Error::ObjectNotFound { .. } => tonic::Status::not_found(err.to_string()),
            Error::BucketNotFound(_) => tonic::Status::not_found(err.to_string()),
            Error::InvalidObjectState(_) => tonic::Status::failed_precondition(err.to_string()),
            Error::TapeOffline(_) => tonic::Status::unavailable(err.to_string()),
            Error::DriveUnavailable(_) => tonic::Status::unavailable(err.to_string()),
            Error::CacheMiss { .. } => tonic::Status::not_found(err.to_string()),
            Error::InsufficientCapacity(_) => tonic::Status::resource_exhausted(err.to_string()),
            Error::InvalidStateTransition { .. } => {
                tonic::Status::failed_precondition(err.to_string())
            }
            _ => tonic::Status::internal(err.to_string()),
        }
    }
}
