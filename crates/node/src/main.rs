use anyhow::Result;
use coldstore_common::config::{
    CacheBackendConfig, CacheConfig, GatewayConfig, MetadataConfig, SchedulerConfig, TapeConfig,
};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};
use tracing::{error, info, warn};

const METADATA_ADDR: &str = "127.0.0.1:21001";
const SCHEDULER_ADDR: &str = "127.0.0.1:22001";
const CACHE_ADDR: &str = "127.0.0.1:23001";
const TAPE_ADDR: &str = "127.0.0.1:24001";
const GATEWAY_ADDR: &str = "127.0.0.1:9000";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "coldstore_node=info,coldstore_metadata=info,coldstore_cache=info,coldstore_tape=info,coldstore_scheduler=info,coldstore_gateway=info".into()
            }),
        )
        .init();

    let runtime = load_runtime_config()?;
    let data_dir = runtime.data_dir.clone();
    tokio::fs::create_dir_all(&data_dir).await?;

    let configs = single_node_configs(&data_dir, runtime.tape_enabled);
    info!(
        data_dir = %data_dir.display(),
        gateway = GATEWAY_ADDR,
        tape_enabled = runtime.tape_enabled,
        "starting ColdStore single-node runtime"
    );

    let mut tasks = Vec::new();
    tasks.push(spawn_component(
        "metadata",
        coldstore_metadata::run(configs.metadata),
    ));
    tasks.push(spawn_component(
        "cache",
        coldstore_cache::run(configs.cache),
    ));
    if let Some(tape) = configs.tape {
        tasks.push(spawn_component("tape", coldstore_tape::run(tape)));
    } else {
        info!("Tape Worker disabled by config; archive and recall loops will not run");
    }

    sleep(Duration::from_millis(300)).await;
    tasks.push(spawn_component(
        "scheduler",
        coldstore_scheduler::run(configs.scheduler),
    ));

    sleep(Duration::from_millis(300)).await;
    tasks.push(spawn_component(
        "gateway",
        coldstore_gateway::run(configs.gateway),
    ));

    info!("ColdStore single-node runtime is listening on http://{GATEWAY_ADDR}");
    tokio::signal::ctrl_c().await?;
    info!("shutting down ColdStore single-node runtime");
    for task in tasks {
        task.abort();
    }
    Ok(())
}

struct SingleNodeConfigs {
    metadata: MetadataConfig,
    cache: CacheConfig,
    tape: Option<TapeConfig>,
    scheduler: SchedulerConfig,
    gateway: GatewayConfig,
}

fn single_node_configs(data_dir: &Path, tape_enabled: bool) -> SingleNodeConfigs {
    let metadata = MetadataConfig {
        listen: METADATA_ADDR.to_string(),
        cluster: format!("1:{METADATA_ADDR}"),
        data_path: data_dir.join("metadata").display().to_string(),
        ..MetadataConfig::default()
    };
    let cache = CacheConfig {
        listen: CACHE_ADDR.to_string(),
        metadata_addrs: vec![METADATA_ADDR.to_string()],
        backend: CacheBackendConfig::Hdd {
            path: data_dir.join("cache").display().to_string(),
            max_size_gb: 100,
        },
        ..CacheConfig::default()
    };
    let tape = tape_enabled.then(|| TapeConfig {
        listen: TAPE_ADDR.to_string(),
        metadata_addrs: vec![METADATA_ADDR.to_string()],
        ..TapeConfig::default()
    });
    let mut scheduler = SchedulerConfig {
        listen: SCHEDULER_ADDR.to_string(),
        metadata_addrs: vec![METADATA_ADDR.to_string()],
        cache_addrs: vec![CACHE_ADDR.to_string()],
        tape_addrs: if tape_enabled {
            vec![TAPE_ADDR.to_string()]
        } else {
            vec![]
        },
        ..SchedulerConfig::default()
    };
    scheduler.archive.scan_interval_secs = 2;
    scheduler.archive.batch_size = 128;
    scheduler.recall.scan_interval_secs = 2;
    scheduler.recall.max_concurrent_restores = 4;
    if !tape_enabled {
        scheduler.archive.enabled = false;
        scheduler.recall.enabled = false;
    }

    let gateway = GatewayConfig {
        listen: GATEWAY_ADDR.to_string(),
        scheduler_addrs: vec![SCHEDULER_ADDR.to_string()],
    };

    SingleNodeConfigs {
        metadata,
        cache,
        tape,
        scheduler,
        gateway,
    }
}

#[derive(Debug, Clone, Deserialize)]
struct NodeRuntimeConfig {
    #[serde(default = "default_data_dir")]
    data_dir: PathBuf,
    #[serde(default = "default_tape_enabled")]
    tape_enabled: bool,
}

impl Default for NodeRuntimeConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            tape_enabled: default_tape_enabled(),
        }
    }
}

fn load_runtime_config() -> Result<NodeRuntimeConfig> {
    let mut runtime = if let Some(path) = config_path()? {
        let parsed = config::Config::builder()
            .add_source(config::File::from(path.clone()))
            .build()?
            .try_deserialize()?;
        info!(config = %path.display(), "loaded ColdStore node config");
        parsed
    } else {
        NodeRuntimeConfig::default()
    };

    if let Ok(data_dir) = std::env::var("COLDSTORE_DATA_DIR") {
        runtime.data_dir = PathBuf::from(data_dir);
    }
    if no_tape_override() {
        warn!("overriding config: Tape Worker disabled by --no-tape or COLDSTORE_NO_TAPE");
        runtime.tape_enabled = false;
    }
    Ok(runtime)
}

fn config_path() -> Result<Option<PathBuf>> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--config" {
            return args
                .next()
                .map(PathBuf::from)
                .map(Some)
                .ok_or_else(|| anyhow::anyhow!("--config requires a path"));
        }
        if let Some(path) = arg.strip_prefix("--config=") {
            return Ok(Some(PathBuf::from(path)));
        }
    }

    if let Ok(path) = std::env::var("COLDSTORE_CONFIG") {
        return Ok(Some(PathBuf::from(path)));
    }
    Ok(None)
}

fn no_tape_override() -> bool {
    std::env::args().any(|arg| arg == "--no-tape")
        || matches!(
            std::env::var("COLDSTORE_NO_TAPE").as_deref(),
            Ok("1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON")
        )
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("data/single-node")
}

fn default_tape_enabled() -> bool {
    true
}

fn spawn_component<F>(name: &'static str, future: F) -> JoinHandle<()>
where
    F: std::future::Future<Output = Result<()>> + Send + 'static,
{
    tokio::spawn(async move {
        if let Err(err) = future.await {
            error!(component = name, error = %err, "ColdStore component exited");
        }
    })
}
