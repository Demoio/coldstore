# 元数据集群模块设计

> 所属架构：ColdStore 冷存储系统  
> 参考：[总架构设计](../DESIGN.md)

## 1. 模块概述

元数据集群提供 ColdStore 的控制面元数据存储。当前实现分两层看待：

- **当前代码基线（Phase 1 + Phase 2A）**：MetadataService 先以进程内状态机承载所有元数据，并提供 opt-in 本地二进制 snapshot 持久化入口；该能力用于安全验证状态模型和重启恢复，不启动真实分布式集群。
- **目标架构（Phase 2B）**：将同一套元数据状态机接入 OpenRaft + RocksDB/openraft-rocksstore，实现强一致复制、持久 Raft log 和成员变更。

元数据分为两大类：

| 类别 | 内容 | 说明 |
|------|------|------|
| **集群元数据** | 节点信息、集群拓扑、Raft 状态 | 集群自身管理 |
| **归档元数据** | 对象、桶、归档包、磁带、任务 | 业务数据管理 |

### 1.1 职责

- 对象元数据：bucket、key、storage_class、archive_id、tape_id、restore_status 等
- 桶管理：桶创建/删除
- 归档包、磁带、取回任务、归档任务索引
- 集群管理：节点注册、Raft 领导选举、成员变更
- 强一致性读写、高可用

### 1.2 在架构中的位置

```
                            Console ──管控读写──▶ Metadata
                                                    ▲
Gateway ──全部请求──▶ Scheduler Worker ──读+写──────┘
                          │
                          ├──gRPC──▶ Cache Worker (纯数据)
                          └──gRPC──▶ Tape Worker  (纯硬件)
```

> Gateway 不直连 Metadata。Scheduler Worker 是唯一的元数据业务读写入口。

---

## 2. 技术选型

| 组件 | 选型 | 说明 |
|------|------|------|
| 当前状态机 | 进程内 `MetadataState` | Phase 1 已实现，覆盖对象、桶、归档包、归档任务、取回任务、磁带、Worker 注册表 |
| 当前持久化 | 二进制 snapshot 文件 | Phase 2A 已启动，opt-in 使用 `MetadataServiceImpl::new_with_snapshot(config, path)`；写后原子替换 snapshot，重启加载恢复 |
| Raft 共识 | **openraft** | Phase 2B 目标，[databendlabs/openraft](https://github.com/databendlabs/openraft) |
| Raft 存储 | **openraft-rocksstore** | Phase 2B 目标，RocksDB 后端 |
| 存储引擎 | RocksDB | Phase 2B 目标，LSM-tree，高吞吐 |

---

## 3. 核心数据结构

### 3.1 ObjectMetadata（对象元数据）

系统中最核心的结构，记录每个 S3 对象的完整状态。

```rust
pub struct ObjectMetadata {
    pub bucket: String,
    pub key: String,
    pub version_id: Option<String>,
    pub size: u64,
    pub checksum: String,
    pub content_type: Option<String>,
    pub etag: Option<String>,
    pub storage_class: StorageClass,
    pub archive_id: Option<Uuid>,
    pub tape_id: Option<String>,
    pub tape_set: Option<Vec<String>>,
    pub tape_block_offset: Option<u64>,
    pub restore_status: Option<RestoreStatus>,
    pub restore_expire_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

| 字段 | 类型 | 含义 | 写入方 | 消费方 |
|------|------|------|--------|--------|
| `bucket` | String | S3 桶名 | 接入层 PutObject | 全部 |
| `key` | String | S3 对象键 | 接入层 PutObject | 全部 |
| `version_id` | Option\<String\> | 对象版本 ID | 接入层 PutObject | 协议层、调度层 |
| `size` | u64 | 对象字节数 | 接入层 PutObject | 调度层（聚合计算）、协议层（Content-Length） |
| `checksum` | String | SHA256 hex | 接入层 PutObject | 调度层（写磁带/校验）、缓存层（透传） |
| `content_type` | Option\<String\> | MIME 类型 | 接入层 PutObject | 协议层 GET 响应头、调度层传给缓存层 |
| `etag` | Option\<String\> | S3 ETag | 接入层 PutObject | 协议层 GET 响应头、调度层传给缓存层 |
| `storage_class` | StorageClass | 存储类别 | 接入层 PutObject / 调度层归档完成 | 协议层（判断是否可 GET）、调度层（扫描 ColdPending） |
| `archive_id` | Option\<Uuid\> | 所属 ArchiveBundle ID | 调度层归档完成 | 调度层取回时定位 Bundle |
| `tape_id` | Option\<String\> | 主副本所在磁带 ID | 调度层归档完成 | 调度层取回时定位磁带 |
| `tape_set` | Option\<Vec\<String\>\> | 所有副本磁带列表 | 调度层归档完成 | 调度层副本切换 |
| `tape_block_offset` | Option\<u64\> | 对象在磁带上的块偏移 | 调度层归档完成 | 调度层取回时 seek 定位 |
| `restore_status` | Option\<RestoreStatus\> | 解冻状态 | 调度层取回流程 | 协议层（判断 GET/HEAD 响应） |
| `restore_expire_at` | Option\<DateTime\<Utc\>\> | 解冻过期时间 | 调度层取回完成 | 协议层（x-amz-restore 头）、缓存层（TTL） |
| `created_at` | DateTime\<Utc\> | 创建时间 | 接入层 PutObject | 调度层（聚合排序） |
| `updated_at` | DateTime\<Utc\> | 最后更新时间 | 每次写入 | 审计 |

### 3.2 枚举类型

```rust
pub enum StorageClass {
    ColdPending,   // 待归档（PutObject 写入即标记，等待调度器扫描写入磁带）
    Cold,          // 已归档到磁带
}

pub enum RestoreStatus {
    Pending,          // 已入队，等待调度
    WaitingForMedia,  // 磁带离线，等待人工上线
    InProgress,       // 正在从磁带读取
    Completed,        // 已写入缓存，可 GET
    Expired,          // 解冻过期
    Failed,           // 取回失败
}

pub enum RestoreTier {
    Expedited,   // 加急：1-5 分钟
    Standard,    // 标准：3-5 小时
    Bulk,        // 批量：5-12 小时
}
```

### 3.3 ArchiveBundle（归档包）

与 05-scheduler-layer §8.3 定义一致。

```rust
pub struct ArchiveBundle {
    pub id: Uuid,
    pub tape_id: String,
    pub tape_set: Vec<String>,
    pub entries: Vec<BundleEntry>,
    pub total_size: u64,
    pub filemark_start: u32,
    pub filemark_end: u32,
    pub checksum: Option<String>,
    pub status: ArchiveBundleStatus,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}
```

| 字段 | 类型 | 含义 | 写入方 |
|------|------|------|--------|
| `id` | Uuid | 归档包唯一标识 | 调度层 |
| `tape_id` | String | 主副本磁带 ID | 调度层 |
| `tape_set` | Vec\<String\> | 所有副本磁带 | 调度层 |
| `entries` | Vec\<BundleEntry\> | 包内对象列表与偏移 | 调度层 |
| `total_size` | u64 | 总字节数 | 调度层 |
| `filemark_start` | u32 | 起始 FileMark | 调度层（从 WriteResult 获取） |
| `filemark_end` | u32 | 结束 FileMark | 调度层 |
| `checksum` | Option\<String\> | 整包 SHA256 | 调度层（可选） |
| `status` | ArchiveBundleStatus | 状态 | 调度层 |
| `created_at` | DateTime\<Utc\> | 创建时间 | 调度层 |
| `completed_at` | Option\<DateTime\<Utc\>\> | 完成时间 | 调度层 |

```rust
pub struct BundleEntry {
    pub bucket: String,
    pub key: String,
    pub version_id: Option<String>,
    pub size: u64,
    pub offset_in_bundle: u64,
    pub tape_block_offset: u64,
    pub checksum: String,
}

pub enum ArchiveBundleStatus {
    Pending, Writing, Completed, Failed,
}
```

### 3.4 TapeInfo（磁带信息）

与 06-tape-layer §4.6 定义一致。由调度层写入元数据。

```rust
pub struct TapeInfo {
    pub id: String,
    pub barcode: Option<String>,
    pub format: String,
    pub status: TapeStatus,
    pub location: Option<String>,
    pub capacity_bytes: u64,
    pub used_bytes: u64,
    pub remaining_bytes: u64,
    pub archive_bundles: Vec<Uuid>,
    pub last_verified_at: Option<DateTime<Utc>>,
    pub error_count: u32,
    pub registered_at: DateTime<Utc>,
}
```

| 字段 | 类型 | 含义 | 写入方 |
|------|------|------|--------|
| `id` | String | 磁带唯一标识 | 调度层（注册时） |
| `barcode` | Option\<String\> | 条码 | 调度层 |
| `format` | String | "LTO-9"、"LTO-10" | 调度层 |
| `status` | TapeStatus | Online/Offline/Error/Retired | 调度层 |
| `location` | Option\<String\> | 位置描述 | 调度层 |
| `capacity_bytes` | u64 | 总容量 | 调度层 |
| `used_bytes` | u64 | 已使用 | 调度层（每次写入后更新） |
| `remaining_bytes` | u64 | 剩余 | 调度层 |
| `archive_bundles` | Vec\<Uuid\> | 已写入的归档包列表 | 调度层 |
| `last_verified_at` | Option\<DateTime\<Utc\>\> | 最近校验时间 | 调度层 |
| `error_count` | u32 | 累计错误次数 | 调度层 |
| `registered_at` | DateTime\<Utc\> | 注册时间 | 调度层 |

```rust
pub enum TapeStatus {
    Online, Offline, Error, Retired, Unknown,
}
```

### 3.5 RecallTask（取回任务）

与 05-scheduler-layer §8.7 定义一致。

```rust
pub struct RecallTask {
    pub id: Uuid,
    pub bucket: String,
    pub key: String,
    pub version_id: Option<String>,
    pub archive_id: Uuid,
    pub tape_id: String,
    pub tape_set: Vec<String>,
    pub tape_block_offset: u64,
    pub object_size: u64,
    pub checksum: String,
    pub tier: RestoreTier,
    pub days: u32,
    pub expire_at: DateTime<Utc>,
    pub status: RestoreStatus,
    pub drive_id: Option<String>,
    pub retry_count: u32,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}
```

### 3.6 ArchiveTask（归档任务）

与 05-scheduler-layer §8.6 定义一致。

```rust
pub struct ArchiveTask {
    pub id: Uuid,
    pub bundle_id: Uuid,
    pub tape_id: String,
    pub drive_id: Option<String>,
    pub object_count: u32,
    pub total_size: u64,
    pub bytes_written: u64,
    pub status: ArchiveTaskStatus,
    pub retry_count: u32,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

pub enum ArchiveTaskStatus {
    Pending, InProgress, Completed, Failed,
}
```

### 3.7 BucketInfo（桶信息）

```rust
pub struct BucketInfo {
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub owner: Option<String>,
    pub versioning_enabled: bool,
    pub object_count: u64,
    pub total_size: u64,
}
```

| 字段 | 类型 | 含义 | 写入方 |
|------|------|------|--------|
| `name` | String | 桶名 | 接入层 CreateBucket |
| `created_at` | DateTime\<Utc\> | 创建时间 | 接入层 |
| `owner` | Option\<String\> | 拥有者 | 接入层 |
| `versioning_enabled` | bool | 是否启用多版本 | 接入层 |
| `object_count` | u64 | 对象数 | 写入时增减 |
| `total_size` | u64 | 总大小 | 写入时增减 |

### 3.8 ListObjectsResult

```rust
pub struct ListObjectsResult {
    pub objects: Vec<ObjectMetadata>,
    pub next_marker: Option<String>,
    pub is_truncated: bool,
}
```

| 字段 | 类型 | 含义 |
|------|------|------|
| `objects` | Vec\<ObjectMetadata\> | 当前页对象列表 |
| `next_marker` | Option\<String\> | 分页游标（用于下次请求的 marker） |
| `is_truncated` | bool | 是否还有更多结果 |

### 3.10 集群元数据与部署模型

#### 3.10.1 物理部署模型

ColdStore 由五类节点组成。**Scheduler Worker 是唯一业务中枢**：Gateway 仅连 Scheduler，
Console 仅连 Metadata，三类 Worker 向 Metadata 注册并上报心跳。

```
┌────────────────┐                      ┌────────────────┐
│  Gateway (N台)  │                      │  Console (1台)  │
│  S3 HTTP 前端   │                      │  管控面 Web UI  │
│  无状态         │                      │  无状态         │
└──────┬─────────┘                      └──────┬─────────┘
       │ 配置: scheduler_addrs                  │ 配置: metadata_addrs
       ▼                                       ▼
 ┌─────────────────────────────┐    ┌──────────────────────────┐
 │  同一物理节点                  │    │  Metadata 节点 (3/5 台)    │
 │  ┌─────────────────────────┐ │    │  Raft 共识 + RocksDB      │
 │  │  Scheduler Worker       │─┼──►│  元数据 + Worker 注册中心   │
 │  │  业务中枢：调度编排       │ │    └─────────────────────────┘
 │  ├─────────────────────────┤ │               ▲
 │  │  Cache Worker           │ │               │ 心跳
 │  │  SPDK Blobstore 缓存    │ │    ┌──────────┴──────────────┐
 │  └─────────────────────────┘ │    │  Tape Worker (独立物理节点) │
 └─────────────────────────────┘    │  磁带驱动 /dev/nst*        │
                                    │  带库 /dev/sg*              │
                                    └────────────────────────────┘
```

| 节点类型 | 职责 | 数量 | 连接对象 |
|----------|------|------|----------|
| **Metadata** | Raft 共识、元数据存储、Worker 注册中心 | 3/5 | Scheduler (读写)、三类 Worker (心跳)、Console (管控) |
| **Scheduler Worker** | **业务中枢**：调度编排、对接全部层（与 Cache 同机） | 1~N | Metadata, Cache Worker, Tape Worker, Gateway |
| **Cache Worker** | SPDK NVMe 数据缓存（与 Scheduler 同机） | 1~N | Scheduler Worker (gRPC)、Metadata (心跳) |
| **Tape Worker** | 磁带驱动管理、数据读写（独立物理节点） | 1~N | Scheduler Worker (gRPC)、Metadata (心跳) |
| **Gateway** | S3 HTTP 接入 + 协议适配（无状态） | N | **仅 Scheduler Worker** |
| **Console** | 管控面 Web UI + Admin API（无状态） | 1 | **仅 Metadata** |

**关键设计决策**：

- **Gateway 不直连 Metadata**：Gateway 是纯 S3 协议前端，所有业务请求都发往 Scheduler Worker。
  Gateway 配置文件中只需写 Scheduler Worker 地址。
- **Scheduler Worker 是唯一业务中枢**：持有 MetadataClient，编排 Cache Worker 和 Tape Worker，
  接受 Gateway 的全部请求，是唯一的元数据读写业务入口。
- **Scheduler ↔ Cache 使用 gRPC**：虽然同机部署，仍通过 gRPC 通信，保持架构一致性，
  未来可独立扩展。
- **Console 仅连 Metadata**：管控面直接读写元数据集群，管理 Worker 增删。
- **Tape Worker 独立部署**：磁带机为独立物理节点，通过 gRPC 接收 Scheduler 的读写指令。

#### 3.10.2 节点间通信拓扑

```
Gateway                          Console
   │ 配置: scheduler_addrs          │ 配置: metadata_addrs
   │                                │
   └──(gRPC)──► Scheduler Worker    └──(gRPC)──► Metadata
                    │                               ▲
                    ├──(gRPC)──► Metadata ───────────┘ (元数据读写)
                    ├──(gRPC)──► Cache Worker (同机，gRPC)
                    └──(gRPC)──► Tape Worker  (远程，gRPC)
                                    │
                 三类 Worker ──(gRPC)──► Metadata (心跳注册)
```

| 通信路径 | 协议 | 场景 |
|----------|------|------|
| Gateway → Scheduler Worker | gRPC | **全部** S3 请求（PUT/GET/HEAD/DELETE/Restore） |
| Scheduler → Metadata | gRPC | 元数据读写（PutObject、扫描 ColdPending、状态更新等） |
| Scheduler → Cache Worker | gRPC（同机） | 缓存写入/读取（PutObject 暂存、GET 解冻数据） |
| Scheduler → Tape Worker | gRPC（远程） | 磁带写入/读取指令下发 |
| Console → Metadata | gRPC | 集群管理、Worker 增删、元数据查询 |
| 三类 Worker → Metadata | gRPC | 心跳上报（资源状态） |
| Metadata ↔ Metadata | Raft RPC | 共识协议 |

> Gateway **不连接** Metadata，也**不连接** Cache Worker 和 Tape Worker。
> 所有业务流量都经过 Scheduler Worker 中转。

#### 3.10.3 集群全局信息

```rust
pub struct ClusterInfo {
    pub cluster_id: String,
    pub metadata_nodes: Vec<MetadataNodeInfo>,
    pub scheduler_workers: Vec<SchedulerWorkerInfo>,
    pub cache_workers: Vec<CacheWorkerInfo>,
    pub tape_workers: Vec<TapeWorkerInfo>,
    pub leader_id: Option<u64>,
    pub term: u64,
    pub committed_index: u64,
}
```

| 字段 | 类型 | 含义 |
|------|------|------|
| `cluster_id` | String | 集群唯一标识 |
| `metadata_nodes` | Vec\<MetadataNodeInfo\> | Raft 元数据节点列表 |
| `scheduler_workers` | Vec\<SchedulerWorkerInfo\> | 调度 Worker 列表 |
| `cache_workers` | Vec\<CacheWorkerInfo\> | 缓存 Worker 列表 |
| `tape_workers` | Vec\<TapeWorkerInfo\> | 磁带 Worker 列表 |
| `leader_id` | Option\<u64\> | 当前 Raft Leader 节点 ID |
| `term` | u64 | 当前 Raft 任期 |
| `committed_index` | u64 | 已提交日志索引 |

> Gateway / Console 不出现在 `ClusterInfo` 中。
> Gateway 通过配置直连 Scheduler Worker；Console 通过配置直连 Metadata。

#### 3.10.4 公共类型

```rust
pub enum NodeStatus {
    Online,       // 正常服务
    Offline,      // 失联（心跳超时）
    Draining,     // 排空中（不再分配新任务，等待现有任务完成）
    Maintenance,  // 维护模式（管理员手动设置）
}

pub enum RaftRole {
    Leader,
    Follower,
    Learner,
}

pub enum WorkerType {
    Scheduler,
    Cache,
    Tape,
}
```

#### 3.10.5 MetadataNodeInfo（元数据节点）

```rust
pub struct MetadataNodeInfo {
    pub node_id: u64,
    pub addr: String,              // Raft RPC + gRPC 服务地址
    pub raft_role: RaftRole,
    pub last_heartbeat: Option<DateTime<Utc>>,
    pub status: NodeStatus,
}
```

| 字段 | 类型 | 含义 |
|------|------|------|
| `node_id` | u64 | Raft 节点 ID |
| `addr` | String | 服务地址（`host:port`），供 Console/Worker 连接 |
| `raft_role` | RaftRole | 当前 Raft 角色 |
| `last_heartbeat` | Option\<DateTime\<Utc\>\> | Raft 内部心跳时间 |
| `status` | NodeStatus | 节点状态 |

#### 3.10.6 SchedulerWorkerInfo（调度 Worker）

调度 Worker 是 ColdStore 的**业务中枢**，接受 Gateway 全部 S3 请求，编排 Cache Worker 和 Tape Worker。
物理上与 Cache Worker **同机部署**，通过 gRPC 通信（保持架构一致性）。

```rust
pub struct SchedulerWorkerInfo {
    pub node_id: u64,
    pub addr: String,                  // gRPC 服务地址
    pub status: NodeStatus,
    pub last_heartbeat: Option<DateTime<Utc>>,

    // ── 调度状态 ──
    pub is_active: bool,               // 主备模式下是否为 active scheduler
    pub pending_archive_tasks: u64,    // 待归档任务数
    pub pending_recall_tasks: u64,     // 待取回任务数
    pub active_jobs: u64,              // 正在执行的作业数

    // ── 关联的 Cache Worker ──
    pub paired_cache_worker_id: u64,   // 同机部署的 Cache Worker ID
}
```

| 字段 | 类型 | 含义 | 心跳更新 |
|------|------|------|:---:|
| `node_id` | u64 | 全局唯一 ID | 否 |
| `addr` | String | gRPC 地址 | 否 |
| `status` | NodeStatus | 节点状态 | 是 |
| `is_active` | bool | 是否为活跃调度器（多 Scheduler 时仅一个 active） | 是 |
| `pending_archive_tasks` | u64 | 待归档任务数 | 是 |
| `pending_recall_tasks` | u64 | 待取回任务数 | 是 |
| `active_jobs` | u64 | 正在执行作业数 | 是 |
| `paired_cache_worker_id` | u64 | 同机 Cache Worker 的 ID | 否（注册时固定） |

#### 3.10.7 CacheWorkerInfo（缓存 Worker）

缓存 Worker 运行 SPDK Blobstore，提供高性能 NVMe 数据缓存。
物理上与 Scheduler Worker **同机部署**。

```rust
pub struct CacheWorkerInfo {
    pub node_id: u64,
    pub addr: String,                  // gRPC 服务地址（供远程 GET 读取解冻数据）
    pub status: NodeStatus,
    pub last_heartbeat: Option<DateTime<Utc>>,

    // ── 缓存资源 ──
    pub bdev_name: String,             // SPDK bdev 名称（如 "NVMe0n1"）
    pub total_capacity: u64,           // 总容量 (bytes)
    pub used_capacity: u64,            // 已用容量 (bytes)
    pub blob_count: u64,              // 当前缓存对象数
    pub io_unit_size: u32,             // SPDK io_unit 大小 (bytes)
}
```

| 字段 | 类型 | 含义 | 心跳更新 |
|------|------|------|:---:|
| `node_id` | u64 | 全局唯一 ID | 否 |
| `addr` | String | gRPC 地址（Gateway GET 时连接此地址） | 否 |
| `status` | NodeStatus | 节点状态 | 是 |
| `bdev_name` | String | SPDK bdev 名称 | 否（注册时固定） |
| `total_capacity` | u64 | NVMe 总容量 | 否 |
| `used_capacity` | u64 | 已用容量 | 是 |
| `blob_count` | u64 | 缓存对象数 | 是 |
| `io_unit_size` | u32 | SPDK io_unit 大小 | 否 |

#### 3.10.8 TapeWorkerInfo（磁带 Worker）

磁带 Worker 独立部署于挂载磁带驱动和带库的物理节点上，
通过 gRPC 接收调度器的读写指令。

```rust
pub struct TapeWorkerInfo {
    pub node_id: u64,
    pub addr: String,                  // gRPC 服务地址
    pub status: NodeStatus,
    pub last_heartbeat: Option<DateTime<Utc>>,

    // ── 磁带驱动 ──
    pub drives: Vec<DriveEndpoint>,

    // ── 带库（可选，无带库时为 None） ──
    pub library: Option<LibraryEndpoint>,
}

/// 磁带驱动端点
pub struct DriveEndpoint {
    pub drive_id: String,              // 驱动唯一标识
    pub device_path: String,           // 设备路径 /dev/nst0
    pub drive_type: String,            // 驱动类型 "LTO-9" / "LTO-10"
    pub status: DriveStatus,
    pub current_tape: Option<String>,  // 当前装载的磁带 ID
}

pub enum DriveStatus {
    Idle,       // 空闲可分配
    InUse,      // 正在执行读写任务
    Loading,    // 正在装载磁带
    Unloading,  // 正在卸载磁带
    Error,      // 故障
    Offline,    // 离线维护
}

/// 带库端点
pub struct LibraryEndpoint {
    pub device_path: String,           // /dev/sg5
    pub slot_count: u32,               // 存储槽位数
    pub import_export_count: u32,      // 进出槽位数（邮箱）
    pub drive_count: u32,              // 带库中驱动数量
}
```

| 字段 | 类型 | 含义 | 心跳更新 |
|------|------|------|:---:|
| `node_id` | u64 | 全局唯一 ID | 否 |
| `addr` | String | gRPC 地址（调度器连接此地址下发指令） | 否 |
| `status` | NodeStatus | 节点状态 | 是 |
| `drives[].status` | DriveStatus | 每个驱动的当前状态 | 是 |
| `drives[].current_tape` | Option\<String\> | 驱动中当前磁带 ID | 是 |
| `drives[].drive_type` | String | 驱动型号 | 否 |
| `library.slot_count` | u32 | 槽位总数 | 否 |

#### 3.10.9 节点注册、心跳与管理

```
                      Console (添加/下线操作)
                          │
                          ▼
  ┌──────────────┐   RegisterWorker    ┌──────────────────────┐
  │ Scheduler    │ ─────────────────► │                      │
  │ Worker       │   Heartbeat (5s)   │   Metadata Cluster   │
  ├──────────────┤ ─────────────────► │   (Raft + RocksDB)   │
  │ Cache Worker │   Heartbeat (5s)   │                      │
  ├──────────────┤ ─────────────────► │  cf_scheduler_workers │
  │ Tape Worker  │   Heartbeat (5s)   │  cf_cache_workers     │
  └──────────────┘                    │  cf_tape_workers      │
                                      └──────────────────────┘
```

**注册流程**：

1. 管理员通过 Console 添加 Worker（指定类型、地址等基本信息）
2. Console 调用 Metadata 的 `RegisterXxxWorker` 写入集群信息
3. Worker 进程启动后连接 Metadata，确认自身已注册，开始心跳上报

**心跳机制**：

- **频率**：每 5s 一次
- **内容**：各 Worker 上报自身实时状态（队列深度、缓存容量、驱动状态等）
- **存储**：`last_heartbeat` 由 Metadata Leader 本地内存更新，**不写 Raft 日志**
- **状态变更走 Raft**：仅当状态发生实质变更时（Online ↔ Offline、驱动状态切换）
  才通过 Raft Propose 持久化

**失联检测**：

- Metadata Leader 每 15s 扫描，连续 3 次未收到心跳（>15s）的 Worker 标记为 `Offline`
- Offline 的 Tape Worker 上的驱动不再被调度器分配任务

**Leader 切换时的 last_heartbeat 处理**：

`last_heartbeat` 仅在 Leader 本地内存维护，不写入 Raft 日志。Leader 切换后，新 Leader 
的 `last_heartbeat` 表为空，需要特殊处理以避免误判所有 Worker 为 Offline：

1. **宽限期（Grace Period）**：新 Leader 就任后，设置一个等于 2 倍心跳间隔（即 10s）的宽限期。
   宽限期内不执行失联检测
2. **首次心跳恢复**：宽限期内收到的首次心跳即恢复该 Worker 的 `last_heartbeat` 记录
3. **宽限期后处理**：宽限期结束后，仍未收到心跳的 Worker 标记为 `Offline`
   （这些 Worker 确实已失联，不是 Leader 切换造成的误判）

```rust
pub struct HeartbeatManager {
    last_heartbeat: HashMap<(WorkerType, u64), Instant>,
    leader_since: Option<Instant>,
    grace_period: Duration,  // 默认 10s（2 × heartbeat_interval）
}

impl HeartbeatManager {
    fn on_become_leader(&mut self) {
        self.last_heartbeat.clear();
        self.leader_since = Some(Instant::now());
    }

    fn is_in_grace_period(&self) -> bool {
        self.leader_since
            .map(|since| since.elapsed() < self.grace_period)
            .unwrap_or(false)
    }

    fn check_offline(&self) -> Vec<(WorkerType, u64)> {
        if self.is_in_grace_period() {
            return vec![];
        }
        // 正常失联检测逻辑...
    }
}
```

**持久化策略**：`last_heartbeat` 本身不需要持久化到 Raft（高频写入会占满日志）。
Leader 切换后通过宽限期机制即可恢复状态，代价仅为一个短暂的窗口期（10s）内
不进行失联检测，对系统可用性影响极小。

**优雅下线**：

1. 管理员通过 Console 发起 Drain（排空）
2. Worker 状态变为 `Draining`，不再接受新任务
3. 等待正在执行的任务完成
4. 管理员确认后通过 Console 执行 Deregister

#### 3.10.10 配置与服务发现

各节点的配置各不相同，遵循"最小知识"原则：

```yaml
# Gateway 配置 — 仅需 Scheduler Worker 地址
gateway:
  scheduler_addrs:
    - "10.0.2.1:22001"
    - "10.0.2.2:22001"

# Console 配置 — 仅需 Metadata 地址
console:
  metadata_addrs:
    - "10.0.1.1:21001"
    - "10.0.1.2:21001"
    - "10.0.1.3:21001"

# Scheduler Worker 配置 — 需要 Metadata 地址（Cache/Tape 通过 Metadata 发现）
scheduler_worker:
  metadata_addrs:
    - "10.0.1.1:21001"
    - "10.0.1.2:21001"
    - "10.0.1.3:21001"

# Cache Worker / Tape Worker 配置 — 仅需 Metadata 地址（用于心跳注册）
cache_worker:
  metadata_addrs:
    - "10.0.1.1:21001"
    - "10.0.1.2:21001"
    - "10.0.1.3:21001"
```

**Scheduler Worker 的服务发现**：Scheduler 启动时通过 Metadata 查询在线的 Cache Worker 和 Tape Worker：

```rust
async fn discover_workers(metadata: &MetadataClient) -> WorkerTopology {
    let caches = metadata.list_online_cache_workers().await?;
    let tapes = metadata.list_online_tape_workers().await?;
    WorkerTopology { caches, tapes }
}
```

| 场景 | 发现路径 |
|------|----------|
| Gateway 全部请求 | 配置的 Scheduler 地址 → gRPC 发送请求 |
| Scheduler 操作缓存 | 查 Metadata → 获取 Cache Worker 地址 → gRPC 读写缓存 |
| Scheduler 操作磁带 | 查 Metadata → 获取 Tape Worker 地址 → gRPC 下发磁带指令 |
| Console 管理集群 | 配置的 Metadata 地址 → gRPC 管控操作 |

---

## 4. Column Family 设计

| CF 名称 | Key 格式 | Value 类型 | 说明 |
|---------|----------|-----------|------|
| `cf_objects` | `obj:{bucket}:{key}` | ObjectMetadata | 对象元数据（当前版本） |
| `cf_object_versions` | `objv:{bucket}:{key}:{version_id}` | ObjectMetadata | 多版本对象 |
| `cf_buckets` | `bkt:{bucket}` | BucketInfo | 桶信息 |
| `cf_bundles` | `bundle:{uuid}` | ArchiveBundle | 归档包（含 entries） |
| `cf_tapes` | `tape:{tape_id}` | TapeInfo | 磁带信息 |
| `cf_recall_tasks` | `recall:{uuid}` | RecallTask | 取回任务 |
| `cf_archive_tasks` | `archive:{uuid}` | ArchiveTask | 归档任务 |
| `cf_idx_bundle_objects` | `ibo:{archive_id}:{bucket}:{key}` | 空 | 归档包→对象 反查索引 |
| `cf_idx_tape_bundles` | `itb:{tape_id}:{bundle_id}` | 空 | 磁带→归档包 反查索引 |
| `cf_idx_pending` | `pend:{created_at}:{bucket}:{key}` | 空 | ColdPending 对象扫描索引（按时间排序） |
| `cf_idx_recall_by_tape` | `rbt:{tape_id}:{recall_id}` | 空 | 按磁带查取回任务（合并用） |
| `cf_scheduler_workers` | `sw:{node_id}` | SchedulerWorkerInfo | 调度 Worker 注册信息 |
| `cf_cache_workers` | `cw:{node_id}` | CacheWorkerInfo | 缓存 Worker 注册信息 |
| `cf_tape_workers` | `tw:{node_id}` | TapeWorkerInfo | 磁带 Worker 注册信息 |

---

## 5. MetadataService trait：面向各层的 API

### 5.1 设计原则

MetadataService 拆分为多个子 trait，按消费方组织：

```
                     ┌──────────────────────────────┐
                     │     MetadataService          │
                     │  impl ObjectApi              │
                     │  impl BucketApi              │
                     │  impl ArchiveApi             │
                     │  impl RecallApi              │
                     │  impl TapeApi                │
                     │  impl ClusterApi             │
                     └──────────────────────────────┘
                          │           │
            ┌─────────────┘           └───────────────┐
            ▼                                         ▼
     接入层/协议层                                  调度层
   ObjectApi (读+写)                         ArchiveApi (读+写)
   BucketApi (读+写)                         RecallApi (读+写)
   RecallApi (只写:创建)                      TapeApi (读+写)
                                             ObjectApi (读+写)
```

### 5.2 ObjectApi（对象元数据）

```rust
#[async_trait]
pub trait ObjectApi: Send + Sync {
    // ── 接入层使用 ──
    async fn put_object(&self, meta: ObjectMetadata) -> Result<()>;
    async fn get_object(&self, bucket: &str, key: &str) -> Result<Option<ObjectMetadata>>;
    async fn get_object_version(
        &self, bucket: &str, key: &str, version_id: &str,
    ) -> Result<Option<ObjectMetadata>>;
    async fn delete_object(&self, bucket: &str, key: &str) -> Result<()>;
    async fn head_object(&self, bucket: &str, key: &str) -> Result<Option<ObjectMetadata>>;
    async fn list_objects(
        &self, bucket: &str, prefix: Option<&str>, marker: Option<&str>, max_keys: u32,
    ) -> Result<ListObjectsResult>;

    // ── 调度层使用 ──
    async fn update_storage_class(
        &self, bucket: &str, key: &str, class: StorageClass,
    ) -> Result<()>;
    async fn update_archive_location(
        &self,
        bucket: &str,
        key: &str,
        archive_id: Uuid,
        tape_id: &str,
        tape_set: Vec<String>,
        tape_block_offset: u64,
    ) -> Result<()>;
    async fn update_restore_status(
        &self,
        bucket: &str,
        key: &str,
        status: RestoreStatus,
        expire_at: Option<DateTime<Utc>>,
    ) -> Result<()>;
    async fn scan_cold_pending(
        &self, limit: u32,
    ) -> Result<Vec<ObjectMetadata>>;
}
```

| 方法 | 消费方 | 说明 |
|------|--------|------|
| `put_object` | Scheduler（代理 Gateway PutObject） | 创建/覆盖对象元数据 |
| `get_object` | Scheduler（代理 Gateway GET/HEAD） | 查询当前版本 |
| `get_object_version` | Scheduler | 查询指定版本 |
| `delete_object` | Scheduler（代理 Gateway DELETE） | 删除对象 |
| `head_object` | Scheduler（代理 Gateway HEAD） | 返回 storage_class、restore_status |
| `list_objects` | Scheduler（代理 Gateway ListObjects） | ListObjects |
| `update_storage_class` | 调度层 | 归档完成后 ColdPending → Cold |
| `update_archive_location` | 调度层 | 归档完成后写入 archive_id、tape_id、tape_block_offset |
| `update_restore_status` | 调度层 | 取回状态流转 + expire_at |
| `scan_cold_pending` | 调度层 | 扫描待归档对象（使用 `cf_idx_pending` 索引） |

### 5.3 BucketApi（桶管理）

```rust
#[async_trait]
pub trait BucketApi: Send + Sync {
    async fn create_bucket(&self, info: BucketInfo) -> Result<()>;
    async fn get_bucket(&self, name: &str) -> Result<Option<BucketInfo>>;
    async fn delete_bucket(&self, name: &str) -> Result<()>;
    async fn list_buckets(&self) -> Result<Vec<BucketInfo>>;
}
```

| 方法 | 消费方 | 说明 |
|------|--------|------|
| `create_bucket` | 接入层 | CreateBucket |
| `delete_bucket` | 接入层 | DeleteBucket（需检查桶为空） |
| `list_buckets` | 接入层 | ListBuckets |

### 5.4 ArchiveApi（归档元数据）

```rust
#[async_trait]
pub trait ArchiveApi: Send + Sync {
    async fn put_archive_bundle(&self, bundle: ArchiveBundle) -> Result<()>;
    async fn get_archive_bundle(&self, id: Uuid) -> Result<Option<ArchiveBundle>>;
    async fn update_archive_bundle_status(
        &self, id: Uuid, status: ArchiveBundleStatus,
    ) -> Result<()>;
    async fn list_bundles_by_tape(&self, tape_id: &str) -> Result<Vec<Uuid>>;

    async fn put_archive_task(&self, task: ArchiveTask) -> Result<()>;
    async fn get_archive_task(&self, id: Uuid) -> Result<Option<ArchiveTask>>;
    async fn update_archive_task(&self, task: ArchiveTask) -> Result<()>;
    async fn list_pending_archive_tasks(&self) -> Result<Vec<ArchiveTask>>;
}
```

| 方法 | 消费方 | 说明 |
|------|--------|------|
| `put_archive_bundle` | 调度层 | 归档完成后写入 Bundle（含 entries） |
| `get_archive_bundle` | 调度层 | 取回时获取 Bundle（含 entries、filemark_start） |
| `list_bundles_by_tape` | 调度层 | 查询磁带上所有 Bundle（使用 `cf_idx_tape_bundles`） |
| `put_archive_task` | 调度层 | 创建归档任务 |
| `update_archive_task` | 调度层 | 更新任务状态/进度 |

### 5.5 RecallApi（取回元数据）

```rust
#[async_trait]
pub trait RecallApi: Send + Sync {
    async fn put_recall_task(&self, task: RecallTask) -> Result<()>;
    async fn get_recall_task(&self, id: Uuid) -> Result<Option<RecallTask>>;
    async fn update_recall_task(&self, task: RecallTask) -> Result<()>;
    async fn list_pending_recall_tasks(&self) -> Result<Vec<RecallTask>>;
    async fn list_recall_tasks_by_tape(&self, tape_id: &str) -> Result<Vec<RecallTask>>;
    async fn find_active_recall(
        &self, bucket: &str, key: &str,
    ) -> Result<Option<RecallTask>>;
}
```

| 方法 | 消费方 | 说明 |
|------|--------|------|
| `put_recall_task` | Scheduler（代理 Gateway RestoreObject） | 创建取回任务 |
| `get_recall_task` | Scheduler | 查询任务详情 |
| `update_recall_task` | Scheduler | 更新状态 Pending → InProgress → Completed |
| `list_pending_recall_tasks` | Scheduler | 扫描待调度任务 |
| `list_recall_tasks_by_tape` | Scheduler | 合并同磁带任务（使用 `cf_idx_recall_by_tape`） |
| `find_active_recall` | Scheduler（代理 RestoreObject 时检测重复） | RestoreAlreadyInProgress |

### 5.6 TapeApi（磁带元数据）

```rust
#[async_trait]
pub trait TapeApi: Send + Sync {
    async fn put_tape(&self, info: TapeInfo) -> Result<()>;
    async fn get_tape(&self, tape_id: &str) -> Result<Option<TapeInfo>>;
    async fn update_tape(&self, info: TapeInfo) -> Result<()>;
    async fn list_tapes(&self) -> Result<Vec<TapeInfo>>;
    async fn list_tapes_by_status(&self, status: TapeStatus) -> Result<Vec<TapeInfo>>;
}
```

| 方法 | 消费方 | 说明 |
|------|--------|------|
| `put_tape` | 调度层 | 注册新磁带 |
| `get_tape` | 调度层 | 查询磁带信息 |
| `update_tape` | 调度层 | 更新 used_bytes、status、error_count 等 |
| `list_tapes_by_status` | 调度层 | 筛选可用磁带（Online + 有剩余空间） |

### 5.7 ClusterApi（集群管理）

```rust
#[async_trait]
pub trait ClusterApi: Send + Sync {
    // ── Raft 状态 ──
    async fn cluster_info(&self) -> Result<ClusterInfo>;
    async fn is_leader(&self) -> bool;
    async fn ensure_linearizable(&self) -> Result<()>;

    // ── Scheduler Worker ──
    async fn register_scheduler_worker(&self, info: SchedulerWorkerInfo) -> Result<()>;
    async fn deregister_scheduler_worker(&self, node_id: u64) -> Result<()>;
    async fn list_online_scheduler_workers(&self) -> Result<Vec<SchedulerWorkerInfo>>;
    async fn get_scheduler_worker(&self, node_id: u64) -> Result<Option<SchedulerWorkerInfo>>;

    // ── Cache Worker ──
    async fn register_cache_worker(&self, info: CacheWorkerInfo) -> Result<()>;
    async fn deregister_cache_worker(&self, node_id: u64) -> Result<()>;
    async fn list_online_cache_workers(&self) -> Result<Vec<CacheWorkerInfo>>;
    async fn get_cache_worker(&self, node_id: u64) -> Result<Option<CacheWorkerInfo>>;

    // ── Tape Worker ──
    async fn register_tape_worker(&self, info: TapeWorkerInfo) -> Result<()>;
    async fn deregister_tape_worker(&self, node_id: u64) -> Result<()>;
    async fn list_online_tape_workers(&self) -> Result<Vec<TapeWorkerInfo>>;
    async fn get_tape_worker(&self, node_id: u64) -> Result<Option<TapeWorkerInfo>>;

    // ── 通用 Worker 操作 ──
    async fn update_worker_status(&self, worker_type: WorkerType, node_id: u64, status: NodeStatus) -> Result<()>;
    async fn drain_worker(&self, worker_type: WorkerType, node_id: u64) -> Result<()>;
}
```

| 方法 | 消费方 | 说明 |
|------|--------|------|
| `cluster_info` | Console/Scheduler | 获取集群全局信息 |
| `register_xxx_worker` | Console（管理员添加） | 注册 Worker 到集群 |
| `deregister_xxx_worker` | Console（管理员下线） | 从集群移除 Worker |
| `list_online_xxx_workers` | Scheduler（服务发现） | 获取在线 Cache/Tape Worker 列表 |
| `update_worker_status` | Metadata Leader | 心跳超时时更新状态 |
| `drain_worker` | Console | 排空 Worker（不再分配新任务） |

### 5.8 MetadataService（聚合类型）

```rust
pub struct MetadataService {
    raft: Arc<Raft<ColdStoreTypeConfig>>,
    store: Arc<RocksStore>,
}

impl ObjectApi for MetadataService { /* ... */ }
impl BucketApi for MetadataService { /* ... */ }
impl ArchiveApi for MetadataService { /* ... */ }
impl RecallApi for MetadataService { /* ... */ }
impl TapeApi for MetadataService { /* ... */ }
impl ClusterApi for MetadataService { /* ... */ }
```

为方便外部引用，提供类型别名：

```rust
pub type MetadataClient = MetadataService;
```

调度层和接入层注入时使用 `Arc<MetadataClient>`（即 `Arc<MetadataService>`）:

```rust
// 接入层
pub struct S3Handler {
    metadata: Arc<MetadataClient>,   // ObjectApi + BucketApi + RecallApi
    cache: Arc<dyn CacheReadApi>,
}

// 调度层
pub struct ArchiveScheduler {
    metadata: Arc<MetadataClient>,   // ObjectApi + ArchiveApi + TapeApi
    tape_manager: Arc<TapeManager>,
}

pub struct RecallScheduler {
    metadata: Arc<MetadataClient>,   // ObjectApi + RecallApi + ArchiveApi + TapeApi
    tape_manager: Arc<TapeManager>,
    cache: Arc<dyn CacheWriteApi>,
}
```

---

## 6. Raft 状态机命令

```rust
pub enum ColdStoreRequest {
    // ── 对象 ──
    PutObject(ObjectMetadata),
    DeleteObject { bucket: String, key: String },
    UpdateStorageClass { bucket: String, key: String, class: StorageClass },
    UpdateArchiveLocation {
        bucket: String, key: String,
        archive_id: Uuid, tape_id: String, tape_set: Vec<String>, tape_block_offset: u64,
    },
    UpdateRestoreStatus {
        bucket: String, key: String,
        status: RestoreStatus, expire_at: Option<DateTime<Utc>>,
    },

    // ── 桶 ──
    CreateBucket(BucketInfo),
    DeleteBucket { name: String },

    // ── 归档 ──
    PutArchiveBundle(ArchiveBundle),
    UpdateArchiveBundleStatus { id: Uuid, status: ArchiveBundleStatus },
    PutArchiveTask(ArchiveTask),
    UpdateArchiveTask(ArchiveTask),

    // ── 取回 ──
    PutRecallTask(RecallTask),
    UpdateRecallTask(RecallTask),

    // ── 磁带 ──
    PutTape(TapeInfo),
    UpdateTape(TapeInfo),

    // ── Worker 注册（三类） ──
    RegisterSchedulerWorker(SchedulerWorkerInfo),
    DeregisterSchedulerWorker { node_id: u64 },
    RegisterCacheWorker(CacheWorkerInfo),
    DeregisterCacheWorker { node_id: u64 },
    RegisterTapeWorker(TapeWorkerInfo),
    DeregisterTapeWorker { node_id: u64 },

    // ── Worker 状态变更 ──
    UpdateWorkerStatus { worker_type: WorkerType, node_id: u64, status: NodeStatus },
}
```

| 命令 | 写入方 | 说明 |
|------|--------|------|
| `PutObject` | Scheduler Worker（代理 Gateway 请求） | 创建对象（storage_class = ColdPending） |
| `DeleteObject` | Scheduler Worker（代理 Gateway 请求） | 删除对象 |
| `UpdateStorageClass` | Scheduler Worker | ColdPending → Cold |
| `UpdateArchiveLocation` | Scheduler Worker | 归档完成后写入磁带位置 |
| `UpdateRestoreStatus` | Scheduler Worker | 取回状态流转（含 expire_at） |
| `PutArchiveBundle` | Scheduler Worker | 创建归档包 |
| `PutRecallTask` | Scheduler Worker（代理 Gateway RestoreObject） | 创建取回任务 |
| `UpdateRecallTask` | Scheduler Worker | 更新取回任务状态 |
| `PutTape` / `UpdateTape` | Scheduler Worker | 磁带信息管理 |
| `RegisterXxxWorker` | Console（管理员添加） | 三类 Worker 注册 |
| `DeregisterXxxWorker` | Console（管理员下线） | 三类 Worker 注销 |
| `UpdateWorkerStatus` | Metadata Leader | 仅在状态变更时写入（Online ↔ Offline 等） |

> **心跳优化**：三类 Worker 每 5s 上报心跳，但 `last_heartbeat` 仅在 Leader 本地内存更新，
> 不写 Raft 日志。仅当**状态发生实质变更**（节点上下线、驱动状态切换）时才通过
> `UpdateWorkerStatus` 走 Raft Propose，避免高频心跳占满日志。

---

## 7. 读写路径

| 操作 | 路径 | 一致性 |
|------|------|--------|
| 写 | Client → Leader → Raft Propose → 多数派 Append → Apply → RocksDB | 强一致 |
| 读（默认） | 任意节点 → 本地 RocksDB | 最终一致 |
| 线性读 | `raft.ensure_linearizable().await` → 本地 RocksDB | 强一致 |

**写入方使用建议**：

| 操作 | 一致性要求 | 建议 |
|------|-----------|------|
| PutObject / DeleteObject | 强一致 | 写入走 Raft |
| RestoreObject 检查重复 | 强一致 | `find_active_recall` 使用线性读 |
| GET 查 restore_status | 可容忍短暂延迟 | 默认读即可 |
| scan_cold_pending | 可容忍短暂延迟 | 默认读 |
| 归档完成更新 | 强一致 | 写入走 Raft |

---

## 8. 架构图

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                       MetadataService                                        │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  ObjectApi | BucketApi | ArchiveApi | RecallApi | TapeApi | ClusterApi │  │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                                    │                                         │
│                         ┌──────────┴──────────┐                              │
│                         ▼                     ▼                              │
│                   ┌──────────┐          ┌──────────┐                        │
│                   │ 写路径   │          │ 读路径   │                        │
│                   │ Raft     │          │ 本地     │                        │
│                   │ Propose  │          │ RocksDB  │                        │
│                   └────┬─────┘          └──────────┘                        │
│                        ▼                                                     │
│            ┌───────────────────────┐                                        │
│            │   Raft Core (openraft) │                                        │
│            │   多数派复制 → Apply    │                                        │
│            └───────────┬───────────┘                                        │
│                        ▼                                                     │
│            ┌───────────────────────┐                                        │
│            │  RocksDB              │                                        │
│            │  cf_objects           │                                        │
│            │  cf_bundles           │                                        │
│            │  cf_tapes             │                                        │
│            │  cf_recall_tasks      │                                        │
│            │  cf_archive_tasks     │                                        │
│            │  cf_buckets           │                                        │
│            │  cf_idx_*             │                                        │
│            └───────────────────────┘                                        │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 9. 模块结构

```
src/metadata/
├── mod.rs                   # pub use，MetadataService 导出
├── service.rs               # MetadataService 结构体，聚合所有 trait 实现
├── traits/                  # ─── trait 定义 ───
│   ├── mod.rs
│   ├── object.rs            # ObjectApi
│   ├── bucket.rs            # BucketApi
│   ├── archive.rs           # ArchiveApi
│   ├── recall.rs            # RecallApi
│   ├── tape.rs              # TapeApi
│   └── cluster.rs           # ClusterApi
├── models/                  # ─── 数据结构 ───
│   ├── mod.rs
│   ├── object.rs            # ObjectMetadata, StorageClass, RestoreStatus
│   ├── bundle.rs            # ArchiveBundle, BundleEntry, ArchiveBundleStatus
│   ├── tape.rs              # TapeInfo, TapeStatus
│   ├── task.rs              # RecallTask, ArchiveTask, RestoreTier
│   ├── bucket.rs            # BucketInfo
│   ├── cluster.rs           # ClusterInfo, MetadataNodeInfo, RaftRole, NodeStatus, WorkerType
│   └── worker.rs            # SchedulerWorkerInfo, CacheWorkerInfo, TapeWorkerInfo, DriveEndpoint, LibraryEndpoint
├── raft/                    # ─── Raft 实现 ───
│   ├── mod.rs
│   ├── client.rs            # Raft 客户端（Propose + 线性读）
│   ├── store.rs             # RocksStore 扩展（CF 初始化）
│   ├── state_machine.rs     # ColdStoreRequest Apply 逻辑
│   └── network.rs           # 节点间 RPC
└── index.rs                 # 二级索引维护逻辑
```

---

## 10. 配置项

```yaml
metadata:
  backend: "RaftRocksDB"
  raft:
    node_id: 1
    cluster: "1:127.0.0.1:21001,2:127.0.0.1:21002,3:127.0.0.1:21003"
    data_path: "/var/lib/coldstore/metadata"
    snapshot_interval: 10000        # 每 10000 条日志做一次 snapshot
    heartbeat_interval_ms: 200
    election_timeout_ms: 1000
  rocksdb:
    max_open_files: 1024
    write_buffer_size_mb: 64
    max_background_jobs: 4
```

---

## 11. 依赖关系（方案 B）

### 11.1 元数据写入入口

| 写入方 | 使用的 trait | 操作 |
|--------|-------------|------|
| **接入层** | ObjectApi, BucketApi | PutObject, DeleteObject, CreateBucket |
| **协议层** | RecallApi | PutRecallTask（RestoreObject 时创建） |
| **调度层** | ObjectApi, ArchiveApi, RecallApi, TapeApi | 归档/取回全流程状态流转 |

### 11.2 元数据读取方

| 读取方 | 使用的 trait | 用途 |
|--------|-------------|------|
| 接入层/协议层 | ObjectApi, BucketApi, RecallApi | 查询对象状态、桶列表、检测重复 Restore |
| 调度层 | ObjectApi, ArchiveApi, RecallApi, TapeApi | 扫描 ColdPending、查磁带位置、合并任务 |

### 11.3 不直接依赖的层

| 层 | 关系 |
|------|------|
| 缓存层 | **不持有 MetadataService**，由调度层编排 |
| 磁带层 | **不持有 MetadataService**，由调度层编排 |

---

## 12. 参考资料

- [OpenRaft](https://github.com/databendlabs/openraft)
- [openraft-rocksstore](https://crates.io/crates/openraft-rocksstore)
- [RocksDB Column Families](https://github.com/facebook/rocksdb/wiki/Column-Families)
