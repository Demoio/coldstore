# 数据缓存层模块设计

> 所属架构：ColdStore 冷存储系统  
> 参考：[总架构设计](../DESIGN.md)

## 1. 模块概述

数据缓存层承载从磁带取回的解冻数据，供 GET 请求快速响应，避免重复触发磁带读取。

> **部署模型**：缓存层运行在 **Cache Worker** 节点上，与 **Scheduler Worker** 同机部署。
> Scheduler 通过 gRPC 与 Cache Worker 通信（虽然同机，仍使用 gRPC 保持架构一致性）。
> Gateway 不直连 Cache Worker，所有数据读写都由 Scheduler Worker 代理。

### 1.1 职责

- 存储解冻后的对象数据
- 响应 GET 请求（冷对象已解冻时）
- 缓存淘汰：LRU、LFU、TTL、容量限制
- 与 SPDK 集成，实现高性能用户态 I/O

### 1.2 在架构中的位置

```
取回调度器 ──► 磁带读取 ──► 数据缓存层 ──► 协议层/接入层 ──► GET 响应
                                │
                                └─ async-spdk (SPDK Blobstore on bdev)

注意：缓存层不直接持有元数据 client。
元数据更新由调度层统一负责（方案 B）。
```

---

## 2. 技术选型

| 组件 | 选型 | 说明 |
|------|------|------|
| SPDK 绑定 | **async-spdk** | [madsys-dev/async-spdk](https://github.com/madsys-dev/async-spdk) |
| SPDK 存储方式 | **Blobstore (Blob)** | 基于 Blob 的持久化块分配器，非 raw bdev |
| 底层设备 | NVMe bdev / Malloc (测试) | Blobstore 构建于 bdev 之上 |

---

## 3. 数据布局：SPDK Blob 方式

### 3.1 Blobstore 层级结构

SPDK Blobstore 定义以下抽象（[SPDK Blobstore 文档](https://spdk.io/doc/blob.html)）：

| 层级 | 典型大小 | 说明 |
|------|----------|------|
| **Logical Block** | 512B / 4KB | 磁盘暴露的基本单位 |
| **Page** | 4KB | 固定页，读写最小单位 |
| **Cluster** | 1MB (256 pages) | 分配单元，Blob 由 cluster 组成 |
| **Blob** | 可变 | 有序 cluster 列表，持久化，由 blob_id 标识 |
| **Blobstore** | 整设备 | 拥有底层 bdev，含元数据区 + blob 集合 |

### 3.2 缓存层与 Blob 的对应关系

```
┌─────────────────────────────────────────────────────────────────────────┐
│                     Blobstore 布局（整 bdev）                             │
├─────────────────────────────────────────────────────────────────────────┤
│  [ Super Block ]  [ Metadata Region ]  [ Blob 数据区 ]                    │
│       │                    │                    │                         │
│  Cluster 0 专用      Blob 元数据链表      Cluster 1..N                     │
│  签名/版本/配置      per-blob 元数据      各 Blob 的 cluster               │
└─────────────────────────────────────────────────────────────────────────┘

每个缓存对象 = 1 个 Blob
  - blob_id: Blobstore 分配的唯一 ID
  - 数据: 按 page 读写，支持 thin provisioning（首次写时分配 cluster）
  - xattrs: bucket, key, size, checksum, expire_at, cached_at
```

### 3.3 对象与 Blob 映射

| 缓存对象 | Blob 表示 |
|----------|-----------|
| 1 个 S3 对象 (bucket, key) | 1 个 Blob |
| 对象数据 | Blob 的 cluster 序列，按 page 读写 |
| 元数据 | Blob xattrs（见 3.3.1） |

**cache_key → blob_id 索引**：

- Blobstore 不提供按名称查找，需自维护 `cache_key → blob_id` 映射
- 存储方式：内存 HashMap + 可选持久化（如写入 Super Blob 或独立索引 Blob）
- `cache_key = format!("{}:{}", bucket, key)` 或含 version_id

### 3.3.1 Blob xattrs 字段定义

xattrs 为 Blob 的扩展属性，键值对形式，用于存储对象级元数据。字段含义与用途如下：

| xattr key | 类型 | 必填 | 含义 | 用途 |
|-----------|------|------|------|------|
| `bucket` | string | 是 | S3 桶名 | 与 key 共同唯一标识对象；GET 时校验、淘汰时按桶过滤 |
| `key` | string | 是 | S3 对象键 | 与 bucket 共同唯一标识对象；重建 cache_key |
| `size` | u64 (string 序列化) | 是 | 对象字节大小 | 读取时分配缓冲；校验读取长度；淘汰时计算容量 |
| `expire_at` | i64 Unix 时间戳 (string) | 是 | 解冻过期时间 | 与元数据 restore_expire_at 一致；TTL 淘汰依据 |
| `cached_at` | i64 Unix 时间戳 (string) | 是 | 写入缓存时间 | LRU 淘汰依据；可观测性（缓存时长） |
| `checksum` | string (hex) | 否 | 对象校验和 SHA256 | 读后校验数据完整性；可选，调度层传入时写入 |
| `version_id` | string | 否 | S3 对象版本 ID | 多版本支持；cache_key 含 version 时需存 |
| `content_type` | string | 否 | 对象 Content-Type | 协议层 GET 响应头；可选透传 |
| `etag` | string | 否 | 对象 ETag | 协议层 GET 响应头；可选透传 |

**xattrs 用途汇总**：

- **查找与校验**：bucket + key 用于重建 cache_key，与索引一致时确认 Blob 归属
- **淘汰**：expire_at 驱动 TTL 淘汰，cached_at 驱动 LRU 淘汰，size 参与容量计算
- **完整性**：checksum 用于读后校验，发现损坏可触发重新 Restore
- **协议透传**：content_type、etag 供 GET 响应头使用，避免再查元数据

### 3.3.2 cache_key → blob_id 索引条目

内存索引（及可选持久化）中每条目的字段：

| 字段 | 类型 | 含义 | 用途 |
|------|------|------|------|
| `cache_key` | string | `{bucket}:{key}` 或含 version | 主键，唯一标识缓存对象 |
| `blob_id` | u64 | Blobstore 分配的 Blob ID | 定位 Blob，执行 open/read/write/delete |
| `size` | u64 | 对象大小 | 淘汰时容量统计；读时分配缓冲 |
| `expire_at` | i64 | 过期时间戳 | 快速判断是否过期，无需开 Blob 读 xattr |
| `cached_at` | i64 | 缓存时间戳 | LRU 排序；避免开 Blob 读 xattr |

**索引与 xattrs 的关系**：索引用于快速查找（cache_key → blob_id）和淘汰决策；xattrs 为 Blob 内持久化元数据，用于校验、重建索引、协议透传。两者需保持一致，写入时同步更新。

### 3.3.3 其他相关字段

| 来源 | 字段 | 含义 | 用途 |
|------|------|------|------|
| Blobstore | `blob_id` | Blob 唯一标识 | 所有 Blob 操作的句柄 |
| Blobstore | `blob_size` (num_clusters × cluster_size) | Blob 逻辑大小 | resize 时设置；与对象 size 对齐到 cluster |
| 调度层传入 | `data` | 对象原始字节 | 写入 Blob 的 payload |
| 调度层传入 | `checksum` | 可选 SHA256 | 写入 xattr，读后校验 |
| 调度层传入 | `expire_at` | 解冻保留截止时间 | 写入 xattr 与索引；来自 RestoreRequest.Days |

### 3.4 Blob 创建与读写流程

```
创建/写入:
  1. 创建 Blob:   spdk_bs_create_blob() → blob_id
  2. 打开 Blob:   spdk_bs_open_blob(blob_id) → blob handle
  3. 设置 xattrs:  spdk_blob_set_xattr(blob, "bucket", ...) 等
  4. 调整大小:    spdk_blob_resize(blob, num_clusters)
  5. 同步元数据:  spdk_blob_sync_md(blob)
  6. 写入数据:    spdk_blob_io_write(blob, channel, offset, len, buf, cb)
  7. 关闭 Blob:   spdk_blob_close(blob)

读取:
  1. 查索引:     cache_key → blob_id
  2. 打开 Blob:  spdk_bs_open_blob(blob_id) → blob handle
  3. 读取:       spdk_blob_io_read(blob, channel, offset, len, buf, cb)
  4. 关闭 Blob:  spdk_blob_close(blob)

注意：set_xattr / resize 需在 open 之后的 blob handle 上调用。
读写单位为 io_unit（通常 4KB，可通过 spdk_bs_get_io_unit_size() 获取）。
```

### 3.5 Blob 数据格式

- **读写单位**：io_unit（通常 4KB），`spdk_blob_io_read`/`spdk_blob_io_write` 的 offset 和 length 以 io_unit 为单位
- **对象数据**：连续写入 Blob，不足一 page 的尾部需填充或按 page 写入
- **Thin Provisioning**：可创建 thin blob，首次写入时分配 cluster，节省空间

### 3.6 关键参数

| 参数 | 推荐值 | 说明 |
|------|--------|------|
| cluster_size | 1MB | Blobstore 创建时指定，Blob 按 cluster 分配 |
| page_size | 4KB | 读写粒度 |
| max_object_size | 5GB | 单对象上限，超限不缓存或分块 |
| blobstore_type | "coldstore_cache" | 用于识别 Blobstore 归属 |

### 3.7 与 async-spdk 的集成

async-spdk 提供 `hello_blob` 示例，使用 Blobstore API。缓存层需：

- 初始化 Blobstore：`spdk_bs_init()` / `spdk_bs_load()` 于指定 bdev
- 使用 Blob API：`create_blob`、`open_blob`、`resize`、`write`、`read`、`close`、`delete_blob`
- 通过 xattrs 存储 bucket、key、expire_at 等
- 自维护 `cache_key → blob_id` 索引（内存 + 可选持久化）

---

## 4. 核心数据结构

### 4.1 CacheManager

缓存层的顶层入口，同时实现 `CacheReadApi` + `CacheWriteApi`。

```rust
pub struct CacheManager {
    backend: Box<dyn CacheBackend>,
    index: CacheIndex,
    config: CacheConfig,
    stats: CacheStats,
}
```

| 字段 | 类型 | 含义 | 说明 |
|------|------|------|------|
| `backend` | `Box<dyn CacheBackend>` | 存储后端实例 | 多态：SpdkBlobCacheBackend 或 FileCacheBackend |
| `index` | `CacheIndex` | cache_key → blob_id 的内存索引 | 所有查找/淘汰决策的入口 |
| `config` | `CacheConfig` | 缓存配置 | 容量上限、TTL、淘汰策略等 |
| `stats` | `CacheStats` | 运行时统计 | 命中率、容量使用、淘汰次数等 |

### 4.2 CacheBackend trait

```rust
#[async_trait]
pub trait CacheBackend: Send + Sync {
    async fn write_blob(
        &self, blob_id: u64, data: &[u8], xattrs: &BlobXattrs,
    ) -> Result<u64>;
    async fn read_blob(&self, blob_id: u64) -> Result<Vec<u8>>;
    async fn delete_blob(&self, blob_id: u64) -> Result<()>;
    async fn create_blob(&self, xattrs: &BlobXattrs, size: u64) -> Result<u64>;
    async fn read_xattrs(&self, blob_id: u64) -> Result<BlobXattrs>;
    async fn list_blobs(&self) -> Result<Vec<u64>>;
}
```

| 方法 | 含义 |
|------|------|
| `write_blob` | 向已创建的 Blob 写入对象数据 |
| `read_blob` | 从 Blob 读取对象全部数据 |
| `delete_blob` | 删除 Blob（淘汰时调用） |
| `create_blob` | 创建新 Blob、设置 xattrs、resize，返回 blob_id |
| `read_xattrs` | 读取 Blob 的 xattrs（启动时重建索引用） |
| `list_blobs` | 列出 Blobstore 中所有 Blob ID（启动时重建索引用） |

### 4.3 BlobXattrs

对应 Blob 上持久化的 xattrs 键值对，Rust 侧的结构化表示。

```rust
pub struct BlobXattrs {
    pub bucket: String,
    pub key: String,
    pub size: u64,
    pub expire_at: i64,
    pub cached_at: i64,
    pub checksum: Option<String>,
    pub version_id: Option<String>,
    pub content_type: Option<String>,
    pub etag: Option<String>,
}
```

| 字段 | 类型 | 必填 | 含义 | 写入时机 |
|------|------|------|------|----------|
| `bucket` | String | 是 | S3 桶名 | 调度层 put_restored 传入 |
| `key` | String | 是 | S3 对象键 | 调度层 put_restored 传入 |
| `size` | u64 | 是 | 对象原始字节数（非 Blob 占用的 cluster 数） | 调度层传入 data.len() |
| `expire_at` | i64 | 是 | 解冻过期 Unix 时间戳（秒） | 调度层从 RestoreRequest.Days 计算后传入 |
| `cached_at` | i64 | 是 | 写入缓存的 Unix 时间戳（秒） | CacheManager 写入时取 Utc::now() |
| `checksum` | Option\<String\> | 否 | SHA256 hex 字符串 | 调度层传入（若磁带读取时有校验） |
| `version_id` | Option\<String\> | 否 | S3 对象版本 ID | 调度层传入（多版本场景） |
| `content_type` | Option\<String\> | 否 | MIME 类型 | 调度层传入（协议层 GET 响应透传） |
| `etag` | Option\<String\> | 否 | S3 ETag | 调度层传入（协议层 GET 响应透传） |

### 4.4 CacheEntry（内存索引条目）

```rust
pub struct CacheEntry {
    pub cache_key: String,
    pub blob_id: u64,
    pub size: u64,
    pub expire_at: i64,
    pub cached_at: i64,
    pub last_accessed_at: i64,
    pub access_count: u64,
}
```

| 字段 | 类型 | 含义 | 用途 |
|------|------|------|------|
| `cache_key` | String | `{bucket}:{key}` 或 `{bucket}:{key}:{version_id}` | 索引主键，唯一标识缓存对象 |
| `blob_id` | u64 | Blobstore 分配的 Blob 唯一 ID | 所有 Blob 操作的句柄 |
| `size` | u64 | 对象原始字节数 | 淘汰时计算总容量；读时预分配缓冲 |
| `expire_at` | i64 | 解冻过期 Unix 时间戳 | TTL 淘汰判断，过期即可删除 |
| `cached_at` | i64 | 写入缓存 Unix 时间戳 | LRU 兜底排序（若无访问记录则用此值） |
| `last_accessed_at` | i64 | 最近一次 GET 读取时间 | LRU 淘汰排序，每次 get 时更新 |
| `access_count` | u64 | 累计 GET 次数 | LFU 淘汰排序（可选策略） |

### 4.5 CacheIndex

```rust
pub struct CacheIndex {
    entries: HashMap<String, CacheEntry>,
    total_size: u64,
}
```

| 字段 | 类型 | 含义 | 说明 |
|------|------|------|------|
| `entries` | HashMap\<String, CacheEntry\> | cache_key → CacheEntry | 全量内存索引，O(1) 查找 |
| `total_size` | u64 | 当前缓存总占用字节 | 每次 put/delete 更新，淘汰时与 max_size 比较 |

### 4.6 CachedObject（读取返回）

```rust
pub struct CachedObject {
    pub data: Vec<u8>,
    pub size: u64,
    pub expire_at: DateTime<Utc>,
    pub content_type: Option<String>,
    pub etag: Option<String>,
    pub checksum: Option<String>,
}
```

| 字段 | 类型 | 含义 | 消费方 |
|------|------|------|--------|
| `data` | Vec\<u8\> | 对象完整数据 | 协议层写入 HTTP 响应体 |
| `size` | u64 | 数据字节数 | 协议层 Content-Length |
| `expire_at` | DateTime\<Utc\> | 解冻过期时间 | 协议层生成 `x-amz-restore` 头 |
| `content_type` | Option\<String\> | MIME 类型 | 协议层 Content-Type 响应头 |
| `etag` | Option\<String\> | ETag | 协议层 ETag 响应头 |
| `checksum` | Option\<String\> | SHA256 hex | 协议层可选校验数据完整性 |

### 4.7 CacheConfig

```rust
pub struct CacheConfig {
    pub backend: CacheBackendType,
    pub max_size_bytes: u64,
    pub default_ttl_secs: u64,
    pub eviction_policy: EvictionPolicy,
    pub eviction_batch_size: usize,
    pub eviction_low_watermark: f64,
    pub bdev_name: String,
    pub cluster_size_mb: u32,
}
```

| 字段 | 类型 | 含义 | 说明 |
|------|------|------|------|
| `backend` | CacheBackendType | 后端类型 | `Spdk` / `File`（测试） |
| `max_size_bytes` | u64 | 缓存总容量上限 | 超过此值触发淘汰 |
| `default_ttl_secs` | u64 | 默认 TTL | 未指定 expire_at 时的兜底值 |
| `eviction_policy` | EvictionPolicy | 淘汰策略 | `Lru` / `Lfu` / `TtlFirst` |
| `eviction_batch_size` | usize | 单次淘汰个数 | 批量删除，减少淘汰频率 |
| `eviction_low_watermark` | f64 | 淘汰水位线 | 0.8 表示淘汰到 80% 容量后停止 |
| `bdev_name` | String | SPDK bdev 名称 | 如 `"Malloc0"` 或 NVMe bdev |
| `cluster_size_mb` | u32 | Blobstore cluster 大小 | 如 1MB |

### 4.8 CacheStats

```rust
pub struct CacheStats {
    pub total_size: u64,
    pub object_count: u64,
    pub hit_count: AtomicU64,
    pub miss_count: AtomicU64,
    pub put_count: AtomicU64,
    pub evict_count: AtomicU64,
    pub evict_bytes: AtomicU64,
}
```

| 字段 | 类型 | 含义 |
|------|------|------|
| `total_size` | u64 | 当前缓存数据总字节 |
| `object_count` | u64 | 当前缓存对象数 |
| `hit_count` | AtomicU64 | 累计缓存命中次数 |
| `miss_count` | AtomicU64 | 累计缓存未命中次数 |
| `put_count` | AtomicU64 | 累计写入次数 |
| `evict_count` | AtomicU64 | 累计淘汰对象次数 |
| `evict_bytes` | AtomicU64 | 累计淘汰字节数 |

---

## 5. 缓存策略

| 策略 | 说明 |
|------|------|
| LRU | 淘汰最久未访问 |
| LFU | 淘汰访问频率最低 |
| TTL | 按 expire_at 淘汰（独立后台扫描） |
| 容量 | 总容量上限，超限触发淘汰 |

```rust
pub enum EvictionPolicy {
    Lru,        // 按 last_accessed_at 排序淘汰
    Lfu,        // 按 access_count 排序淘汰
    TtlFirst,   // 优先淘汰已过期对象，再按 LRU
}
```

**淘汰逻辑**：

1. **TTL 扫描**（独立后台任务）：定期扫描 `expire_at < now` 的 CacheEntry，直接删除对应 Blob
2. **容量淘汰**：当 `total_size > max_size_bytes` 时，按 `EvictionPolicy` 选择淘汰对象，批量删除 `eviction_batch_size` 个，直到 `total_size ≤ max_size_bytes × eviction_low_watermark`

---

## 6. 与调度层的对接

### 5.1 接口定义

```rust
/// 缓存层提供给调度层的接口
#[async_trait]
pub trait CacheWriteApi: Send + Sync {
    async fn put_restored(&self, item: RestoredItem) -> Result<()>;
    async fn put_restored_batch(&self, items: Vec<RestoredItem>) -> Result<()>;
    async fn delete(&self, bucket: &str, key: &str, version_id: Option<&str>) -> Result<()>;
}

pub struct RestoredItem {
    pub bucket: String,
    pub key: String,
    pub version_id: Option<String>,
    pub data: Vec<u8>,
    pub checksum: Option<String>,       // SHA256 hex
    pub expire_at: DateTime<Utc>,
    pub content_type: Option<String>,   // 调度层从元数据获取后传入
    pub etag: Option<String>,           // 调度层从元数据获取后传入
}
```

| 字段 | 类型 | 含义 | 来源 |
|------|------|------|------|
| `bucket` | String | S3 桶名 | 调度层传入 |
| `key` | String | S3 对象键 | 调度层传入 |
| `version_id` | Option\<String\> | 对象版本 | 调度层从元数据获取 |
| `data` | Vec\<u8\> | 对象原始数据 | 磁带读取 |
| `checksum` | Option\<String\> | SHA256 hex 字符串 | 调度层校验后传入 |
| `expire_at` | DateTime\<Utc\> | 解冻过期时间 | 调度层从 RestoreRequest.Days 计算 |
| `content_type` | Option\<String\> | MIME 类型 | 调度层从元数据获取，用于 GET 响应透传 |
| `etag` | Option\<String\> | S3 ETag | 调度层从元数据获取，用于 GET 响应透传 |

> `delete` 方法用于接入层 DeleteObject 时清理缓存（由接入层调用）。

### 5.2 调用时机与流程

缓存层自身**不**触发元数据更新。写入完成后返回 `Result<()>` 给调度层，由调度层统一负责后续元数据变更。

```
取回调度器执行 TapeReadJob              （调度层职责）
    │
    ├─ 1. 从磁带顺序读取对象 A, B, C
    │
    ├─ 2. 每读完一个对象:
    │        cache.put_restored(bucket, key, data, checksum, expire_at).await
    │        若失败: 重试或标记 RecallTask 失败
    │
    └─ 3. 全部写入缓存后:                 （调度层负责更新元数据）
           metadata.update_restore_status(Completed, restore_expire_at)
```

> **关键原则**：缓存层是纯数据存储，不持有 MetadataClient。元数据写入入口收敛在调度层。

### 5.3 数据流与校验

| 步骤 | 负责方 | 说明 |
|------|--------|------|
| 磁带读取 | 调度器 | 带 checksum 校验 |
| 写入缓存 | 缓存层 | 存储 data + checksum，纯数据操作 |
| 更新元数据 | **调度器** | restore_status=Completed, restore_expire_at |
| GET 时校验 | 协议层/缓存层 | 可选读后校验 checksum |

### 5.4 失败与重试

- 缓存写入失败：调度器重试 2 次，仍失败则 RecallTask 标记 Failed，**不更新元数据**
- 缓存层空间不足：先触发淘汰，再写入；若仍不足则返回错误，调度器不更新元数据
- 缓存写入成功但元数据更新失败：缓存有数据但 GET 仍返回未解冻状态，调度器需重试元数据更新

---

## 7. 与元数据层的关系（方案 B：缓存层不直接依赖元数据）

### 6.1 设计原则

**缓存层不持有 MetadataClient，不直接读写元数据。**

元数据的写入入口收敛为两个：
- **接入层**：`PutObject`、`DeleteObject`
- **调度层**：归档/取回状态流转（`UpdateStorageClass`、`UpdateRestoreStatus` 等）

缓存层是纯粹的数据存储层，只暴露 `CacheReadApi` 和 `CacheWriteApi`。

### 6.2 缓存层对元数据的间接关系

| 场景 | 依赖 | 说明 |
|------|------|------|
| GET 前 | 无直接依赖 | 协议层先查元数据，若 Completed 再调缓存 |
| 淘汰时 | 无 | 基于自身 xattrs 中的 expire_at 判断，无需查元数据 |
| 写入时 | 无 | 调度层传入 expire_at，缓存层直接写入 xattr |
| 启动时 | 无 | 从 Blobstore xattrs 重建内存索引，不依赖元数据 |

### 6.3 一致性保证（由调度层负责）

| 顺序 | 操作 | 负责方 |
|------|------|--------|
| 1 | 磁带读取完成 | 调度器 |
| 2 | `cache.put_restored(...)` 成功 | 调度器 → 缓存层 |
| 3 | `metadata.update_restore_status(Completed)` | **调度器** → 元数据层 |
| 4 | GET：查元数据 → Completed → 从缓存读 | 协议层 |

若 2 成功、3 失败：缓存有数据但元数据未更新，GET 仍返回未解冻。调度器需重试元数据更新（补偿机制）。

---

## 8. 与协议层/接入层的对接

### 7.1 接口定义

```rust
/// 缓存层提供给协议层/接入层的接口
#[async_trait]
pub trait CacheReadApi: Send + Sync {
    async fn get(
        &self,
        bucket: &str,
        key: &str,
        version_id: Option<&str>,
    ) -> Result<Option<CachedObject>>;

    async fn contains(
        &self,
        bucket: &str,
        key: &str,
        version_id: Option<&str>,
    ) -> Result<bool>;
}
```

> `CachedObject` 定义见 §4.6，包含 data、size、expire_at、content_type、etag、checksum。

### 7.2 协议层调用流程

```
GET /{bucket}/{key}
    │
    ├─ 协议层/Handler: metadata.get_object(bucket, key)
    │
    ├─ if storage_class != Cold:
    │     从热存储读取，返回
    │
    ├─ if storage_class == Cold:
    │     if restore_status != Completed:
    │         返回 403 InvalidObjectState
    │     if restore_expire_at < now:
    │         返回 403 InvalidObjectState（已过期）
    │
    └─ cache.get(bucket, key)
           if Some(obj): 返回 obj.data，附带 x-amz-restore 头
           if None:      返回 404 或 503（缓存丢失，需重新 Restore）
```

### 7.3 缓存未命中处理

| 情况 | 协议层行为 |
|------|------------|
| 元数据 Completed 但缓存无数据 | 返回 503，提示重新 Restore；或触发异步取回 |
| 缓存已淘汰（TTL 内） | 同上，元数据 restore_expire_at 未过期但缓存已删 |
| 缓存损坏 | 校验失败时返回 500，建议重新 Restore |

### 7.4 响应头

GET 命中缓存时，协议层需附加：

```
x-amz-restore: ongoing-request="false", expiry-date="Fri, 28 Feb 2025 12:00:00 GMT"
```

---

## 9. 对接汇总图（方案 B）

```
┌─────────────────────────────────────────────────────────────────────────┐
│                            协议层 / 接入层                                │
│  GET: metadata.get_object → if Completed → cache.get → 返回 data          │
│  HEAD: metadata.get_object → 生成 x-amz-restore 头                         │
│  PUT/DELETE: metadata.put_object / delete_object                           │
└─────────────────────────────────────────────────────────────────────────┘
        │ 读                                 │ 读
        ▼                                    ▼
┌───────────────────┐              ┌───────────────────────────────┐
│   元数据层         │              │   缓存层（纯数据存储）            │
│  restore_status   │              │   CacheReadApi: get/contains   │
│  restore_expire_at│              │   CacheWriteApi: put_restored  │
└───────────────────┘              │   ⚡ 不持有 MetadataClient      │
        ▲                          └───────────────────────────────┘
        │ 写（唯一写入协调者）                       ▲
        │                                          │ 写
        │              ┌───────────────────────────┘
        │              │
┌───────┴──────────────┴───────────────────────────────┐
│   调度层（归档/取回）                                     │
│   1. 磁带读取 → 2. cache.put_restored → 3. 更新元数据    │
│   元数据写入的唯一协调者（除接入层 PUT/DELETE 外）          │
└──────────────────────────────────────────────────────┘
        │
        ▼
┌──────────────────┐
│   磁带层（纯硬件） │
│   不持有           │
│   MetadataClient  │
└──────────────────┘
```

---

## 10. 集成方式

- **方案 A**：缓存服务独立二进制，通过 gRPC/HTTP 与主服务通信
- **方案 B**：主进程在 SPDK block_on 内启动 Axum（需调整启动顺序）
- **方案 C**：缓存层先使用文件后端，待架构稳定后再接入 async-spdk

---

## 11. 模块结构

```
src/cache/
├── mod.rs
├── manager.rs              # CacheManager，实现 CacheReadApi + CacheWriteApi
├── index.rs                # cache_key → blob_id 索引（内存 + 可选持久化）
├── spdk/
│   ├── mod.rs
│   ├── blob_backend.rs     # SpdkBlobCacheBackend，Blobstore/Blob API 封装
│   └── config.rs
└── file/                   # 文件后端（兼容/测试）
    └── backend.rs
```

---

## 12. 配置项

```yaml
cache:
  backend: "spdk"
  spdk:
    enabled: true
    config_file: "/etc/coldstore/cache_spdk.json"
    bdev_name: "Malloc0"
    blobstore_type: "coldstore_cache"
    cluster_size_mb: 1
    max_size_gb: 100
  ttl_secs: 86400
  eviction_policy: "Lru"
```

---

## 13. 依赖关系

| 对接方 | 方向 | 接口 | 说明 |
|--------|------|------|------|
| 调度层 | 调度 → 缓存 | `CacheWriteApi`: `put_restored` / `put_restored_batch` | 取回后写数据 |
| 接入层 | 接入 → 缓存 | `CacheWriteApi`: `delete` | DeleteObject 时清理缓存 |
| 协议层/接入层 | 协议 → 缓存 | `CacheReadApi`: `get` / `contains` | GET 读数据 |
| 元数据层 | **无依赖** | — | 缓存层不持有 MetadataClient |

> **方案 B 原则**：缓存层是纯数据存储，不感知元数据。元数据写入由调度层统一协调。

---

## 14. 参考资料

- [async-spdk](https://github.com/madsys-dev/async-spdk)（含 hello_blob 示例）
- [SPDK Blobstore Programmer's Guide](https://spdk.io/doc/blob.html)
- [SPDK blob.h API](https://spdk.io/doc/blob_8h.html)
