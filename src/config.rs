use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub metadata: MetadataConfig,
    pub scheduler: SchedulerConfig,
    pub cache: CacheConfig,
    pub tape: TapeConfig,
    pub notification: NotificationConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub s3_endpoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataConfig {
    pub backend: MetadataBackend,
    pub postgres: Option<PostgresConfig>,
    pub etcd: Option<EtcdConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MetadataBackend {
    Postgres,
    Etcd,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostgresConfig {
    pub url: String,
    pub max_connections: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EtcdConfig {
    pub endpoints: Vec<String>,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerConfig {
    pub archive: ArchiveSchedulerConfig,
    pub recall: RecallSchedulerConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveSchedulerConfig {
    pub scan_interval_secs: u64,
    pub batch_size: usize,
    pub min_archive_size_mb: u64,
    pub target_throughput_mbps: u64, // 目标吞吐量 MB/s
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallSchedulerConfig {
    pub queue_size: usize,
    pub max_concurrent_restores: usize,
    pub restore_timeout_secs: u64,
    pub min_restore_interval_secs: u64, // 最小取回间隔（5分钟）
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    pub enabled: bool,
    pub path: PathBuf,
    pub max_size_gb: u64,
    pub ttl_secs: u64,
    pub eviction_policy: EvictionPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EvictionPolicy {
    Lru,
    Lfu,
    Ttl,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TapeConfig {
    pub library_path: Option<String>,
    pub supported_formats: Vec<String>, // e.g., ["LTO-9", "LTO-10"]
    pub replication_factor: u32,
    pub verify_readability: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationConfig {
    pub enabled: bool,
    pub webhook_url: Option<String>,
    pub mq_endpoint: Option<String>,
}

impl Config {
    pub fn load() -> crate::error::Result<Self> {
        let config_path =
            std::env::var("COLDSTORE_CONFIG").unwrap_or_else(|_| "config/config.yaml".to_string());

        let settings = config::Config::builder()
            .add_source(config::File::with_name(&config_path).required(false))
            .add_source(config::Environment::with_prefix("COLDSTORE"))
            .build()?;

        Ok(settings.try_deserialize()?)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig {
                host: "0.0.0.0".to_string(),
                port: 9000,
                s3_endpoint: "http://localhost:9000".to_string(),
            },
            metadata: MetadataConfig {
                backend: MetadataBackend::Postgres,
                postgres: Some(PostgresConfig {
                    url: "postgresql://localhost/coldstore".to_string(),
                    max_connections: 10,
                }),
                etcd: None,
            },
            scheduler: SchedulerConfig {
                archive: ArchiveSchedulerConfig {
                    scan_interval_secs: 3600,
                    batch_size: 1000,
                    min_archive_size_mb: 100,
                    target_throughput_mbps: 300,
                },
                recall: RecallSchedulerConfig {
                    queue_size: 10000,
                    max_concurrent_restores: 10,
                    restore_timeout_secs: 3600,
                    min_restore_interval_secs: 300, // 5分钟
                },
            },
            cache: CacheConfig {
                enabled: true,
                path: PathBuf::from("/var/cache/coldstore"),
                max_size_gb: 100,
                ttl_secs: 86400, // 24小时
                eviction_policy: EvictionPolicy::Lru,
            },
            tape: TapeConfig {
                library_path: None,
                supported_formats: vec!["LTO-9".to_string(), "LTO-10".to_string()],
                replication_factor: 2,
                verify_readability: true,
            },
            notification: NotificationConfig {
                enabled: true,
                webhook_url: None,
                mq_endpoint: None,
            },
        }
    }
}
