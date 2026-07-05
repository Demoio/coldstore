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
    #[serde(default = "default_cache_addrs")]
    pub cache_addrs: Vec<String>,
    #[serde(default = "default_tape_addrs")]
    pub tape_addrs: Vec<String>,
    pub archive: ArchiveSchedulerConfig,
    pub recall: RecallSchedulerConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveSchedulerConfig {
    #[serde(default = "default_scheduler_enabled")]
    pub enabled: bool,
    pub scan_interval_secs: u64,
    pub batch_size: usize,
    #[serde(default = "default_scheduler_drive_id")]
    pub drive_id: String,
    #[serde(default = "default_scheduler_tape_id")]
    pub tape_id: String,
    #[serde(default = "default_scheduler_tape_set")]
    pub tape_set: Vec<String>,
    pub min_archive_size_mb: u64,
    pub max_archive_size_mb: u64,
    pub target_throughput_mbps: u64,
    pub aggregation_window_secs: u64,
    pub write_buffer_mb: u64,
    pub block_size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallSchedulerConfig {
    #[serde(default = "default_scheduler_enabled")]
    pub enabled: bool,
    #[serde(default = "default_recall_scan_interval_secs")]
    pub scan_interval_secs: u64,
    pub max_concurrent_restores: usize,
    pub merge_window_secs: u64,
    pub restore_timeout_secs: u64,
    pub read_buffer_mb: u64,
    #[serde(default = "default_scheduler_drive_id")]
    pub drive_id: String,
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
            cache_addrs: default_cache_addrs(),
            tape_addrs: default_tape_addrs(),
            archive: ArchiveSchedulerConfig {
                enabled: true,
                scan_interval_secs: 60,
                batch_size: 1000,
                drive_id: default_scheduler_drive_id(),
                tape_id: default_scheduler_tape_id(),
                tape_set: default_scheduler_tape_set(),
                min_archive_size_mb: 100,
                max_archive_size_mb: 10240,
                target_throughput_mbps: 300,
                aggregation_window_secs: 300,
                write_buffer_mb: 128,
                block_size: 262144,
            },
            recall: RecallSchedulerConfig {
                enabled: true,
                scan_interval_secs: default_recall_scan_interval_secs(),
                max_concurrent_restores: 10,
                merge_window_secs: 60,
                restore_timeout_secs: 3600,
                read_buffer_mb: 64,
                drive_id: default_scheduler_drive_id(),
            },
        }
    }
}

fn default_cache_addrs() -> Vec<String> {
    vec!["127.0.0.1:23001".to_string()]
}

fn default_tape_addrs() -> Vec<String> {
    vec!["127.0.0.1:24001".to_string()]
}

fn default_scheduler_enabled() -> bool {
    true
}

fn default_recall_scan_interval_secs() -> u64 {
    60
}

fn default_scheduler_drive_id() -> String {
    "drive-0".to_string()
}

fn default_scheduler_tape_id() -> String {
    "TAPE0001".to_string()
}

fn default_scheduler_tape_set() -> Vec<String> {
    vec![default_scheduler_tape_id()]
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
    #[serde(default)]
    pub simulator: TapeSimulatorConfig,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TapeSimulatorConfig {
    #[serde(default = "default_simulator_slot_count")]
    pub slot_count: u32,
    #[serde(default = "default_simulator_tape_ids")]
    pub tape_ids: Vec<String>,
    #[serde(default = "default_simulator_autoload_tape_id")]
    pub autoload_tape_id: Option<String>,
}

impl Default for TapeSimulatorConfig {
    fn default() -> Self {
        Self {
            slot_count: default_simulator_slot_count(),
            tape_ids: default_simulator_tape_ids(),
            autoload_tape_id: default_simulator_autoload_tape_id(),
        }
    }
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
            sdk_backend: "simulator".to_string(),
            scsi: ScsiConfig {
                devices: vec!["/dev/nst0".to_string()],
                block_size: 262144,
                buffer_size_mb: 64,
            },
            simulator: TapeSimulatorConfig::default(),
            library_device: None,
            supported_formats: vec!["LTO-9".to_string(), "LTO-10".to_string()],
            tape_hold_secs: 300,
            drive_acquire_timeout_secs: 600,
        }
    }
}

fn default_simulator_slot_count() -> u32 {
    8
}

fn default_simulator_tape_ids() -> Vec<String> {
    vec![default_scheduler_tape_id()]
}

fn default_simulator_autoload_tape_id() -> Option<String> {
    Some(default_scheduler_tape_id())
}
