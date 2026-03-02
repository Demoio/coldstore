# ColdStore 详细设计方案

> 版本：1.1  
> 更新日期：2025-02-27

## 1. 文档概述

本文档描述 ColdStore 冷存储系统的详细技术设计方案，重点涵盖：

> **模块设计文档**：各层独立设计详见 [modules/](modules/) 目录。

1. **协议层**：兼容 S3 冷归档协议（Glacier 语义）
2. **集群元数据**：基于 [OpenRaft](https://github.com/databendlabs/openraft) + RocksDB 持久化
3. **数据缓存层**：基于 [async-spdk](https://github.com/madsys-dev/async-spdk) 的高性能用户态缓存

### 1.1 核心技术选型（已确定）

| 组件 | 库/方案 | 说明 |
|------|---------|------|
| Raft 共识 | **openraft** | [databendlabs/openraft](https://github.com/databendlabs/openraft)，异步 Raft，Tokio 驱动 |
| Raft 存储 | **openraft-rocksstore** | RocksDB 后端，RaftLogStorage + RaftStateMachine |
| SPDK 绑定 | **async-spdk** | [madsys-dev/async-spdk](https://github.com/madsys-dev/async-spdk)，原生 async/await |
| 磁带 SDK | **自研抽象层** | 前期对接 Linux SCSI（st 驱动 + MTIO ioctl），后期可扩展厂商 SDK |

---

## 2. 系统架构总览

### 2.1 物理部署模型

ColdStore 由五类节点组成。**Scheduler Worker 是唯一的业务中枢**，
Gateway 仅连接 Scheduler Worker，Console 仅连接 Metadata。

```
┌────────────────┐                      ┌────────────────┐
│  Gateway (N台)  │                      │  Console (1台)  │
│  S3 HTTP 前端   │                      │  管控面 Web UI  │
│  无状态         │                      │  无状态         │
└──────┬─────────┘                      └──────┬─────────┘
       │ gRPC (配置: Scheduler 地址)             │ gRPC (配置: Metadata 地址)
       ▼                                       ▼
 ┌─────────────────────────────┐    ┌──────────────────────────┐
 │  同一物理节点                  │    │  Metadata 节点 (3/5 台)    │
 │  ┌─────────────────────────┐ │    │  Raft 共识 + RocksDB      │
 │  │  Scheduler Worker       │─┼───►│  元数据 + Worker 注册中心   │
 │  │  业务中枢：调度编排       │ │    └──────────────────────────┘
 │  ├─────────────────────────┤ │               ▲
 │  │  Cache Worker           │ │               │ 心跳
 │  │  SPDK Blobstore 缓存    │ │    ┌──────────┴───────────────┐
 │  └─────────────────────────┘ │    │  Tape Worker (独立物理节点) │
 └─────────────────────────────┘    │  磁带驱动 /dev/nst*        │
                                    │  带库 /dev/sg*              │
                                    └────────────────────────────┘
```

| 节点类型 | 职责 | 数量 | 连接对象 |
|----------|------|------|----------|
| **Metadata** | Raft 共识、元数据存储、Worker 注册中心 | 3/5 | 三类 Worker 心跳；Console 管控 |
| **Scheduler Worker** | **业务中枢**：调度编排、对接全部层（与 Cache 同机） | 1~N | Metadata、Cache Worker、Tape Worker、Gateway |
| **Cache Worker** | SPDK NVMe 数据缓存（与 Scheduler 同机） | 1~N | Scheduler Worker (gRPC)、Metadata (心跳) |
| **Tape Worker** | 磁带驱动管理、数据读写（独立物理节点） | 1~N | Scheduler Worker (gRPC)、Metadata (心跳) |
| **Gateway** | S3 HTTP 接入 + 协议适配（无状态） | N | **仅 Scheduler Worker** |
| **Console** | 管控面 Web UI + Admin API（无状态） | 1 | **仅 Metadata** |

**关键设计决策**：

- **Gateway 不直连 Metadata**：Gateway 是纯粹的 S3 协议前端，所有业务请求（包括元数据查询）
  都通过 Scheduler Worker 代理。Gateway 配置文件中只需写 Scheduler Worker 地址。
- **Scheduler Worker 是唯一业务中枢**：持有 MetadataClient，编排 Cache Worker 和 Tape Worker，
  接受 Gateway 的全部请求。
- **Console 仅连 Metadata**：管控面直接查询/操作元数据集群，管理 Worker 的增删。
- **Scheduler ↔ Cache 使用 gRPC**：虽然同机部署，仍通过 gRPC 跨网络连接，
  保持架构一致性，未来可独立扩展。

### 2.2 逻辑分层架构

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                            S3 接入层 (Axum) — Gateway                        │
│  PUT / GET / HEAD / DELETE / RestoreObject / ListBuckets ...                 │
│                      S3 冷归档协议适配层                                      │
│  StorageClass 映射 | RestoreRequest 解析 | x-amz-restore 响应 | 错误码映射    │
└─────────────────────────────────────────────────────────────────────────────┘
                                        │ gRPC (全部请求)
                                        ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                    归档/取回调度器 — Scheduler Worker（业务中枢）               │
│      接受 Gateway 请求 │ 编排 Cache 和 Tape │ 读写 Metadata                   │
└─────────────────────────────────────────────────────────────────────────────┘
            │                       │                       │
            ▼ gRPC                  ▼ gRPC (同机)            ▼ gRPC (远程)
┌───────────────────────┐  ┌───────────────────────┐  ┌───────────────────────┐
│   元数据集群 (Raft)     │  │  数据缓存层 (SPDK)     │  │  磁带管理层 (Tape)     │
│   Metadata 节点        │  │  Cache Worker         │  │  Tape Worker          │
│   RocksDB 持久化       │  │  用户态 NVMe 缓存     │  │  自研 SDK + SCSI      │
└───────────────────────┘  └───────────────────────┘  └───────────────────────┘
```

### 2.3 交互模式

**Scheduler Worker 是唯一的元数据读写入口**（Console 管控除外）。
Gateway、Cache Worker、Tape Worker 均不直接访问 Metadata。

```
                                Console ──管控读写──▶ Metadata
                                                        ▲
Gateway ──全部请求──▶ Scheduler Worker ──读+写──────────┘
                           │
                           ├──gRPC──▶ Cache Worker   (纯数据存储)
                           └──gRPC──▶ Tape Worker    (纯硬件抽象)
```

| 层 | 与 Metadata 的关系 | 与 Scheduler 的关系 | 说明 |
|------|------|------|------|
| Gateway | **无直接连接** | gRPC 客户端 | 所有请求代理给 Scheduler |
| Scheduler Worker | 读 + 写 | — | 唯一业务中枢 |
| Cache Worker | 仅心跳注册 | gRPC 服务端 | 纯数据存储 |
| Tape Worker | 仅心跳注册 | gRPC 服务端 | 纯硬件抽象 |
| Console | 管控读写 | 无 | Worker 增删、元数据查询 |

---

## 3. S3 冷归档协议兼容设计

### 3.1 协议兼容目标

ColdStore 是**纯冷归档系统**，设计模型与 AWS S3 Glacier Deep Archive 一致：
所有对象写入即排队归档到磁带，不提供在线热存储层。外部热存储系统在需要冷归档时调用 ColdStore 的 PutObject API，迁移决策由外部系统负责。

在协议层面兼容 AWS S3 Glacier 冷归档语义，使现有 S3 客户端、SDK 及业务系统无需改造即可接入。

### 3.2 存储类别映射

ColdStore 仅有两种内部存储状态：

| S3 Storage Class | ColdStore 内部状态 | 说明 |
|------------------|-------------------|------|
| 任意（`GLACIER` / `DEEP_ARCHIVE` / `STANDARD` 等） | `ColdPending` | 写入即排队归档（统一冷存储入口） |
| 归档完成 | `Cold` | 已归档到磁带 |

### 3.3 RestoreObject API 规范

#### 3.3.1 请求格式

```
POST /{Bucket}/{Key}?restore&versionId={VersionId} HTTP/1.1
Host: {endpoint}
Content-Type: application/xml

<RestoreRequest xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Days>integer</Days>
  <GlacierJobParameters>
    <Tier>Expedited|Standard|Bulk</Tier>
  </GlacierJobParameters>
</RestoreRequest>
```

**关键参数：**

| 参数 | 必填 | 说明 |
|------|------|------|
| `Days` | 是 | 解冻数据保留天数，最小 1 天 |
| `GlacierJobParameters.Tier` | 否 | 取回优先级：Expedited / Standard / Bulk |
| `versionId` | 否 | 对象版本，缺省为当前版本 |

#### 3.3.2 取回层级 (Tier) 语义

| Tier | 预期完成时间 | ColdStore 实现策略 |
|------|-------------|-------------------|
| **Expedited** | 1–5 分钟 | 高优先级队列，可配置预留容量 |
| **Standard** | 3–5 小时 | 默认队列，常规调度 |
| **Bulk** | 5–12 小时 | 低优先级，批量合并取回 |

#### 3.3.3 响应规范

| 场景 | HTTP 状态码 | 说明 |
|------|------------|------|
| 首次解冻请求 | `202 Accepted` | 任务已接受，等待处理 |
| 已解冻且未过期 | `200 OK` | 可延长 Days，仅更新过期时间 |
| 解冻进行中 | `409 Conflict` | 错误码 `RestoreAlreadyInProgress` |
| Expedited 容量不足 | `503 Service Unavailable` | 错误码 `GlacierExpeditedRetrievalNotAvailable` |
| 对象尚未归档 | `409 Conflict` | ColdPending 状态对象不可 Restore |

### 3.4 HEAD Object 与 x-amz-restore 响应头

对冷对象执行 HEAD 时，需返回 `x-amz-restore` 头以表示解冻状态：

```
x-amz-restore: ongoing-request="true"
```
或
```
x-amz-restore: ongoing-request="false", expiry-date="Fri, 28 Feb 2025 12:00:00 GMT"
```

### 3.5 GET Object 冷对象访问控制

| 对象状态 | GET 行为 |
|----------|----------|
| ColdPending（排队中） | 返回 `403 InvalidObjectState`，对象尚未归档 |
| Cold + 未解冻 | 返回 `403 InvalidObjectState`，提示需先 Restore |
| Cold + 解冻中 | 返回 `403 InvalidObjectState` |
| Cold + 已解冻 | 从 SPDK 缓存读取并返回 |
| Cold + 解冻已过期 | 返回 `403 InvalidObjectState`，需重新 Restore |

### 3.6 错误码映射

| AWS 错误码 | HTTP 状态 | ColdStore 实现 |
|------------|-----------|----------------|
| `InvalidObjectState` | 403 | 冷对象未解冻时 GET |
| `RestoreAlreadyInProgress` | 409 | 重复 Restore 请求 |
| `GlacierExpeditedRetrievalNotAvailable` | 503 | Expedited 容量不足 |
### 3.7 归档触发模型

ColdStore 是纯冷归档系统（类似 AWS Glacier Deep Archive），所有对象 PutObject 写入即标记为 `ColdPending`，
自动排队等待调度器聚合写入磁带。不支持 Hot/Warm 存储类别和生命周期规则。

外部热存储系统在需要冷归档时，直接调用 ColdStore 的 PutObject API 写入对象。
迁移决策（何时归档）由外部系统负责，ColdStore 不做生命周期管理。

---

## 4. 集群元数据：Raft + RocksDB 设计

### 4.1 设计目标

- **强一致性**：元数据读写通过 Raft 达成共识
- **高可用**：多数节点存活即可服务
- **持久化**：RocksDB 作为 Raft 日志与状态机的存储引擎
- **可扩展**：支持 3/5/7 节点集群

### 4.2 架构图

```
┌─────────────────────────────────────────────────────────────────┐
│                     ColdStore 元数据节点                          │
├─────────────────────────────────────────────────────────────────┤
│  ┌─────────────────────────────────────────────────────────────┐  │
│  │ Raft Core (openraft)                                         │  │
│  │ - databendlabs/openraft                                      │  │
│  │ - 异步事件驱动，Tokio 运行时                                   │  │
│  └─────────────────────────────────────────────────────────────┘  │
│         │                                                         │
│         │  RaftLogStorage / RaftStateMachine                      │
│         ▼                                                         │
│  ┌─────────────────────────────────────────────────────────────┐  │
│  │ openraft-rocksstore (RocksStore)                             │  │
│  │ - Raft 日志 + 状态机持久化                                     │  │
│  │ - RocksDB 单实例，可扩展 CF 存储业务元数据                     │  │
│  └─────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

### 4.3 技术选型（已确定）

| 组件 | 选型 | 说明 |
|------|------|------|
| Raft 实现 | **openraft** | [databendlabs/openraft](https://github.com/databendlabs/openraft)，异步 Raft，Tokio 驱动 |
| Raft 存储 | **openraft-rocksstore** | [crates.io/openraft-rocksstore](https://crates.io/crates/openraft-rocksstore)，RocksDB 后端 |
| 存储引擎 | RocksDB | 通过 openraft-rocksstore 封装 |

### 4.3.1 OpenRaft 集成要点

- **依赖**：`openraft = "0.9"`，`openraft-rocksstore = "0.9"`
- **TypeConfig**：定义 `NodeId`、`Node`、`AppData`、`AppDataResponse` 等类型
- **RocksRequest**：扩展 `AppData`，定义 `PutObject`、`DeleteObject`、`PutRecallTask` 等命令
- **RocksStateMachine**：实现 `apply` 逻辑，将命令写入 RocksDB 业务 CF
- **线性读**：`raft.ensure_linearizable().await` 保证读到已提交状态

### 4.4 RocksDB 数据模型

#### 4.4.1 Column Family 设计

| CF 名称 | Key 格式 | Value 格式 | 说明 |
|---------|----------|-----------|------|
| `objects` | `obj:{bucket}:{key}` | ObjectMetadata (JSON/Protobuf) | 对象元数据 |
| `object_versions` | `objv:{bucket}:{key}:{version}` | ObjectMetadata | 多版本对象 |
| `bundles` | `bundle:{uuid}` | ArchiveBundle | 归档包 |
| `tapes` | `tape:{tape_id}` | TapeInfo | 磁带信息 |
| `recall_tasks` | `recall:{uuid}` | RecallTask | 取回任务 |
| `archive_tasks` | `archive:{uuid}` | ArchiveTask | 归档任务 |
| `index_bundle_objects` | `idx_b:{archive_id}:{bucket}:{key}` | 空 | 归档包→对象反向索引 |
| `index_tape_bundles` | `idx_t:{tape_id}:{bundle_id}` | 空 | 磁带→归档包索引 |

#### 4.4.2 Raft 日志存储 (raftdb)

- **Key**: `{log_index}` (8 bytes)
- **Value**: Raft 日志条目序列化
- **用途**: 仅存储 Raft 共识日志，与业务 KV 分离

### 4.5 Raft 状态机命令（AppData / RocksRequest 扩展）

参考 openraft-rocksstore 的 `RocksRequest` 模式，扩展 ColdStore 专用命令：

```rust
// 实现 openraft::AppData
#[derive(Clone, Debug, Serialize, Deserialize)]
enum ColdStoreRequest {
    PutObject(ObjectMetadata),
    DeleteObject { bucket: String, key: String },
    UpdateStorageClass { bucket: String, key: String, class: StorageClass },
    PutArchiveBundle(ArchiveBundle),
    PutTape(TapeInfo),
    PutRecallTask(RecallTask),
    UpdateRecallTask { id: Uuid, status: RestoreStatus },
    PutArchiveTask(ArchiveTask),
}
```

可基于 `openraft-rocksstore` 的 `RocksStore` 进行扩展，或实现自定义 `RaftStorage` 以支持上述业务命令。

### 4.6 读写路径

| 操作类型 | 路径 |
|----------|------|
| **写** | Client → Leader → Raft Propose → 多数派 Append → Apply → RocksDB Write |
| **读** | Client → 任意节点 → 本地 RocksDB Read（需保证 ReadYourWrites 时可选走 Leader） |
| **线性读** | 通过 Raft 的 ReadIndex 保证读到已提交状态 |

### 4.7 集群部署

- **推荐配置**：3 或 5 节点
- **Leader 选举**：Raft 自动选举
- **网络**：节点间 gRPC 或自定义 RPC
- **持久化路径**：`/var/lib/coldstore/metadata/{raftdb|kvdb}`

### 4.8 与现有代码集成

- 新增 `MetadataBackend::RaftRocksDB` 枚举变体
- `MetadataService` 通过 Raft 客户端代理写请求
- 读请求可直接访问本地 RocksDB（Follower 只读）

---

## 5. 数据缓存层：async-spdk 设计

### 5.1 设计目标

- **高性能**：用户态 I/O，绕过内核，降低延迟
- **高吞吐**：Poll 模式、零拷贝，充分发挥 NVMe 性能
- **解冻数据缓存**：承载从磁带取回的数据，供 GET 快速响应
- **原生 async/await**：与 Tokio 无缝集成，无需回调桥接

### 5.2 技术选型：async-spdk + SPDK Blob

采用 [madsys-dev/async-spdk](https://github.com/madsys-dev/async-spdk)：

- **异步 Rust 绑定**：原生 `async`/`await`，基于 Tokio
- **存储方式**：**SPDK Blobstore (Blob)**，非 raw bdev；Blobstore 构建于 bdev 之上
- **优势**：Blob 提供持久化块分配、xattrs 元数据、thin provisioning，适合对象缓存

### 5.3 架构概览

```
┌─────────────────────────────────────────────────────────────────┐
│                    ColdStore 缓存服务进程                         │
├─────────────────────────────────────────────────────────────────┤
│  ┌─────────────────────────────────────────────────────────┐   │
│  │  Cache Manager (Rust)                                    │   │
│  │  - cache_key → blob_id 索引                              │   │
│  │  - LRU/LFU/TTL 淘汰策略                                   │   │
│  │  - 容量与并发控制                                         │   │
│  └─────────────────────────────────────────────────────────┘   │
│                              │                                   │
│                              ▼                                   │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │  async-spdk (SPDK Blobstore / Blob API)                   │   │
│  │  - 1 对象 = 1 Blob，xattrs 存 bucket/key/expire_at        │   │
│  │  - blob_write / blob_read 按 page 读写                    │   │
│  │  - 构建于 bdev 之上                                       │   │
│  └─────────────────────────────────────────────────────────┘   │
│                              │                                   │
│                              ▼                                   │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │  SPDK Bdev (Blobstore 底层)                               │   │
│  │  - Malloc0: 测试用内存 bdev                               │   │
│  │  - Nvme0n1: 生产 NVMe 设备                               │   │
│  └─────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
```

### 5.4 集成方式与启动模型

async-spdk 采用 **SPDK 事件循环 + Tokio** 的混合模型：

```rust
use async_spdk::{event::AppOpts, bdev::*, env};

fn main() {
    event::AppOpts::new()
        .name("coldstore_cache")
        .config_file(&std::env::args().nth(1).expect("config file"))
        .block_on(async_main())
        .unwrap();
}

async fn async_main() -> Result<()> {
    // Blobstore 方式：先加载/初始化 Blobstore，再通过 Blob API 读写
    // let bs = Blobstore::load("Malloc0").await?;
    // let blob_id = bs.create_blob().await?;
    // bs.blob_write(blob_id, offset_pages, buf).await?;
    // bs.blob_read(blob_id, offset_pages, buf).await?;
    app_stop();
    Ok(())
}
```

**注意**：ColdStore 主进程若已使用 Tokio（Axum），需将 async-spdk 作为**独立子模块**或**专用线程**运行，因 SPDK 需独占 reactor。可选方案：

- **方案 A**：缓存服务作为独立二进制，通过 gRPC/HTTP 与主服务通信
- **方案 B**：主进程在 SPDK `block_on` 内启动 Axum（需调整启动顺序）
- **方案 C**：缓存层先使用文件后端，待架构稳定后再接入 async-spdk

### 5.5 SPDK JSON 配置示例

**测试用 Malloc bdev**（`cache_spdk.json`）：

```json
{
  "subsystems": [
    {
      "subsystem": "bdev",
      "config": [
        {
          "method": "bdev_malloc_create",
          "params": {
            "name": "Malloc0",
            "num_blocks": 32768,
            "block_size": 512
          }
        }
      ]
    }
  ]
}
```

**生产 NVMe**：根据 [async-spdk README](https://github.com/madsys-dev/async-spdk)，需配置对应 bdev 名称（如 `Nvme0n1`），并确保：

- 以 root 运行
- 大页内存：`echo "1024" > /sys/kernel/mm/hugepages/hugepages-2048kB/nr_hugepages`
- SPDK 编译：`./configure --without-isal` 可避免部分 isa-l 错误

### 5.6 缓存数据布局（SPDK Blob 方式）

| 层级 | 存储介质 | 用途 |
|------|----------|------|
| L1 | NVMe SSD (SPDK Blobstore on bdev) | 热解冻数据，低延迟 |
| L2 (可选) | 文件系统 | 兼容旧实现，温数据 |

对象与 Blob 的对应：

- **1 对象 = 1 Blob**：每个解冻对象对应一个 Blobstore Blob
- **元数据**：Blob xattrs 存储 bucket、key、size、checksum、expire_at
- **索引**：自维护 `cache_key → blob_id` 映射（Blobstore 不提供按名查找）
- **读写单位**：Page（4KB），Cluster（1MB）为分配单元

### 5.7 缓存策略

| 策略 | 说明 |
|------|------|
| **LRU** | 淘汰最久未访问对象 |
| **LFU** | 淘汰访问频率最低对象 |
| **TTL** | 按解冻过期时间淘汰 |
| **容量** | 总容量上限，超限触发淘汰 |

### 5.8 配置项

```yaml
cache:
  backend: "spdk"  # spdk | file (兼容旧实现)
  spdk:
    enabled: true
    config_file: "/etc/coldstore/cache_spdk.json"
    bdev_name: "Malloc0"     # 测试用; 生产改为 "Nvme0n1"
    max_size_gb: 100
    block_size: 4096
  ttl_secs: 86400
  eviction_policy: "Lru"
```

### 5.9 Cargo 依赖

```toml
[dependencies]
async-spdk = { git = "https://github.com/madsys-dev/async-spdk" }
# 或发布到 crates.io 后: async-spdk = "0.1"
```

---

## 6. 磁带管理层：自研 SDK 抽象层

### 6.1 设计目标

- **自研 SDK 抽象层**：不依赖厂商闭源 SDK，统一磁带、驱动、库的访问接口
- **前期对接 Linux SCSI**：基于 Linux 内核 SCSI 磁带驱动（st/ sg）实现
- **可扩展**：后期可替换为厂商 SDK、LTFS 等实现，无需改动上层调度逻辑

### 6.2 架构分层

```
┌─────────────────────────────────────────────────────────────────┐
│  TapeManager (调度层)                                             │
│  - 磁带库抽象、驱动调度、归档包聚合、取回合并                        │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│  Tape SDK 抽象层 (自研)                                           │
│  - TapeDrive trait: read, write, seek, status, eject             │
│  - TapeLibrary trait: list_slots, load, unload, inventory        │
│  - 与具体实现解耦                                                  │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│  实现层 (前期)                                                    │
│  Linux SCSI: st 驱动 (/dev/nst0) + MTIO ioctl                     │
│  - 顺序读写、定位、状态查询                                         │
└─────────────────────────────────────────────────────────────────┘
```

### 6.3 前期实现：Linux SCSI 协议

#### 6.3.1 设备与接口

| 接口 | 设备节点 | 说明 |
|------|----------|------|
| **st 驱动** | `/dev/nst0` | 非自动回卷，推荐用于顺序读写（避免每次操作回卷） |
| **st 驱动** | `/dev/st0` | 自动回卷，操作后回卷到 BOT |
| **SCSI Generic** | `/dev/sg*` | 透传 SCSI 命令，用于高级控制（可选） |

#### 6.3.2 MTIO 控制接口

基于 `<sys/mtio.h>`，主要 ioctl：

| ioctl | 用途 |
|-------|------|
| `MTIOCTOP` | 执行磁带操作：`MTFSF`(前进文件)、`MTBSF`(后退文件)、`MTFSR`(前进记录)、`MTBSR`(后退记录)、`MTREW`(回卷)、`MTEOM`(到卷尾) |
| `MTIOCGET` | 获取驱动状态 |
| `MTIOCPOS` | 获取当前磁带位置 |

#### 6.3.3 数据读写

- **块模式**：可变块或固定块（通过 ioctl 设置）
- **读写**：标准 `read()` / `write()` 系统调用
- **顺序性**：磁带必须顺序写入，随机读需 seek 后再读

#### 6.3.4 依赖与工具

- `mt-st`：mt 命令，用于控制磁带（可选，SDK 自实现 ioctl）
- 内核：`st` 模块（`modprobe st`）
- 权限：设备节点需可读写（通常 root 或 tape 组）

### 6.4 SDK 抽象接口定义

```rust
/// 磁带驱动抽象
pub trait TapeDrive: Send + Sync {
    fn drive_id(&self) -> &str;
    async fn read(&self, offset: u64, length: u64) -> Result<Vec<u8>>;
    async fn write(&self, data: &[u8]) -> Result<()>;
    async fn seek(&self, position: u64) -> Result<()>;
    async fn status(&self) -> Result<DriveStatus>;
    async fn rewind(&self) -> Result<()>;
    async fn eject(&self) -> Result<()>;
}

/// 磁带库抽象（带机械臂的库，后期扩展）
pub trait TapeLibrary: Send + Sync {
    async fn list_slots(&self) -> Result<Vec<SlotInfo>>;
    async fn load(&self, slot_id: &str, drive_id: &str) -> Result<()>;
    async fn unload(&self, drive_id: &str) -> Result<()>;
    async fn inventory(&self) -> Result<Vec<TapeInfo>>;
}

/// Linux SCSI 实现（前期）
pub struct ScsiTapeDrive {
    device_path: PathBuf,  // e.g. /dev/nst0
    // ...
}
impl TapeDrive for ScsiTapeDrive { ... }
```

### 6.5 实现路径

| 阶段 | 实现 | 说明 |
|------|------|------|
| **前期** | `ScsiTapeDrive` | 基于 st 驱动 + MTIO，单驱动直连 |
| **中期** | `ScsiTapeLibrary` | 带库场景，通过 sg 或厂商 MMC 协议控制机械臂 |
| **后期** | `VendorTapeDrive` | 可选对接厂商 SDK（如 IBM、HP 等） |

### 6.6 配置项

```yaml
tape:
  sdk:
    backend: "scsi"  # scsi | vendor (后期)
  scsi:
    device: "/dev/nst0"
    block_size: 262144   # 256KB，LTO 常用
    buffer_size_mb: 64
  library_path: null     # 带库场景时配置
  supported_formats:
    - "LTO-9"
    - "LTO-10"
```

### 6.7 模块结构

```
src/tape/
├── mod.rs
├── sdk/                    # 自研 SDK 抽象层
│   ├── mod.rs
│   ├── drive.rs            # TapeDrive trait
│   ├── library.rs          # TapeLibrary trait (可选)
│   └── types.rs            # DriveStatus, SlotInfo 等
├── scsi/                   # Linux SCSI 实现
│   ├── mod.rs
│   ├── drive.rs            # ScsiTapeDrive
│   └── mtio.rs             # MTIO ioctl 封装
├── manager.rs              # TapeManager
└── driver.rs               # 兼容旧接口，委托给 sdk
```

---

## 7. 模块交互与数据流

### 7.1 Restore 完整流程

```
1. Client: POST /bucket/key?restore + RestoreRequest
2. S3 Handler: 解析 Days/Tier，校验对象为 Cold
3. Metadata (Raft): 检查是否已有进行中 Restore，写入 RecallTask
4. Recall Scheduler: 按 archive_id 合并任务，调度磁带读取
5. Tape: 顺序读取 → 数据块
6. Cache (SPDK): 写入 SPDK bdev，更新索引
7. Metadata (Raft): 更新 ObjectMetadata.restore_status=Completed, restore_expire_at
8. Client: HEAD 可见 x-amz-restore: expiry-date="..."
9. Client: GET 从 SPDK 缓存返回数据
```

### 7.2 PutObject 完整流程

```
1. Gateway: 接收 S3 PutObject 请求
2. Scheduler Worker: 将数据暂存到 Cache Worker (gRPC: PutStaging)
3. Scheduler Worker: 写入 Metadata (Raft: PutObject, storage_class=ColdPending, staging_id)
4. Gateway: 返回 200 OK
```

### 7.3 Archive 完整流程

```
1. Archive Scheduler: 扫描 ColdPending 对象，获取 staging_id
2. Archive Scheduler: 从 Cache Worker 暂存区读取数据 (gRPC: GetStaging)
3. Archive Scheduler: 按策略聚合为 ArchiveBundle
4. Tape: 顺序写入磁带
5. Metadata (Raft): 写入 ArchiveBundle, 更新 ObjectMetadata (Cold, archive_id, tape_id, 清空 staging_id)
6. Cache Worker: 删除暂存数据 (gRPC: DeleteStaging)
```

---

## 8. 非功能性设计

### 8.1 高可用

- 元数据：Raft 3/5 节点，多数派存活即可
- 缓存：单点故障时，解冻数据需重新从磁带取回（可接受）
- S3 接入：无状态，可多实例 + 负载均衡

### 8.2 可观测性

- 日志：tracing + structured logging
- 指标：归档/取回队列长度、缓存命中率、Raft 提交延迟
- 追踪：关键路径分布式 trace

### 8.3 安全

- S3 签名 v4 认证
- 传输加密 TLS
- 元数据集群节点间 mTLS（可选）

---

## 9. 实施路线图

| 阶段 | 内容 |
|------|------|
| **Phase 1** | S3 RestoreObject 协议完整实现，错误码与 x-amz-restore 头 |
| **Phase 2** | Raft + RocksDB 元数据集群，替换 Postgres/Etcd |
| **Phase 3** | SPDK 缓存层集成，替换文件系统缓存 |
| **Phase 4** | 磁带 SDK 抽象层 + Linux SCSI 实现（ScsiTapeDrive） |
| **Phase 5** | 性能调优、生产就绪 |

---

## 10. Cargo 依赖汇总

```toml
[dependencies]
# 元数据集群
openraft = "0.9"
openraft-rocksstore = "0.9"

# 数据缓存层
async-spdk = { git = "https://github.com/madsys-dev/async-spdk" }

# 现有依赖保持不变
tokio = { version = "1.35", features = ["full"] }
axum = "0.7"
rocksdb = "0.22"  # openraft-rocksstore 会引入，可按需显式指定版本
```

---

## 11. 参考资料

- [AWS S3 RestoreObject API](https://docs.aws.amazon.com/AmazonS3/latest/API/API_RestoreObject.html)
- [S3 Glacier Retrieval Options](https://docs.aws.amazon.com/AmazonS3/latest/userguide/restoring-objects-retrieval-options.html)
- [OpenRaft](https://github.com/databendlabs/openraft) - databendlabs/openraft
- [OpenRaft RocksStore](https://crates.io/crates/openraft-rocksstore)
- [async-spdk](https://github.com/madsys-dev/async-spdk) - madsys-dev/async-spdk
- [SPDK Documentation](https://spdk.io/doc/)
- [Linux st(4) - SCSI tape device](https://man7.org/linux/man-pages/man4/st.4.html)
- [Linux SCSI tape driver (kernel.org)](https://kernel.org/doc/Documentation/scsi/st.txt)
