use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Error, Debug)]
pub enum Error {
    #[error("配置错误: {0}")]
    Config(#[from] config::ConfigError),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("数据库错误: {0}")]
    Database(#[from] sqlx::Error),

    #[error("序列化错误: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("S3 协议错误: {0}")]
    S3(String),

    #[error("元数据错误: {0}")]
    Metadata(String),

    #[error("调度器错误: {0}")]
    Scheduler(String),

    #[error("磁带操作错误: {0}")]
    Tape(String),

    #[error("缓存错误: {0}")]
    Cache(String),

    #[error("通知错误: {0}")]
    Notification(String),

    #[error("对象不存在")]
    ObjectNotFound,

    #[error("对象状态无效: {0}")]
    InvalidObjectState(String),

    #[error("磁带离线: {0}")]
    TapeOffline(String),

    #[error("内部错误: {0}")]
    Internal(String),
}
