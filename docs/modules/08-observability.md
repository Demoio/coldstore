# 可观测性与链路追踪设计

> 所属架构：ColdStore 冷存储系统  
> 参考：[总架构设计](../DESIGN.md) | [07-一致性与性能](./07-consistency-performance.md)

## 1. 设计目标

ColdStore 的可观测性基于 **OpenTelemetry** 统一标准，覆盖三大支柱：

| 支柱 | 用途 | 技术 |
|------|------|------|
| **Traces** | 跨层链路追踪，定位延迟瓶颈 | `tracing` + `tracing-opentelemetry` |
| **Metrics** | 实时指标监控，容量规划 | `opentelemetry` Meter API |
| **Logs** | 结构化日志，故障排查 | `tracing` + JSON formatter |

### 1.1 核心挑战

| 挑战 | 原因 | 解决方案 |
|------|------|----------|
| 同步请求 → 异步调度 | RestoreObject 请求后，取回在后台异步执行 | Span Link 关联请求 trace 与调度 trace |
| SPDK 用户态线程 | SPDK reactor 线程不在 Tokio 运行时内 | 跨线程 Context 显式传播 |
| 磁带操作超长耗时 | 单次取回可能 30 分钟+ | 长 Span 分段记录，Event 标记关键节点 |
| 高吞吐 GET 路径 | GET 缓存命中需 < 1ms，不能有过大开销 | 采样策略，热路径最小化 instrumentation |

---

## 2. 技术选型

### 2.1 Crate 依赖

```toml
[dependencies]
# ─── OpenTelemetry 核心 ───
opentelemetry = "0.24"
opentelemetry_sdk = { version = "0.24", features = ["rt-tokio"] }
opentelemetry-otlp = { version = "0.17", features = ["tonic", "metrics", "logs"] }
opentelemetry-semantic-conventions = "0.16"

# ─── tracing 生态 ───
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
tracing-opentelemetry = "0.25"

# ─── Metrics 导出 ───
opentelemetry-prometheus = "0.17"      # Prometheus pull 模式（可选）
prometheus = "0.13"                     # Prometheus client（可选）
```

### 2.2 导出架构

```
┌──────────────────────────────────────────────────────────────────────────┐
│                           ColdStore 进程                                  │
│                                                                          │
│  ┌─────────┐  ┌─────────┐  ┌─────────┐  ┌─────────┐  ┌─────────┐     │
│  │ 接入层  │  │ 元数据  │  │ 缓存层  │  │ 调度层  │  │ 磁带层  │     │
│  └────┬────┘  └────┬────┘  └────┬────┘  └────┬────┘  └────┬────┘     │
│       │            │            │            │            │            │
│       └────────────┴────────────┴────────────┴────────────┘            │
│                              │                                         │
│               ┌──────────────┼──────────────┐                         │
│               ▼              ▼              ▼                         │
│         TracerProvider  MeterProvider  LoggerProvider                  │
│               │              │              │                         │
│               └──────────────┼──────────────┘                         │
│                              ▼                                         │
│                     OTLP gRPC Exporter                                │
└──────────────────────────────┬─────────────────────────────────────────┘
                               │
                               ▼
                 ┌─────────────────────────┐
                 │  OpenTelemetry Collector │
                 │  (otel-collector)        │
                 └─────────┬───────────────┘
                           │
              ┌────────────┼────────────┐
              ▼            ▼            ▼
        ┌──────────┐ ┌──────────┐ ┌──────────┐
        │ Jaeger / │ │Prometheus│ │  Loki /  │
        │ Tempo    │ │          │ │  ES      │
        │ (Traces) │ │ (Metrics)│ │ (Logs)   │
        └──────────┘ └──────────┘ └──────────┘
              │            │            │
              └────────────┼────────────┘
                           ▼
                     ┌──────────┐
                     │ Grafana  │
                     │ Dashboard│
                     └──────────┘
```

### 2.3 初始化

```rust
use opentelemetry::KeyValue;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{runtime, trace, Resource};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

pub struct TelemetryConfig {
    pub service_name: String,
    pub otlp_endpoint: String,
    pub node_id: u64,
    pub environment: String,
    pub trace_sample_rate: f64,
    pub log_level: String,
    pub metrics_export_interval_secs: u64,
}

pub fn init_telemetry(config: &TelemetryConfig) -> Result<TelemetryGuard> {
    let resource = Resource::new(vec![
        KeyValue::new("service.name", config.service_name.clone()),
        KeyValue::new("service.instance.id", format!("node-{}", config.node_id)),
        KeyValue::new("deployment.environment", config.environment.clone()),
        KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
    ]);

    // ── Traces ──
    let tracer = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint(&config.otlp_endpoint),
        )
        .with_trace_config(
            trace::Config::default()
                .with_resource(resource.clone())
                .with_sampler(trace::Sampler::TraceIdRatioBased(config.trace_sample_rate)),
        )
        .install_batch(runtime::Tokio)?;

    // ── Metrics ──
    let meter_provider = opentelemetry_otlp::new_pipeline()
        .metrics(runtime::Tokio)
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint(&config.otlp_endpoint),
        )
        .with_resource(resource.clone())
        .with_period(Duration::from_secs(config.metrics_export_interval_secs))
        .build()?;

    opentelemetry::global::set_meter_provider(meter_provider);

    // ── 组装 tracing subscriber ──
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    let fmt_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_target(true)
        .with_thread_ids(true)
        .with_span_list(true);

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&config.log_level));

    tracing_subscriber::registry()
        .with(filter)
        .with(otel_layer)
        .with(fmt_layer)
        .init();

    Ok(TelemetryGuard { /* shutdown handles */ })
}

pub struct TelemetryGuard {
    // Drop 时调用 opentelemetry::global::shutdown_tracer_provider()
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        opentelemetry::global::shutdown_tracer_provider();
    }
}
```

---

## 3. 分布式追踪设计

### 3.1 Trace 拓扑总览

ColdStore 的操作分为**同步请求链路**和**异步调度链路**两类，通过 **Span Link** 关联：

```
═══ 同步请求 Trace ═══

[PUT Object]                                    Trace A
  └─ [parse_request]
  └─ [metadata.put_object]
       └─ [raft_propose]

[GET Object]                                    Trace B
  └─ [parse_request]
  └─ [metadata.get_object]
  └─ [check_restore_status]
  └─ [cache.get]
       └─ [spdk_blob_read]

[RestoreObject]                                 Trace C
  └─ [parse_restore_request]
  └─ [metadata.find_active_recall]              ← linearizable read
  └─ [metadata.put_recall_task]
  └─ [response: 202 Accepted]


═══ 异步调度 Trace（Link → Trace C）═══

[recall_scheduler.process]                      Trace D ──Link──▶ Trace C
  └─ [dequeue_task]
  └─ [merge_by_tape]
  └─ [execute_tape_read_job]
       └─ [tape.acquire_drive]
       └─ [tape.load]
       └─ [tape.seek]
       └─ [tape.read_objects]
            └─ [read_object: obj1]
            └─ [read_object: obj2]
       └─ [cache.put_restored_batch]
            └─ [spdk_create_blob]
            └─ [spdk_write_blob]
       └─ [metadata.update_restore_status]
  └─ [tape.unload]
```

### 3.2 Span 命名规范

采用 `{layer}.{operation}` 的扁平命名，便于在 Jaeger/Tempo 中按层过滤：

| 层 | Span 名称模式 | 示例 |
|------|--------------|------|
| 接入层 | `s3.{method}` | `s3.put_object`, `s3.get_object`, `s3.restore_object` |
| 协议层 | `protocol.{operation}` | `protocol.parse_restore`, `protocol.build_response` |
| 元数据 | `metadata.{api}` | `metadata.put_object`, `metadata.scan_cold_pending` |
| Raft | `raft.{operation}` | `raft.propose`, `raft.apply`, `raft.ensure_linearizable` |
| 缓存 | `cache.{operation}` | `cache.get`, `cache.put_restored`, `cache.evict_batch` |
| SPDK | `spdk.{operation}` | `spdk.create_blob`, `spdk.write_blob`, `spdk.read_blob` |
| 调度 | `scheduler.{operation}` | `scheduler.archive_scan`, `scheduler.recall_process` |
| 磁带 | `tape.{operation}` | `tape.acquire_drive`, `tape.load`, `tape.write`, `tape.read` |

### 3.3 Span Attributes（标准化字段）

#### 3.3.1 通用 Attributes

| Attribute | 类型 | 说明 | 示例 |
|-----------|------|------|------|
| `coldstore.bucket` | string | S3 桶名 | `"my-bucket"` |
| `coldstore.key` | string | S3 对象键 | `"photos/2025/img.jpg"` |
| `coldstore.version_id` | string | 对象版本 | `"v1"` |
| `coldstore.object_size` | i64 | 对象字节数 | `104857600` |
| `coldstore.storage_class` | string | 存储类别 | `"Cold"` |
| `coldstore.node_id` | i64 | 当前节点 ID | `1` |

#### 3.3.2 元数据层 Attributes

| Attribute | 类型 | 说明 |
|-----------|------|------|
| `raft.is_leader` | bool | 当前节点是否 Leader |
| `raft.term` | i64 | 当前 Raft 任期 |
| `raft.log_index` | i64 | 写入的日志索引 |
| `raft.commit_latency_ms` | f64 | Raft 提交延迟 |
| `metadata.cf` | string | 操作的 Column Family |
| `metadata.batch_size` | i64 | WriteBatch 中的操作数 |

#### 3.3.3 缓存层 Attributes

| Attribute | 类型 | 说明 |
|-----------|------|------|
| `cache.hit` | bool | 是否命中缓存 |
| `cache.blob_id` | i64 | SPDK Blob ID |
| `cache.blob_size` | i64 | Blob 占用空间 |
| `cache.io_unit_count` | i64 | 读写的 io_unit 数 |
| `cache.usage_ratio` | f64 | 当前容量使用率 |
| `cache.eviction_reason` | string | 淘汰原因（`"ttl"` / `"capacity"` / `"lru"`） |

#### 3.3.4 调度层 Attributes

| Attribute | 类型 | 说明 |
|-----------|------|------|
| `scheduler.task_id` | string | 任务 ID (UUID) |
| `scheduler.task_type` | string | `"archive"` / `"recall"` |
| `scheduler.bundle_id` | string | ArchiveBundle ID |
| `scheduler.tier` | string | `"Expedited"` / `"Standard"` / `"Bulk"` |
| `scheduler.merge_count` | i64 | 合并的任务数 |
| `scheduler.queue_depth` | i64 | 当前队列深度 |
| `scheduler.retry_count` | i64 | 重试次数 |

#### 3.3.5 磁带层 Attributes

| Attribute | 类型 | 说明 |
|-----------|------|------|
| `tape.tape_id` | string | 磁带 ID |
| `tape.drive_id` | string | 驱动 ID |
| `tape.operation` | string | `"write"` / `"read"` / `"seek"` / `"load"` / `"unload"` |
| `tape.bytes_transferred` | i64 | 传输字节数 |
| `tape.blocks_transferred` | i64 | 传输块数 |
| `tape.filemark` | i64 | FileMark 位置 |
| `tape.seek_blocks` | i64 | seek 跳过的块数 |
| `tape.swap_duration_ms` | f64 | 换带耗时 |

### 3.4 跨异步边界 Context 传播

#### 3.4.1 问题：RestoreObject → 异步取回

`RestoreObject` 是同步 HTTP 请求（返回 202），取回在后台异步执行。两个 Trace 需通过 **Span Link** 关联：

```rust
use tracing::Span;
use opentelemetry::trace::TraceContextExt;

// ─── 协议层：RestoreObject Handler ───
#[tracing::instrument(name = "s3.restore_object", skip(state, body))]
async fn restore_object(
    state: AppState,
    bucket: &str,
    key: &str,
    body: RestoreRequest,
) -> Result<Response> {
    let recall_task = RecallTask::new(bucket, key, body.tier, body.days);

    // 捕获当前 span context，存入 RecallTask 供调度层关联
    let current_cx = Span::current().context();
    let span_context = current_cx.span().span_context().clone();
    let trace_context = serialize_span_context(&span_context);

    recall_task.trace_context = Some(trace_context);
    state.metadata.put_recall_task(recall_task).await?;

    Ok(Response::new(StatusCode::ACCEPTED))
}

// ─── 调度层：处理 RecallTask ───
#[tracing::instrument(name = "scheduler.recall_process", skip(self))]
async fn process_recall_task(&self, task: RecallTask) {
    let span = Span::current();

    // 建立 Link 到原始 RestoreObject 请求
    if let Some(ref ctx) = task.trace_context {
        if let Some(remote_cx) = deserialize_span_context(ctx) {
            span.add_link(remote_cx, vec![
                KeyValue::new("link.type", "restore_request"),
                KeyValue::new("link.recall_task_id", task.id.to_string()),
            ]);
        }
    }

    // 后续执行...
    self.execute_tape_read_job(task).await;
}
```

#### 3.4.2 问题：Tokio → SPDK reactor 线程

SPDK reactor 运行在独立线程，不在 Tokio 运行时内。需显式传递 Context：

```rust
use opentelemetry::Context;

// ─── CacheManager：在 Tokio 侧捕获 Context ───
#[tracing::instrument(name = "cache.put_restored", skip(self, data))]
async fn put_restored(&self, item: RestoredItem) -> Result<()> {
    let parent_cx = Context::current();

    // 通过 channel 发送到 SPDK reactor 线程
    let (tx, rx) = oneshot::channel();
    self.spdk_sender.send(SpdkCommand::WriteBlob {
        xattrs: item.to_xattrs(),
        data: item.data,
        parent_cx,     // 传递上下文
        reply: tx,
    })?;

    rx.await?
}

// ─── SPDK reactor 线程侧 ───
fn handle_spdk_command(cmd: SpdkCommand) {
    match cmd {
        SpdkCommand::WriteBlob { xattrs, data, parent_cx, reply } => {
            let _guard = parent_cx.attach();
            let span = tracing::info_span!("spdk.write_blob",
                cache.blob_size = data.len() as i64,
            );
            let _enter = span.enter();

            // SPDK 同步操作...
            let result = spdk_blob_write_sync(&xattrs, &data);
            let _ = reply.send(result);
        }
    }
}
```

#### 3.4.3 问题：归档扫描 → 批量磁带写入

归档调度器定时扫描产生多个 ArchiveTask，每个 Task 是独立 Trace，但共享同一 scan 的上下文：

```rust
#[tracing::instrument(name = "scheduler.archive_scan")]
async fn scan_and_archive(&self) {
    let objects = self.metadata.scan_cold_pending(self.config.batch_size).await?;
    let bundles = self.aggregate(objects);

    for bundle in bundles {
        let task_span = tracing::info_span!("scheduler.archive_task",
            scheduler.bundle_id = %bundle.id,
            scheduler.task_type = "archive",
            coldstore.object_count = bundle.entries.len() as i64,
        );

        // 每个 ArchiveTask 独立 spawn，继承 scan span 为 Link
        let metadata = self.metadata.clone();
        let tape_mgr = self.tape_manager.clone();
        let scan_cx = Span::current().context();

        tokio::spawn(async move {
            task_span.add_link(
                scan_cx.span().span_context().clone(),
                vec![KeyValue::new("link.type", "archive_scan")],
            );
            execute_archive(metadata, tape_mgr, bundle)
                .instrument(task_span)
                .await;
        });
    }
}
```

### 3.5 采样策略

| 路径 | 采样率 | 理由 |
|------|--------|------|
| `s3.get_object`（缓存命中） | 1% | 高频热路径，全量采集开销过大 |
| `s3.put_object` | 10% | 中频操作 |
| `s3.restore_object` | 100% | 低频且关键操作，需全量 |
| `scheduler.archive_*` | 100% | 低频，每个 Bundle 必须可追踪 |
| `scheduler.recall_*` | 100% | 低频且面向用户 SLA |
| `tape.*` | 100% | 低频，磁带操作需完整记录 |
| `raft.propose` | 10% | 中频，Leader 节点可能较多 |
| `cache.evict_batch` | 100% | 低频后台任务 |

**实现**：基于 Span 名称的自定义 Sampler：

```rust
pub struct ColdStoreSampler {
    default_rate: f64,
    overrides: HashMap<String, f64>,
}

impl trace::ShouldSample for ColdStoreSampler {
    fn should_sample(
        &self,
        parent_context: Option<&Context>,
        _trace_id: TraceId,
        name: &str,
        _span_kind: &SpanKind,
        _attributes: &[KeyValue],
        _links: &[Link],
    ) -> SamplingResult {
        // 父 span 已采样则继续采样（保持链路完整）
        if let Some(cx) = parent_context {
            if cx.span().span_context().is_sampled() {
                return SamplingResult {
                    decision: SamplingDecision::RecordAndSample,
                    ..Default::default()
                };
            }
        }

        let rate = self.overrides.get(name).copied().unwrap_or(self.default_rate);
        if rand::random::<f64>() < rate {
            SamplingResult { decision: SamplingDecision::RecordAndSample, ..Default::default() }
        } else {
            SamplingResult { decision: SamplingDecision::Drop, ..Default::default() }
        }
    }
}
```

### 3.6 RecallTask 中的 trace_context 字段

为支持 Span Link，RecallTask 和 ArchiveTask 增加 trace context 字段：

```rust
pub struct RecallTask {
    // ... 已有字段 ...
    pub trace_context: Option<String>,  // W3C TraceContext 序列化
}

pub struct ArchiveTask {
    // ... 已有字段 ...
    pub trace_context: Option<String>,
}
```

序列化格式采用 W3C Trace Context（`traceparent` header 格式）：

```
00-{trace_id_hex}-{span_id_hex}-{trace_flags_hex}
```

---

## 4. Metrics 指标体系

### 4.1 指标注册

```rust
use opentelemetry::global;
use opentelemetry::metrics::{Counter, Histogram, Meter, UpDownCounter};

pub struct ColdStoreMetrics {
    meter: Meter,

    // ── 接入层 ──
    pub s3_requests_total: Counter<u64>,
    pub s3_request_duration: Histogram<f64>,
    pub s3_request_body_size: Histogram<f64>,
    pub s3_response_body_size: Histogram<f64>,
    pub s3_errors_total: Counter<u64>,

    // ── 元数据层 ──
    pub raft_propose_total: Counter<u64>,
    pub raft_propose_duration: Histogram<f64>,
    pub raft_apply_duration: Histogram<f64>,
    pub raft_leader_changes: Counter<u64>,
    pub raft_log_entries: Counter<u64>,
    pub metadata_read_duration: Histogram<f64>,
    pub metadata_write_batch_size: Histogram<f64>,

    // ── 缓存层 ──
    pub cache_hits: Counter<u64>,
    pub cache_misses: Counter<u64>,
    pub cache_puts: Counter<u64>,
    pub cache_deletes: Counter<u64>,
    pub cache_evictions: Counter<u64>,
    pub cache_eviction_bytes: Counter<u64>,
    pub cache_get_duration: Histogram<f64>,
    pub cache_put_duration: Histogram<f64>,
    pub cache_usage_bytes: UpDownCounter<i64>,
    pub cache_object_count: UpDownCounter<i64>,
    pub spdk_io_duration: Histogram<f64>,
    pub spdk_io_bytes: Histogram<f64>,

    // ── 调度层 ──
    pub archive_bundles_created: Counter<u64>,
    pub archive_bundles_completed: Counter<u64>,
    pub archive_bundles_failed: Counter<u64>,
    pub archive_objects_total: Counter<u64>,
    pub archive_bytes_total: Counter<u64>,
    pub archive_throughput_bytes: Counter<u64>,
    pub archive_task_duration: Histogram<f64>,

    pub recall_tasks_created: Counter<u64>,
    pub recall_tasks_completed: Counter<u64>,
    pub recall_tasks_failed: Counter<u64>,
    pub recall_queue_depth: UpDownCounter<i64>,
    pub recall_wait_duration: Histogram<f64>,
    pub recall_read_duration: Histogram<f64>,
    pub recall_e2e_duration: Histogram<f64>,

    pub compensation_pending: UpDownCounter<i64>,
    pub compensation_retries: Counter<u64>,

    // ── 磁带层 ──
    pub tape_write_bytes: Counter<u64>,
    pub tape_read_bytes: Counter<u64>,
    pub tape_write_duration: Histogram<f64>,
    pub tape_read_duration: Histogram<f64>,
    pub tape_seek_duration: Histogram<f64>,
    pub tape_swap_total: Counter<u64>,
    pub tape_swap_duration: Histogram<f64>,
    pub tape_errors: Counter<u64>,
    pub drive_utilization: Histogram<f64>,
    pub drive_queue_wait: Histogram<f64>,
    pub offline_requests: Counter<u64>,
    pub offline_wait_duration: Histogram<f64>,
    pub verify_total: Counter<u64>,
    pub verify_failures: Counter<u64>,
}
```

### 4.2 指标详细定义

#### 4.2.1 接入层指标

| 指标名 | 类型 | 单位 | Labels | 说明 |
|--------|------|------|--------|------|
| `coldstore.s3.requests.total` | Counter | 1 | `method`, `status_code`, `bucket` | S3 请求总数 |
| `coldstore.s3.request.duration` | Histogram | ms | `method`, `bucket` | 请求延迟分布 |
| `coldstore.s3.request.body.size` | Histogram | bytes | `method` | 请求体大小分布 |
| `coldstore.s3.response.body.size` | Histogram | bytes | `method` | 响应体大小分布 |
| `coldstore.s3.errors.total` | Counter | 1 | `method`, `error_code` | 错误总数（按 S3 错误码） |

**Histogram buckets（延迟）**：`[0.5, 1, 2, 5, 10, 25, 50, 100, 250, 500, 1000, 5000]` ms

**Label 值**：

| Label | 取值 |
|-------|------|
| `method` | `PUT`, `GET`, `HEAD`, `DELETE`, `RESTORE`, `LIST` |
| `status_code` | `200`, `202`, `403`, `404`, `409`, `500`, `503` |
| `error_code` | `InvalidObjectState`, `RestoreAlreadyInProgress`, ... |

#### 4.2.2 元数据层指标

| 指标名 | 类型 | 单位 | Labels | 说明 |
|--------|------|------|--------|------|
| `coldstore.raft.propose.total` | Counter | 1 | `command_type` | Raft Propose 次数 |
| `coldstore.raft.propose.duration` | Histogram | ms | `command_type` | Propose 到 Commit 延迟 |
| `coldstore.raft.apply.duration` | Histogram | ms | `command_type` | Apply 到 RocksDB 延迟 |
| `coldstore.raft.leader.changes` | Counter | 1 | — | Leader 切换次数 |
| `coldstore.raft.log.entries` | Counter | 1 | — | 累计日志条目数 |
| `coldstore.metadata.read.duration` | Histogram | ms | `cf`, `operation` | 读操作延迟（按 CF） |
| `coldstore.metadata.write_batch.size` | Histogram | 1 | `command_type` | WriteBatch 内操作数 |

**Label `command_type`**：`PutObject`, `DeleteObject`, `BatchArchiveComplete`, `BatchRecallComplete`, `PutRecallTask`, `UpdateTape`, ...

#### 4.2.3 缓存层指标

| 指标名 | 类型 | 单位 | Labels | 说明 |
|--------|------|------|--------|------|
| `coldstore.cache.hits` | Counter | 1 | — | 缓存命中次数 |
| `coldstore.cache.misses` | Counter | 1 | — | 缓存未命中次数 |
| `coldstore.cache.puts` | Counter | 1 | — | 写入次数 |
| `coldstore.cache.deletes` | Counter | 1 | — | 删除次数（接入层 DELETE / 淘汰） |
| `coldstore.cache.evictions` | Counter | 1 | `reason` | 淘汰次数 |
| `coldstore.cache.eviction.bytes` | Counter | bytes | `reason` | 淘汰字节数 |
| `coldstore.cache.get.duration` | Histogram | ms | `hit` | GET 延迟（区分命中/未命中） |
| `coldstore.cache.put.duration` | Histogram | ms | — | PUT 延迟 |
| `coldstore.cache.usage.bytes` | UpDownCounter | bytes | — | 当前使用量 |
| `coldstore.cache.object.count` | UpDownCounter | 1 | — | 当前对象数 |
| `coldstore.spdk.io.duration` | Histogram | μs | `operation` | SPDK I/O 延迟 |
| `coldstore.spdk.io.bytes` | Histogram | bytes | `operation` | SPDK I/O 大小 |

**Label `reason`**：`ttl`, `capacity_lru`, `capacity_lfu`, `manual`

**Label `operation`（SPDK）**：`create_blob`, `write_blob`, `read_blob`, `delete_blob`

#### 4.2.4 调度层指标

| 指标名 | 类型 | 单位 | Labels | 说明 |
|--------|------|------|--------|------|
| `coldstore.archive.bundles.created` | Counter | 1 | — | 创建的 Bundle 数 |
| `coldstore.archive.bundles.completed` | Counter | 1 | — | 完成的 Bundle 数 |
| `coldstore.archive.bundles.failed` | Counter | 1 | — | 失败的 Bundle 数 |
| `coldstore.archive.objects.total` | Counter | 1 | — | 已归档对象总数 |
| `coldstore.archive.bytes.total` | Counter | bytes | — | 已归档字节总数 |
| `coldstore.archive.throughput.bytes` | Counter | bytes | — | 写入磁带字节数（计算吞吐） |
| `coldstore.archive.task.duration` | Histogram | s | — | 单个 ArchiveTask 耗时 |
| `coldstore.recall.tasks.created` | Counter | 1 | `tier` | 创建的取回任务数 |
| `coldstore.recall.tasks.completed` | Counter | 1 | `tier` | 完成的取回任务数 |
| `coldstore.recall.tasks.failed` | Counter | 1 | `tier` | 失败的取回任务数 |
| `coldstore.recall.queue.depth` | UpDownCounter | 1 | `tier` | 当前队列深度 |
| `coldstore.recall.wait.duration` | Histogram | s | `tier` | 入队到开始执行的等待时间 |
| `coldstore.recall.read.duration` | Histogram | s | — | 磁带读取耗时 |
| `coldstore.recall.e2e.duration` | Histogram | s | `tier` | 端到端耗时（RestoreObject → Completed） |
| `coldstore.compensation.pending` | UpDownCounter | 1 | `saga_type` | 待补偿条目数 |
| `coldstore.compensation.retries` | Counter | 1 | `saga_type` | 补偿重试次数 |

#### 4.2.5 磁带层指标

| 指标名 | 类型 | 单位 | Labels | 说明 |
|--------|------|------|--------|------|
| `coldstore.tape.write.bytes` | Counter | bytes | `tape_id` | 写入字节数 |
| `coldstore.tape.read.bytes` | Counter | bytes | `tape_id` | 读取字节数 |
| `coldstore.tape.write.duration` | Histogram | s | — | 写入耗时 |
| `coldstore.tape.read.duration` | Histogram | s | — | 读取耗时 |
| `coldstore.tape.seek.duration` | Histogram | ms | `seek_type` | seek 耗时 |
| `coldstore.tape.swap.total` | Counter | 1 | — | 换带总次数 |
| `coldstore.tape.swap.duration` | Histogram | s | — | 换带耗时分布 |
| `coldstore.tape.errors` | Counter | 1 | `error_type`, `tape_id` | 错误次数 |
| `coldstore.drive.utilization` | Histogram | ratio | `drive_id` | 驱动利用率 |
| `coldstore.drive.queue.wait` | Histogram | s | `priority` | 驱动等待时间 |
| `coldstore.tape.offline.requests` | Counter | 1 | — | 离线请求次数 |
| `coldstore.tape.offline.wait.duration` | Histogram | s | — | 离线等待时间 |
| `coldstore.tape.verify.total` | Counter | 1 | `result` | 校验次数 |
| `coldstore.tape.verify.failures` | Counter | 1 | — | 校验失败次数 |

**Label `seek_type`**：`fsf`（FileMark 前进）, `fsr`（块前进）, `rewind`

**Label `error_type`**：`hardware`, `media`, `write_protected`, `cleaning_required`

**Label `priority`**：`Expedited`, `Archive`, `Standard`, `Bulk`

### 4.3 Instrumentation 代码示例

#### 4.3.1 接入层

```rust
use tracing::instrument;

#[instrument(
    name = "s3.get_object",
    skip(state),
    fields(
        coldstore.bucket = %bucket,
        coldstore.key = %key,
        coldstore.storage_class,
        cache.hit,
    )
)]
async fn handle_get_object(
    state: AppState,
    bucket: &str,
    key: &str,
) -> Result<Response> {
    let start = Instant::now();

    let meta = state.metadata.get_object(bucket, key).await?;
    let meta = meta.ok_or(Error::NoSuchKey)?;

    Span::current().record("coldstore.storage_class", &meta.storage_class.as_str());

    // ColdStore 是纯冷归档系统，所有对象都是 Cold 或 ColdPending
    match meta.storage_class {
        StorageClass::ColdPending => {
            Err(Error::InvalidObjectState)
        }
        StorageClass::Cold => {
            match meta.restore_status {
                Some(RestoreStatus::Completed) if meta.restore_expire_at > Some(Utc::now()) => {
                    let cached = state.cache.get(bucket, key, None).await?;
                    match cached {
                        Some(obj) => {
                            Span::current().record("cache.hit", true);
                            state.metrics.cache_hits.add(1, &[]);
                            state.metrics.s3_request_duration.record(
                                start.elapsed().as_secs_f64() * 1000.0,
                                &[KeyValue::new("method", "GET"), KeyValue::new("bucket", bucket)],
                            );
                            Ok(build_response(obj, &meta))
                        }
                        None => {
                            Span::current().record("cache.hit", false);
                            state.metrics.cache_misses.add(1, &[]);
                            Err(Error::ServiceUnavailable("cache miss, please restore again"))
                        }
                    }
                }
                _ => Err(Error::InvalidObjectState),
            }
        }
    }
}
```

#### 4.3.2 调度层

```rust
#[instrument(
    name = "scheduler.recall_process",
    skip(self),
    fields(
        scheduler.task_type = "recall",
        scheduler.merge_count,
    )
)]
async fn process_recall_batch(&self) {
    let tasks = self.metadata.list_pending_recall_tasks().await.unwrap_or_default();
    if tasks.is_empty() { return; }

    let jobs = self.merge_by_tape(tasks);
    Span::current().record("scheduler.merge_count", jobs.len() as i64);
    self.metrics.recall_queue_depth.add(-(jobs.len() as i64), &[]);

    for job in jobs {
        let span = tracing::info_span!("scheduler.execute_tape_read_job",
            tape.tape_id = %job.tape_id,
            scheduler.task_id = %job.job_id,
            coldstore.object_count = job.total_objects as i64,
        );

        self.execute_tape_read_job(job).instrument(span).await;
    }
}

#[instrument(
    name = "scheduler.execute_tape_read_job",
    skip(self, job),
    fields(
        tape.drive_id,
        tape.bytes_transferred,
    )
)]
async fn execute_tape_read_job(&self, job: TapeReadJob) {
    let acquire_start = Instant::now();

    // 申请驱动
    let guard = self.tape_manager.acquire_drive(
        job.priority.into()
    ).instrument(tracing::info_span!("tape.acquire_drive"))
    .await?;

    self.metrics.drive_queue_wait.record(
        acquire_start.elapsed().as_secs_f64(),
        &[KeyValue::new("priority", job.priority.as_str())],
    );

    Span::current().record("tape.drive_id", guard.drive_id());

    // 加载磁带
    self.tape_manager.load_tape(&guard, &job.tape_id)
        .instrument(tracing::info_span!("tape.load", tape.tape_id = %job.tape_id))
        .await?;

    let mut total_bytes = 0u64;

    for segment in &job.segments {
        // seek 到 FileMark
        guard.drive().seek_filemark(segment.filemark)
            .instrument(tracing::info_span!("tape.seek",
                tape.filemark = segment.filemark as i64,
            ))
            .await?;

        for obj in &segment.objects {
            let read_span = tracing::info_span!("tape.read_object",
                coldstore.bucket = %obj.bucket,
                coldstore.key = %obj.key,
                coldstore.object_size = obj.size as i64,
            );
            let _enter = read_span.enter();

            let data = guard.drive().read(obj.size).await?;
            total_bytes += data.len() as u64;

            // 校验
            verify_checksum(&data, &obj.checksum)?;

            // 写入缓存
            self.cache.put_restored(RestoredItem {
                bucket: obj.bucket.clone(),
                key: obj.key.clone(),
                data,
                expire_at: obj.expire_at,
                // ...
            }).instrument(tracing::info_span!("cache.put_restored"))
            .await?;

            // 更新元数据
            self.metadata.update_restore_status(
                &obj.bucket, &obj.key,
                RestoreStatus::Completed,
                Some(obj.expire_at),
            ).instrument(tracing::info_span!("metadata.update_restore_status"))
            .await?;

            self.metrics.recall_tasks_completed.add(1, &[
                KeyValue::new("tier", job.priority.as_str()),
            ]);
        }
    }

    Span::current().record("tape.bytes_transferred", total_bytes as i64);
    self.metrics.tape_read_bytes.add(total_bytes, &[
        KeyValue::new("tape_id", job.tape_id.clone()),
    ]);
}
```

#### 4.3.3 元数据层

```rust
#[instrument(
    name = "metadata.put_object",
    skip(self, meta),
    fields(
        coldstore.bucket = %meta.bucket,
        coldstore.key = %meta.key,
        raft.log_index,
        raft.commit_latency_ms,
    )
)]
async fn put_object(&self, meta: ObjectMetadata) -> Result<()> {
    let start = Instant::now();

    let request = ColdStoreRequest::PutObject(meta);
    let response = self.raft.client_write(request).await?;

    let latency = start.elapsed().as_secs_f64() * 1000.0;
    Span::current().record("raft.log_index", response.log_id.index as i64);
    Span::current().record("raft.commit_latency_ms", latency);

    self.metrics.raft_propose_total.add(1, &[
        KeyValue::new("command_type", "PutObject"),
    ]);
    self.metrics.raft_propose_duration.record(latency, &[
        KeyValue::new("command_type", "PutObject"),
    ]);

    Ok(())
}
```

---

## 5. 结构化日志设计

### 5.1 日志与 Trace 关联

`tracing` + `tracing-subscriber` JSON formatter 自动在日志中注入 `trace_id` 和 `span_id`：

```json
{
  "timestamp": "2026-02-27T10:15:30.123Z",
  "level": "INFO",
  "target": "coldstore::scheduler::recall",
  "message": "recall task completed",
  "span": {
    "name": "scheduler.execute_tape_read_job",
    "scheduler.task_id": "a1b2c3d4-...",
    "tape.tape_id": "TAPE001"
  },
  "spans": [
    { "name": "scheduler.recall_process" },
    { "name": "scheduler.execute_tape_read_job" }
  ],
  "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736",
  "span_id": "00f067aa0ba902b7",
  "fields": {
    "recall_task_id": "a1b2c3d4-...",
    "bucket": "my-bucket",
    "key": "data/file.bin",
    "duration_ms": 45230,
    "objects_read": 15,
    "bytes_read": 1073741824
  }
}
```

### 5.2 日志级别规范

| 级别 | 使用场景 | 示例 |
|------|----------|------|
| **ERROR** | 不可恢复错误、需人工干预 | 补偿任务放弃、磁带硬件故障、Raft 多数派丢失 |
| **WARN** | 可恢复异常、性能退化 | 重试发生、缓存未命中、驱动分配超时、离线磁带 |
| **INFO** | 关键业务事件 | 归档完成、取回完成、缓存淘汰批次、Leader 切换 |
| **DEBUG** | 操作详情 | 单对象读写、Raft 日志详情、Blob 操作详情 |
| **TRACE** | 最细粒度 | SPDK I/O 细节、RocksDB 读写字节、块对齐填充 |

### 5.3 关键事件（Span Event）

在长 Span 内部使用 `tracing::event!` 记录里程碑：

```rust
#[instrument(name = "scheduler.execute_tape_read_job")]
async fn execute_tape_read_job(&self, job: TapeReadJob) {
    tracing::info!(tape_id = %job.tape_id, "acquiring drive");
    let guard = self.tape_manager.acquire_drive(priority).await?;

    tracing::info!(drive_id = %guard.drive_id(), "drive acquired, loading tape");
    self.tape_manager.load_tape(&guard, &job.tape_id).await?;

    tracing::info!("tape loaded, starting sequential read");

    for (i, obj) in objects.iter().enumerate() {
        tracing::debug!(
            object_index = i,
            bucket = %obj.bucket,
            key = %obj.key,
            size = obj.size,
            "reading object from tape"
        );
        // ...
    }

    tracing::info!(
        objects_read = objects.len(),
        bytes_read = total_bytes,
        duration_ms = start.elapsed().as_millis() as u64,
        "tape read job completed"
    );
}
```

---

## 6. 仪表板设计

### 6.1 Dashboard 布局

```
┌─────────────────────────────────────────────────────────────────┐
│                   ColdStore Overview Dashboard                    │
├─────────────┬─────────────┬─────────────┬───────────────────────┤
│ S3 QPS      │ Error Rate  │ P99 Latency │ Active Restores       │
│ [Counter]   │ [Gauge %]   │ [Gauge ms]  │ [Gauge]               │
├─────────────┴─────────────┴─────────────┴───────────────────────┤
│                                                                   │
│  ┌─ S3 Request Rate (by method) ──────────────────────────────┐ │
│  │  PUT ████████ 120/s                                        │ │
│  │  GET ████████████████████████ 850/s                        │ │
│  │  RESTORE ██ 5/s                                            │ │
│  └────────────────────────────────────────────────────────────┘ │
│                                                                   │
│  ┌─ Request Latency (P50/P95/P99) ───────────────────────────┐ │
│  │  [Time series graph: GET, PUT, RESTORE]                    │ │
│  └────────────────────────────────────────────────────────────┘ │
├───────────────────────────────────────────────────────────────────┤
│                   Metadata Cluster                                │
├─────────────┬─────────────┬───────────────────────────────────────┤
│ Raft QPS    │ Leader Node │ Propose P99                          │
│ [Counter]   │ [Stat]      │ [Gauge ms]                           │
├─────────────┴─────────────┴───────────────────────────────────────┤
│                                                                   │
│  ┌─ Raft Propose Latency ────────────────────────────────────┐  │
│  │  [Histogram heatmap by command_type]                      │  │
│  └───────────────────────────────────────────────────────────┘  │
├───────────────────────────────────────────────────────────────────┤
│                     Cache Layer                                    │
├─────────────┬─────────────┬─────────────┬─────────────────────────┤
│ Hit Rate    │ Usage       │ Objects     │ Eviction Rate           │
│ [Gauge %]   │ [Gauge GB]  │ [Gauge]     │ [Counter /min]          │
├─────────────┴─────────────┴─────────────┴─────────────────────────┤
│                                                                   │
│  ┌─ Cache Hit/Miss Rate ─────┐  ┌─ SPDK I/O Latency ──────────┐│
│  │  [Stacked area chart]     │  │  [Histogram: read/write]     ││
│  └───────────────────────────┘  └──────────────────────────────┘│
├───────────────────────────────────────────────────────────────────┤
│                  Scheduler Layer                                   │
├─────────────┬─────────────┬─────────────┬─────────────────────────┤
│ Archive     │ Recall      │ Queue Depth │ Compensation            │
│ Throughput  │ Completed   │ [Gauge]     │ Pending                 │
│ [MB/s]      │ [/min]      │             │ [Gauge]                 │
├─────────────┴─────────────┴─────────────┴─────────────────────────┤
│                                                                   │
│  ┌─ Recall E2E Latency (by tier) ─────────────────────────────┐ │
│  │  Expedited  ██ P50=2min  P99=4.5min                        │ │
│  │  Standard   █████████████ P50=2hr  P99=4.8hr               │ │
│  │  Bulk       ██████████████████ P50=6hr  P99=11hr           │ │
│  └────────────────────────────────────────────────────────────┘ │
├───────────────────────────────────────────────────────────────────┤
│                    Tape Layer                                      │
├─────────────┬─────────────┬─────────────┬─────────────────────────┤
│ Drive       │ Swap Rate   │ Throughput  │ Errors                  │
│ Utilization │ [/hour]     │ [MB/s]      │ [Counter]               │
│ [Gauge %]   │             │             │                         │
├─────────────┴─────────────┴─────────────┴─────────────────────────┤
│                                                                   │
│  ┌─ Drive Utilization (per drive) ─┐  ┌─ Tape Errors ─────────┐│
│  │  [Multi-line gauge chart]       │  │  [by error_type]       ││
│  └─────────────────────────────────┘  └────────────────────────┘│
└───────────────────────────────────────────────────────────────────┘
```

### 6.2 核心 Dashboard 查询

#### 6.2.1 S3 请求速率

```promql
rate(coldstore_s3_requests_total[5m])
```

#### 6.2.2 S3 GET P99 延迟

```promql
histogram_quantile(0.99, rate(coldstore_s3_request_duration_bucket{method="GET"}[5m]))
```

#### 6.2.3 缓存命中率

```promql
rate(coldstore_cache_hits[5m])
/ (rate(coldstore_cache_hits[5m]) + rate(coldstore_cache_misses[5m]))
```

#### 6.2.4 归档吞吐（MB/s）

```promql
rate(coldstore_archive_throughput_bytes[5m]) / 1048576
```

#### 6.2.5 取回 Expedited P99 端到端延迟

```promql
histogram_quantile(0.99, rate(coldstore_recall_e2e_duration_bucket{tier="Expedited"}[30m]))
```

#### 6.2.6 驱动利用率

```promql
avg(coldstore_drive_utilization) by (drive_id)
```

#### 6.2.7 补偿任务积压

```promql
coldstore_compensation_pending
```

---

## 7. 告警规则

### 7.1 严重告警（P1 — 立即响应）

| 规则 | 条件 | 含义 |
|------|------|------|
| Raft 无 Leader | `absent(raft_leader)` 持续 > 30s | 元数据写入不可用 |
| 多数派丢失 | `raft_peers_connected < quorum` 持续 > 60s | 集群不可用 |
| 全部驱动故障 | `sum(drive_status == "available") == 0` 持续 > 5min | 归档/取回全部阻塞 |
| 缓存设备故障 | `spdk_bdev_status != "online"` | 缓存完全不可用 |
| 补偿任务放弃 | `increase(compensation_abandoned[1h]) > 0` | 数据一致性风险 |

### 7.2 高优先级告警（P2 — 1 小时内响应）

| 规则 | 条件 | 含义 |
|------|------|------|
| S3 错误率飙升 | `rate(s3_errors_total[5m]) / rate(s3_requests_total[5m]) > 0.05` | 5% 以上请求失败 |
| Raft Propose 延迟 | `histogram_quantile(0.99, raft_propose_duration) > 100` ms | 元数据写入变慢 |
| Expedited SLA 超标 | `histogram_quantile(0.99, recall_e2e_duration{tier="Expedited"}) > 300` s | 加急取回超 5 分钟 |
| 缓存使用率过高 | `cache_usage_bytes / cache_max_bytes > 0.95` | 即将触发频繁淘汰 |
| 补偿任务积压 | `compensation_pending > 10` | 元数据更新失败积压 |
| 离线磁带等待 | `offline_wait_duration > 3600` s | 磁带超 1 小时未上线 |

### 7.3 中优先级告警（P3 — 24 小时内关注）

| 规则 | 条件 | 含义 |
|------|------|------|
| 缓存命中率低 | `cache_hit_rate < 0.7` 持续 > 1h | 可能需要扩容缓存 |
| 归档吞吐下降 | `archive_throughput_mbps < 200` 持续 > 30min | 写入性能不达标 |
| 换带频繁 | `rate(tape_swap_total[1h]) > 50` | 合并策略可能需优化 |
| 磁带错误率上升 | `rate(tape_errors[1h]) > 5` | 介质质量退化 |
| 取回队列积压 | `recall_queue_depth > 5000` | 取回能力不足 |
| 驱动利用率饱和 | `avg(drive_utilization) > 0.95` 持续 > 1h | 需增加驱动 |
| Raft Leader 频繁切换 | `increase(raft_leader_changes[1h]) > 3` | 网络或节点不稳定 |

---

## 8. 模块结构

```
src/telemetry/
├── mod.rs                    # pub use，init_telemetry 导出
├── config.rs                 # TelemetryConfig
├── init.rs                   # TracerProvider + MeterProvider + LoggerProvider 初始化
├── metrics.rs                # ColdStoreMetrics 结构体 + 所有指标注册
├── sampler.rs                # ColdStoreSampler 自定义采样器
├── context.rs                # trace_context 序列化/反序列化（W3C TraceContext）
├── middleware.rs             # Axum 中间件（自动记录 S3 请求 span + metrics）
└── shutdown.rs               # TelemetryGuard，优雅关闭
```

---

## 9. 配置项

```yaml
telemetry:
  service_name: "coldstore"
  environment: "production"
  otlp_endpoint: "http://otel-collector:4317"
  log_level: "info"

  traces:
    enabled: true
    sample_rate: 0.1                         # 默认 10%
    sample_overrides:
      "s3.get_object": 0.01                  # GET 热路径 1%
      "s3.restore_object": 1.0               # Restore 100%
      "scheduler.archive_task": 1.0
      "scheduler.recall_process": 1.0
      "tape.*": 1.0
    export_batch_size: 512
    export_timeout_ms: 30000

  metrics:
    enabled: true
    export_interval_secs: 15
    histogram_buckets:
      latency_ms: [0.5, 1, 2, 5, 10, 25, 50, 100, 250, 500, 1000, 5000]
      latency_s: [0.1, 0.5, 1, 5, 10, 30, 60, 300, 600, 1800, 3600]
      size_bytes: [1024, 65536, 1048576, 10485760, 104857600, 1073741824]
    prometheus:
      enabled: true
      listen: "0.0.0.0:9090"

  logs:
    format: "json"
    include_trace_id: true
    include_span_list: true
```

---

## 10. 与其他层的集成要点

### 10.1 各层 instrumentation 清单

| 层 | 集成方式 | 需要的 Crate 特性 |
|------|----------|------------------|
| 接入层 (01) | Axum middleware + `#[instrument]` | `tower-http` tracing layer |
| 协议层 (02) | `#[instrument]` on parse/build 函数 | — |
| 元数据 (03) | `#[instrument]` on trait impl + Raft hooks | `openraft` metrics callback |
| 缓存 (04) | `#[instrument]` + SPDK 线程 Context 传播 | 显式 `Context::current().attach()` |
| 调度 (05) | `#[instrument]` + Span Link + Event | `tracing::Span::add_link` |
| 磁带 (06) | `#[instrument]` on trait impl | — |

### 10.2 Axum 中间件

```rust
use axum::middleware;
use tower_http::trace::TraceLayer;

let app = Router::new()
    .route("/*path", any(s3_handler))
    .layer(
        TraceLayer::new_for_http()
            .make_span_with(|request: &Request<Body>| {
                let method = request.method().as_str();
                let path = request.uri().path();
                tracing::info_span!(
                    "s3.request",
                    http.method = %method,
                    http.url = %path,
                    http.status_code = tracing::field::Empty,
                    coldstore.bucket = tracing::field::Empty,
                    coldstore.key = tracing::field::Empty,
                )
            })
            .on_response(|response: &Response<Body>, latency: Duration, span: &Span| {
                span.record("http.status_code", response.status().as_u16());
            }),
    );
```

### 10.3 性能开销评估

| 组件 | 开销 | 说明 |
|------|------|------|
| `#[instrument]` | ~100ns/span（非采样时） | Span 判断是否采样后短路 |
| 采样 Span 创建 | ~1-2μs | 分配 + attributes 填充 |
| Metrics Counter/Gauge | ~10ns | 原子操作 |
| Metrics Histogram | ~50ns | bucket 查找 + 原子操作 |
| OTLP 批量导出 | 后台异步 | 不阻塞业务路径 |
| JSON 日志格式化 | ~1-5μs/行 | 仅在 log level 匹配时 |

**对 GET 热路径的总开销**（1% 采样）：~99% 请求仅 ~200ns overhead，1% 被采样的请求 ~5μs。对于 < 1ms 的目标延迟，影响可忽略。

---

## 11. 参考资料

- [OpenTelemetry Rust](https://opentelemetry.io/docs/languages/rust/) — 官方文档
- [opentelemetry crate](https://docs.rs/opentelemetry/latest/opentelemetry/) — API 文档
- [tracing-opentelemetry](https://docs.rs/tracing-opentelemetry) — tracing 桥接层
- [OpenTelemetry Semantic Conventions](https://opentelemetry.io/docs/specs/semconv/) — 标准化属性命名
- [OpenTelemetry Collector](https://opentelemetry.io/docs/collector/) — Collector 配置
- [Grafana + Tempo + Prometheus](https://grafana.com/docs/) — 可视化后端
