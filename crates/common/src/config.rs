use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
//  Gateway 配置
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    pub listen: String,
    pub scheduler_addrs: Vec<String>,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:9000".to_string(),
            scheduler_addrs: vec!["127.0.0.1:22001".to_string()],
        }
    }
}

// ---------------------------------------------------------------------------
//  Metadata 节点配置
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataConfig {
    pub node_id: u64,
    pub listen: String,
    pub cluster: String,
    pub data_path: String,
    pub rocksdb: RocksDbConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RocksDbConfig {
    pub max_open_files: i32,
    pub write_buffer_size_mb: u64,
    pub max_background_jobs: i32,
}

impl Default for MetadataConfig {
    fn default() -> Self {
        Self {
            node_id: 1,
            listen: "0.0.0.0:21001".to_string(),
            cluster: "1:127.0.0.1:21001,2:127.0.0.1:21002,3:127.0.0.1:21003".to_string(),
            data_path: "/var/lib/coldstore/metadata".to_string(),
            rocksdb: RocksDbConfig {
                max_open_files: 1024,
                write_buffer_size_mb: 64,
                max_background_jobs: 4,
            },
        }
    }
}

// ---------------------------------------------------------------------------
//  Scheduler Worker 配置
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerConfig {
    pub listen: String,
    pub metadata_addrs: Vec<String>,
    pub cache_addrs: Vec<String>,
    pub archive: ArchiveSchedulerConfig,
    pub recall: RecallSchedulerConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveSchedulerConfig {
    pub scan_interval_secs: u64,
    pub batch_size: usize,
    pub min_archive_size_mb: u64,
    pub max_archive_size_mb: u64,
    pub target_throughput_mbps: u64,
    pub aggregation_window_secs: u64,
    pub write_buffer_mb: u64,
    pub block_size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallSchedulerConfig {
    pub max_concurrent_restores: usize,
    pub merge_window_secs: u64,
    pub restore_timeout_secs: u64,
    pub read_buffer_mb: u64,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:22001".to_string(),
            metadata_addrs: vec![
                "127.0.0.1:21001".to_string(),
                "127.0.0.1:21002".to_string(),
                "127.0.0.1:21003".to_string(),
            ],
            cache_addrs: vec!["127.0.0.1:23001".to_string()],
            archive: ArchiveSchedulerConfig {
                scan_interval_secs: 60,
                batch_size: 1000,
                min_archive_size_mb: 100,
                max_archive_size_mb: 10240,
                target_throughput_mbps: 300,
                aggregation_window_secs: 300,
                write_buffer_mb: 128,
                block_size: 262144,
            },
            recall: RecallSchedulerConfig {
                max_concurrent_restores: 10,
                merge_window_secs: 60,
                restore_timeout_secs: 3600,
                read_buffer_mb: 64,
            },
        }
    }
}

// ---------------------------------------------------------------------------
//  Cache Worker 配置
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    pub listen: String,
    pub metadata_addrs: Vec<String>,
    pub backend: CacheBackendConfig,
    pub default_ttl_secs: u64,
    pub eviction_policy: String,
    pub eviction_batch_size: usize,
    pub eviction_low_watermark: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CacheBackendConfig {
    #[serde(rename = "hdd")]
    Hdd { path: String, max_size_gb: u64 },
    #[serde(rename = "spdk")]
    Spdk {
        config_file: String,
        bdev_name: String,
        max_size_gb: u64,
        cluster_size_mb: u32,
    },
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:23001".to_string(),
            metadata_addrs: vec![
                "127.0.0.1:21001".to_string(),
                "127.0.0.1:21002".to_string(),
                "127.0.0.1:21003".to_string(),
            ],
            backend: CacheBackendConfig::Hdd {
                path: "/var/lib/coldstore/cache".to_string(),
                max_size_gb: 100,
            },
            default_ttl_secs: 86400,
            eviction_policy: "Lru".to_string(),
            eviction_batch_size: 64,
            eviction_low_watermark: 0.8,
        }
    }
}

// ---------------------------------------------------------------------------
//  Tape Worker 配置
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TapeConfig {
    pub listen: String,
    pub metadata_addrs: Vec<String>,
    pub sdk_backend: String,
    pub scsi: ScsiConfig,
    pub library_device: Option<String>,
    pub supported_formats: Vec<String>,
    pub tape_hold_secs: u64,
    pub drive_acquire_timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScsiConfig {
    pub devices: Vec<String>,
    pub block_size: u32,
    pub buffer_size_mb: u64,
}

impl Default for TapeConfig {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:24001".to_string(),
            metadata_addrs: vec![
                "127.0.0.1:21001".to_string(),
                "127.0.0.1:21002".to_string(),
                "127.0.0.1:21003".to_string(),
            ],
            sdk_backend: "scsi".to_string(),
            scsi: ScsiConfig {
                devices: vec!["/dev/nst0".to_string()],
                block_size: 262144,
                buffer_size_mb: 64,
            },
            library_device: None,
            supported_formats: vec!["LTO-9".to_string(), "LTO-10".to_string()],
            tape_hold_secs: 300,
            drive_acquire_timeout_secs: 600,
        }
    }
}
