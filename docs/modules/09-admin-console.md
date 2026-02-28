# 管控面（Admin Console）设计

> 所属架构：ColdStore 冷存储系统  
> 参考：[总架构设计](../DESIGN.md) | [03-元数据集群](./03-metadata-cluster.md) | [06-磁带管理层](./06-tape-layer.md) | [08-可观测性](./08-observability.md)

## 1. 设计目标

管控面为运维人员和管理员提供 Web UI 和 REST API，覆盖 ColdStore 冷归档存储系统的全面运维管理。

### 1.1 核心功能域

| 功能域 | 说明 | 对应业务层 |
|--------|------|-----------|
| 集群管理 | 三类 Worker 添加/下线/排空、Metadata 状态、Raft 监控 | 元数据层 (03) |
| 桶与对象管理 | 桶 CRUD、对象元数据查询、存储类别统计 | 元数据层 (03) |
| 归档任务管理 | ArchiveTask 列表、进度、失败重试 | 调度层 (05) |
| 取回任务管理 | RecallTask 列表、状态追踪、优先级调整 | 调度层 (05) |
| 磁带介质管理 | 磁带注册、容量、状态、离线/上线、退役 | 磁带层 (06) |
| 驱动管理 | 驱动状态、利用率、分配情况 | 磁带层 (06) |
| 缓存监控 | 容量、命中率、淘汰统计 | 缓存层 (04) |
| 系统监控 | 仪表板、指标、告警、审计日志 | 可观测性 (08) |

### 1.2 在架构中的位置

```
┌─────────────────────────────────────────────────────────────────┐
│                     浏览器 (Admin Console)                        │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │  React 18 + TypeScript + Ant Design 5                     │  │
│  └──────────────────────────┬────────────────────────────────┘  │
└─────────────────────────────┼───────────────────────────────────┘
                              │ HTTP/JSON
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│              ColdStore Admin API (Axum :8080)                     │
│  独立于 S3 API (:9000)，共享同一进程内的服务层                      │
│  ┌──────────┬───────────┬──────────┬──────────┬───────────┐    │
│  │ /cluster │ /buckets  │ /objects │ /tapes   │ /tasks    │    │
│  │ /drives  │ /cache    │ /metrics │ /audit   │ /config   │    │
│  └──────────┴───────────┴──────────┴──────────┴───────────┘    │
│              │                                                   │
│              ▼                                                   │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  MetadataService │ TapeManager │ CacheManager │ Metrics  │  │
│  └──────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

---

## 2. 技术选型

### 2.1 后端

| 组件 | 选型 | 说明 |
|------|------|------|
| HTTP 框架 | **Axum** | 与 S3 接入层共用框架，独立 Router 和端口 |
| 序列化 | **serde + serde_json** | JSON 请求/响应 |
| 认证 | **JWT** (jsonwebtoken crate) | Bearer Token 认证 |
| API 文档 | **utoipa + Swagger UI** | OpenAPI 3.0 自动生成 |
| WebSocket | **axum::extract::ws** | 实时推送任务状态、告警事件 |

### 2.2 前端

| 组件 | 选型 | 说明 |
|------|------|------|
| 框架 | **React 18 + TypeScript** | Hooks 函数组件、类型安全 |
| UI 库 | **Ant Design 5** | 企业级管控面组件（Table、Form、Chart） |
| 图表 | **ECharts** (`echarts-for-react`) | 仪表板图表 |
| HTTP | **Axios** | API 调用 |
| 状态管理 | **Zustand** | 轻量全局状态 |
| 路由 | **React Router 6** | SPA 路由 |
| 构建 | **Vite** (`@vitejs/plugin-react`) | 快速构建 |

---

## 3. 后端 Admin API 设计

### 3.1 API 总览

所有 API 以 `/api/v1/admin` 为前缀，返回统一 JSON 格式。

```rust
pub struct ApiResponse<T: Serialize> {
    pub code: u32,         // 0 = 成功，非 0 = 错误码
    pub message: String,
    pub data: Option<T>,
}

pub struct PageRequest {
    pub page: u32,         // 从 1 开始
    pub page_size: u32,    // 默认 20，最大 100
    pub sort_by: Option<String>,
    pub sort_order: Option<String>,  // "asc" / "desc"
}

pub struct PageResponse<T: Serialize> {
    pub items: Vec<T>,
    pub total: u64,
    pub page: u32,
    pub page_size: u32,
}
```

### 3.2 集群管理 API

#### 3.2.1 集群概览

| 方法 | 路径 | 说明 | 权限 |
|------|------|------|------|
| GET | `/cluster/info` | 集群概览（全部节点信息、Raft 状态） | read |
| GET | `/cluster/health` | 健康检查（Raft 状态、各类 Worker 在线率） | read |
| GET | `/cluster/raft/status` | Raft 详细状态（log index、commit index、snapshot） | read |

#### 3.2.2 Scheduler Worker 管理

| 方法 | 路径 | 说明 | 权限 |
|------|------|------|------|
| GET | `/cluster/scheduler-workers` | 调度 Worker 列表 | read |
| GET | `/cluster/scheduler-workers/{id}` | 调度 Worker 详情（队列深度、活跃作业等） | read |
| POST | `/cluster/scheduler-workers` | **添加**调度 Worker（注册到 Metadata） | admin |
| DELETE | `/cluster/scheduler-workers/{id}` | **下线**调度 Worker（从 Metadata 移除） | admin |
| POST | `/cluster/scheduler-workers/{id}/drain` | 排空（不再分配新任务，等待完成） | admin |

#### 3.2.3 Cache Worker 管理

| 方法 | 路径 | 说明 | 权限 |
|------|------|------|------|
| GET | `/cluster/cache-workers` | 缓存 Worker 列表 | read |
| GET | `/cluster/cache-workers/{id}` | 缓存 Worker 详情（容量、对象数等） | read |
| POST | `/cluster/cache-workers` | **添加**缓存 Worker | admin |
| DELETE | `/cluster/cache-workers/{id}` | **下线**缓存 Worker | admin |
| POST | `/cluster/cache-workers/{id}/drain` | 排空 | admin |

#### 3.2.4 Tape Worker 管理

| 方法 | 路径 | 说明 | 权限 |
|------|------|------|------|
| GET | `/cluster/tape-workers` | 磁带 Worker 列表 | read |
| GET | `/cluster/tape-workers/{id}` | 磁带 Worker 详情（驱动列表、带库状态） | read |
| POST | `/cluster/tape-workers` | **添加**磁带 Worker | admin |
| DELETE | `/cluster/tape-workers/{id}` | **下线**磁带 Worker | admin |
| POST | `/cluster/tape-workers/{id}/drain` | 排空 | admin |

> Gateway / Console 为无状态节点，不注册到集群，不出现在管控面中。
> 三类 Worker 的添加和下线都通过 Console 操作，实际写入 Metadata 集群。

**响应示例**：

```json
// GET /cluster/info
{
  "code": 0,
  "message": "ok",
  "data": {
    "cluster_id": "coldstore-prod-01",
    "metadata_nodes": [
      { "node_id": 1, "addr": "10.0.1.1:21001", "raft_role": "Leader", "status": "Online" },
      { "node_id": 2, "addr": "10.0.1.2:21001", "raft_role": "Follower", "status": "Online" },
      { "node_id": 3, "addr": "10.0.1.3:21001", "raft_role": "Follower", "status": "Online" }
    ],
    "scheduler_workers": [
      {
        "node_id": 10, "addr": "10.0.2.1:22001", "status": "Online",
        "is_active": true, "pending_archive": 42, "pending_recall": 5, "active_jobs": 3,
        "paired_cache_worker_id": 20
      }
    ],
    "cache_workers": [
      {
        "node_id": 20, "addr": "10.0.2.1:22002", "status": "Online",
        "bdev_name": "NVMe0n1", "total_capacity": "2TB", "used_capacity": "800GB", "blob_count": 15000
      }
    ],
    "tape_workers": [
      {
        "node_id": 30, "addr": "10.0.3.1:23001", "status": "Online",
        "drives": [
          { "drive_id": "drive0", "device": "/dev/nst0", "status": "InUse", "current_tape": "TAPE001" },
          { "drive_id": "drive1", "device": "/dev/nst1", "status": "Idle", "current_tape": null }
        ],
        "library": { "device": "/dev/sg5", "slots": 24, "drives": 2 }
      }
    ],
    "leader_id": 1,
    "term": 42,
    "committed_index": 1583920
  }
}
```

### 3.3 桶管理 API

| 方法 | 路径 | 说明 | 权限 |
|------|------|------|------|
| GET | `/buckets` | 桶列表（含对象数、容量） | read |
| GET | `/buckets/{name}` | 桶详情 | read |
| POST | `/buckets` | 创建桶 | admin |
| DELETE | `/buckets/{name}` | 删除桶 | admin |
| GET | `/buckets/{name}/stats` | 桶统计（各 StorageClass 对象数/大小） | read |

**桶统计响应**：

```json
// GET /buckets/my-bucket/stats
{
  "code": 0,
  "data": {
    "name": "my-bucket",
    "total_objects": 125000,
    "total_size_bytes": 5368709120,
    "by_storage_class": {
      "ColdPending": { "count": 500, "size_bytes": 214748364 },
      "Cold": { "count": 114500, "size_bytes": 4080218932 }
    },
    "restore_in_progress": 12,
    "restored_available": 45
  }
}
```

### 3.4 对象元数据查询 API

| 方法 | 路径 | 说明 | 权限 |
|------|------|------|------|
| GET | `/objects?bucket=&prefix=&storage_class=&page=&page_size=` | 对象列表（支持筛选） | read |
| GET | `/objects/{bucket}/{key}` | 对象详细元数据 | read |
| GET | `/objects/{bucket}/{key}/versions` | 对象版本列表 | read |
| GET | `/objects/{bucket}/{key}/archive` | 对象归档信息（Bundle、磁带位置） | read |
| GET | `/objects/{bucket}/{key}/restore` | 对象取回状态与历史 | read |

**筛选参数**：

| 参数 | 类型 | 说明 |
|------|------|------|
| `bucket` | string | 必选，桶名 |
| `prefix` | string | 可选，key 前缀过滤 |
| `storage_class` | string | 可选，`ColdPending`/`Cold` |
| `restore_status` | string | 可选，`Pending`/`InProgress`/`Completed`/`Failed` |
| `page` | u32 | 页码 |
| `page_size` | u32 | 每页大小 |

**对象详情响应**：

```json
// GET /objects/my-bucket/data/file.bin
{
  "code": 0,
  "data": {
    "bucket": "my-bucket",
    "key": "data/file.bin",
    "version_id": null,
    "size": 104857600,
    "checksum": "a1b2c3d4...",
    "content_type": "application/octet-stream",
    "etag": "\"9e107d9d372bb6826bd81d3542a419d6\"",
    "storage_class": "Cold",
    "archive_info": {
      "archive_id": "f47ac10b-58cc-4372-a567-0e02b2c3d479",
      "tape_id": "TAPE001",
      "tape_set": ["TAPE001", "TAPE002"],
      "tape_block_offset": 524288,
      "bundle_status": "Completed",
      "archived_at": "2026-02-20T08:30:00Z"
    },
    "restore_info": {
      "status": "Completed",
      "expire_at": "2026-03-06T08:30:00Z",
      "tier": "Standard",
      "recall_task_id": "c1d2e3f4-..."
    },
    "created_at": "2026-02-15T10:00:00Z",
    "updated_at": "2026-02-20T08:30:00Z"
  }
}
```

### 3.5 归档任务 API

| 方法 | 路径 | 说明 | 权限 |
|------|------|------|------|
| GET | `/tasks/archive?status=&page=&page_size=` | 归档任务列表 | read |
| GET | `/tasks/archive/{id}` | 归档任务详情（含 Bundle 对象列表） | read |
| POST | `/tasks/archive/{id}/retry` | 重试失败的归档任务 | admin |
| POST | `/tasks/archive/{id}/cancel` | 取消 Pending 的归档任务 | admin |
| GET | `/tasks/archive/stats` | 归档统计（完成数、失败数、吞吐、趋势） | read |

**归档任务列表响应**：

```json
{
  "code": 0,
  "data": {
    "items": [
      {
        "id": "a1b2c3d4-...",
        "bundle_id": "f47ac10b-...",
        "tape_id": "TAPE001",
        "drive_id": "drive-0",
        "object_count": 500,
        "total_size": 5368709120,
        "bytes_written": 3221225472,
        "progress_percent": 60.0,
        "status": "InProgress",
        "retry_count": 0,
        "created_at": "2026-02-27T09:00:00Z",
        "started_at": "2026-02-27T09:01:30Z"
      }
    ],
    "total": 42,
    "page": 1,
    "page_size": 20
  }
}
```

### 3.6 取回任务 API

| 方法 | 路径 | 说明 | 权限 |
|------|------|------|------|
| GET | `/tasks/recall?status=&tier=&tape_id=&page=&page_size=` | 取回任务列表 | read |
| GET | `/tasks/recall/{id}` | 取回任务详情 | read |
| POST | `/tasks/recall/{id}/retry` | 重试失败的取回任务 | admin |
| POST | `/tasks/recall/{id}/cancel` | 取消 Pending 的取回任务 | admin |
| PUT | `/tasks/recall/{id}/priority` | 调整任务优先级 | admin |
| GET | `/tasks/recall/stats` | 取回统计（SLA 达成率、队列深度、趋势） | read |
| GET | `/tasks/recall/queue` | 当前队列状态（三级队列深度、合并情况） | read |

**取回队列状态响应**：

```json
// GET /tasks/recall/queue
{
  "code": 0,
  "data": {
    "expedited": { "depth": 2, "oldest_wait_secs": 45 },
    "standard": { "depth": 35, "oldest_wait_secs": 1800 },
    "bulk": { "depth": 150, "oldest_wait_secs": 7200 },
    "waiting_for_media": 3,
    "active_jobs": [
      { "job_id": "...", "tape_id": "TAPE003", "drive_id": "drive-1", "objects": 8, "progress_percent": 37.5 }
    ]
  }
}
```

### 3.7 磁带介质管理 API

| 方法 | 路径 | 说明 | 权限 |
|------|------|------|------|
| GET | `/tapes?status=&format=&page=&page_size=` | 磁带列表 | read |
| GET | `/tapes/{tape_id}` | 磁带详情（含 Bundle 列表、容量、错误） | read |
| POST | `/tapes` | 注册新磁带 | admin |
| PUT | `/tapes/{tape_id}/status` | 更新磁带状态（Online/Offline/Retired） | admin |
| GET | `/tapes/{tape_id}/bundles` | 磁带上的 Bundle 列表 | read |
| POST | `/tapes/{tape_id}/verify` | 触发磁带校验 | admin |
| GET | `/tapes/{tape_id}/verify/history` | 校验历史 | read |
| POST | `/tapes/{tape_id}/retire` | 磁带退役（触发数据迁移） | admin |
| GET | `/tapes/offline-requests` | 当前离线请求列表 | read |
| POST | `/tapes/offline-requests/{id}/confirm` | 确认磁带已上线 | admin |
| GET | `/tapes/stats` | 磁带总体统计（容量、使用率、健康） | read |

**磁带详情响应**：

```json
// GET /tapes/TAPE001
{
  "code": 0,
  "data": {
    "id": "TAPE001",
    "barcode": "TAPE001L9",
    "format": "LTO-9",
    "status": "Online",
    "location": "Library-1 / Slot-5",
    "capacity": {
      "total_bytes": 18000000000000,
      "used_bytes": 12600000000000,
      "remaining_bytes": 5400000000000,
      "usage_percent": 70.0
    },
    "bundle_count": 245,
    "last_write_at": "2026-02-27T08:15:00Z",
    "last_read_at": "2026-02-27T09:30:00Z",
    "last_verified_at": "2026-02-25T02:00:00Z",
    "error_count": 0,
    "registered_at": "2025-06-01T00:00:00Z"
  }
}
```

### 3.8 驱动管理 API

| 方法 | 路径 | 说明 | 权限 |
|------|------|------|------|
| GET | `/drives` | 驱动列表（状态、当前磁带、利用率） | read |
| GET | `/drives/{drive_id}` | 驱动详情 | read |
| POST | `/drives/{drive_id}/eject` | 弹出磁带（紧急操作） | admin |
| POST | `/drives/{drive_id}/clean` | 触发清洁 | admin |
| GET | `/drives/stats` | 驱动总体统计 | read |

**驱动列表响应**：

```json
// GET /drives
{
  "code": 0,
  "data": [
    {
      "drive_id": "drive-0",
      "device": "/dev/nst0",
      "status": "InUse",
      "loaded_tape": "TAPE001",
      "current_task": { "type": "archive", "id": "a1b2c3d4-..." },
      "utilization_1h": 0.85,
      "total_bytes_written": 1073741824000,
      "total_bytes_read": 536870912000,
      "error_count": 0
    },
    {
      "drive_id": "drive-1",
      "device": "/dev/nst1",
      "status": "Available",
      "loaded_tape": null,
      "current_task": null,
      "utilization_1h": 0.12,
      "total_bytes_written": 536870912000,
      "total_bytes_read": 268435456000,
      "error_count": 1
    }
  ]
}
```

### 3.9 缓存监控 API

| 方法 | 路径 | 说明 | 权限 |
|------|------|------|------|
| GET | `/cache/stats` | 缓存统计（容量、命中率、淘汰） | read |
| GET | `/cache/objects?bucket=&prefix=&page=&page_size=` | 缓存中的对象列表 | read |
| DELETE | `/cache/objects/{bucket}/{key}` | 手动清除缓存对象 | admin |
| POST | `/cache/evict` | 手动触发淘汰 | admin |
| GET | `/cache/config` | 缓存配置 | read |

**缓存统计响应**：

```json
// GET /cache/stats
{
  "code": 0,
  "data": {
    "capacity": {
      "max_bytes": 1099511627776,
      "used_bytes": 824633720832,
      "usage_percent": 75.0,
      "object_count": 8432
    },
    "performance": {
      "hit_count": 1250000,
      "miss_count": 125000,
      "hit_rate": 0.909,
      "put_count": 45000,
      "evict_count": 12000,
      "avg_get_latency_us": 85,
      "avg_put_latency_us": 320
    },
    "eviction": {
      "policy": "Lru",
      "ttl_evictions": 8000,
      "capacity_evictions": 4000,
      "last_eviction_at": "2026-02-27T10:05:00Z"
    },
    "spdk": {
      "bdev": "NVMe0n1",
      "blobstore_clusters": 1048576,
      "used_clusters": 786432,
      "io_unit_size": 4096
    }
  }
}
```

### 3.10 系统监控 API

| 方法 | 路径 | 说明 | 权限 |
|------|------|------|------|
| GET | `/metrics/dashboard` | 仪表板聚合数据 | read |
| GET | `/metrics/prometheus` | Prometheus 格式指标（拉取端点） | read |
| GET | `/alerts/active` | 当前活跃告警 | read |
| GET | `/alerts/history?page=&page_size=` | 告警历史 | read |
| POST | `/alerts/{id}/acknowledge` | 确认告警 | admin |
| GET | `/compensation/pending` | 补偿任务列表 | read |
| POST | `/compensation/{id}/retry` | 手动触发补偿重试 | admin |

### 3.11 审计日志 API

| 方法 | 路径 | 说明 | 权限 |
|------|------|------|------|
| GET | `/audit?action=&user=&start=&end=&page=` | 审计日志查询 | read |

**审计日志结构**：

```rust
pub struct AuditEntry {
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub user: String,
    pub action: String,
    pub resource_type: String,
    pub resource_id: String,
    pub detail: serde_json::Value,
    pub source_ip: String,
    pub result: AuditResult,
}

pub enum AuditResult { Success, Failed(String) }
```

### 3.12 配置管理 API

| 方法 | 路径 | 说明 | 权限 |
|------|------|------|------|
| GET | `/config` | 获取当前运行配置（脱敏） | read |
| GET | `/config/scheduler` | 调度器配置 | read |
| PUT | `/config/scheduler` | 热更新调度器参数（部分字段） | admin |
| GET | `/config/cache` | 缓存配置 | read |
| PUT | `/config/cache` | 热更新缓存参数（淘汰水位等） | admin |

**可热更新参数**：

| 参数 | 范围 | 说明 |
|------|------|------|
| `scheduler.recall.merge_window_secs` | 10–300 | 取回合并窗口 |
| `scheduler.recall.max_concurrent_restores` | 1–100 | 最大并发取回 |
| `scheduler.archive.aggregation_window_secs` | 60–7200 | 归档聚合窗口 |
| `cache.eviction_low_watermark` | 0.5–0.95 | 淘汰低水位 |
| `cache.max_size_bytes` | — | 缓存容量上限 |

### 3.13 WebSocket 实时推送

```
WS /api/v1/admin/ws
```

**推送事件类型**：

```rust
pub enum WsEvent {
    TaskStatusChanged {
        task_type: String,
        task_id: Uuid,
        old_status: String,
        new_status: String,
    },
    AlertFired {
        alert_id: String,
        severity: String,
        message: String,
    },
    AlertResolved {
        alert_id: String,
    },
    TapeOfflineRequest {
        tape_id: String,
        barcode: Option<String>,
        reason: String,
    },
    DriveStatusChanged {
        drive_id: String,
        old_status: String,
        new_status: String,
    },
    ArchiveProgress {
        task_id: Uuid,
        bytes_written: u64,
        total_size: u64,
        progress_percent: f64,
    },
    RecallProgress {
        task_id: Uuid,
        objects_read: u32,
        total_objects: u32,
        progress_percent: f64,
    },
    ClusterLeaderChanged {
        old_leader: Option<u64>,
        new_leader: u64,
        term: u64,
    },
}
```

客户端连接后可通过 JSON 消息订阅特定事件类别：

```json
{ "subscribe": ["task", "alert", "tape", "drive", "cluster"] }
```

---

## 4. 认证与鉴权

### 4.1 认证方式

| 方式 | 适用场景 |
|------|----------|
| **JWT Bearer Token** | Web UI 登录后使用 |
| **API Key** | 自动化脚本 / 外部系统调用 |

```
Authorization: Bearer <jwt_token>
```

或

```
X-API-Key: <api_key>
```

### 4.2 用户与角色

```rust
pub struct AdminUser {
    pub id: Uuid,
    pub username: String,
    pub password_hash: String,
    pub role: AdminRole,
    pub created_at: DateTime<Utc>,
    pub last_login_at: Option<DateTime<Utc>>,
}

pub enum AdminRole {
    SuperAdmin,    // 全部权限
    Admin,         // 管理操作 + 读
    ReadOnly,      // 只读
}
```

### 4.3 权限矩阵

| 操作类型 | SuperAdmin | Admin | ReadOnly |
|----------|-----------|-------|----------|
| 查看集群/状态 | ✓ | ✓ | ✓ |
| 查看对象/磁带/任务 | ✓ | ✓ | ✓ |
| 查看监控/审计 | ✓ | ✓ | ✓ |
| 重试/取消任务 | ✓ | ✓ | ✗ |
| 创建/删除桶 | ✓ | ✓ | ✗ |
| 磁带上下线/退役 | ✓ | ✓ | ✗ |
| 热更新配置 | ✓ | ✓ | ✗ |
| 节点增删 | ✓ | ✗ | ✗ |
| 用户管理 | ✓ | ✗ | ✗ |

### 4.4 登录 API

| 方法 | 路径 | 说明 |
|------|------|------|
| POST | `/auth/login` | 登录，返回 JWT |
| POST | `/auth/refresh` | 刷新 Token |
| POST | `/auth/logout` | 登出（可选 Token 黑名单） |
| GET | `/auth/me` | 当前用户信息 |

---

## 5. 前端页面设计

### 5.1 页面导航结构

```
┌──────────────────────────────────────────────────────────────┐
│ ColdStore Admin                               [用户名] [退出] │
├────────────┬─────────────────────────────────────────────────┤
│            │                                                 │
│  📊 仪表板  │   ┌─────────────────────────────────────┐      │
│            │   │         主内容区                     │      │
│  🖥 集群    │   │                                     │      │
│   ├ 概览    │   │                                     │      │
│   └ 节点    │   │                                     │      │
│            │   │                                     │      │
│  📦 存储    │   │                                     │      │
│   ├ 桶管理  │   │                                     │      │
│   └ 对象查询 │   │                                     │      │
│            │   │                                     │      │
│  📼 磁带    │   │                                     │      │
│   ├ 介质管理 │   └─────────────────────────────────────┘      │
│   ├ 驱动    │                                                 │
│   └ 离线请求 │                                                 │
│            │                                                 │
│  📋 任务    │                                                 │
│   ├ 归档任务 │                                                 │
│   └ 取回任务 │                                                 │
│            │                                                 │
│  💾 缓存    │                                                 │
│            │                                                 │
│  📈 监控    │                                                 │
│   ├ 指标    │                                                 │
│   └ 告警    │                                                 │
│            │                                                 │
│  ⚙ 设置    │                                                 │
│   ├ 配置    │                                                 │
│   ├ 用户    │                                                 │
│   └ 审计日志 │                                                 │
│            │                                                 │
└────────────┴─────────────────────────────────────────────────┘
```

### 5.2 仪表板页面

**路由**：`/dashboard`

```
┌─────────────────────────────────────────────────────────────────────────┐
│                            ColdStore Dashboard                            │
├──────────────┬──────────────┬──────────────┬────────────────────────────┤
│  S3 QPS      │  错误率      │  集群状态     │  活跃告警                   │
│  ████ 975/s  │  0.3%        │  ✅ 3/3 节点  │  ⚠ 2 条                    │
├──────────────┴──────────────┴──────────────┴────────────────────────────┤
│                                                                         │
│  ┌─ 请求延迟趋势（P50/P95/P99）───────────────────────────────────────┐│
│  │  [折线图: 24h, 按 method 分组]                                    ││
│  └──────────────────────────────────────────────────────────────────┘│
│                                                                         │
│  ┌─ 存储概览 ─────────────────────┐  ┌─ 磁带与驱动 ──────────────────┐│
│  │  总对象: 2.5M                  │  │  磁带总数: 120               ││
│  │  总容量: 2.1 PB               │  │  在线: 95 / 离线: 25          ││
│  │  Restored: 100 GB             │  │  驱动: 4 (3 in-use, 1 idle)  ││
│  │  ColdPending: 5 GB           │  │  归档吞吐: 320 MB/s          ││
│  │  Cold: 2.0 PB                │  │                               ││
│  │  [饼图]                       │  │  [驱动状态色块图]              ││
│  └────────────────────────────────┘  └────────────────────────────────┘│
│                                                                         │
│  ┌─ 缓存 ──────────────────────┐  ┌─ 任务队列 ──────────────────────┐│
│  │  容量: 750GB / 1TB (75%)    │  │  归档 Pending: 3               ││
│  │  命中率: 90.9%              │  │  取回 Expedited: 2             ││
│  │  [使用量仪表盘]              │  │  取回 Standard: 35             ││
│  │                             │  │  取回 Bulk: 150                ││
│  │  [命中率趋势折线]            │  │  WaitingForMedia: 3           ││
│  └──────────────────────────────┘  └────────────────────────────────┘│
│                                                                         │
│  ┌─ 最近事件 ─────────────────────────────────────────────────────────┐│
│  │  10:15  ArchiveTask a1b2... completed (500 objects, 5.2GB)       ││
│  │  10:12  RecallTask c3d4... started on drive-1 (TAPE003)          ││
│  │  10:10  ⚠ Tape TAPE042 offline request created                  ││
│  │  10:05  Cache eviction batch: 64 objects, 2.1GB freed            ││
│  │  09:58  Raft leader changed: node-2 → node-1 (term 42)          ││
│  └────────────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────────────┘
```

### 5.3 集群概览页面

**路由**：`/cluster`

| 区域 | 内容 |
|------|------|
| 集群状态卡片 | cluster_id、term、committed_index、Leader 节点 |
| 节点列表表格 | node_id、地址、角色（Tag 颜色）、心跳时间、操作（移除） |
| Raft 状态 | log index、commit index、apply index、snapshot index |
| 节点健康时间线 | 最近 24h 心跳/角色变化的时间轴 |

### 5.4 桶管理页面

**路由**：`/buckets`

| 区域 | 内容 |
|------|------|
| 桶列表表格 | 桶名、对象数、总大小、版本控制、创建时间 |
| 桶详情（点击展开） | 各 StorageClass（ColdPending/Cold）统计饼图 |
| 操作 | 创建桶（弹窗表单）、删除桶（二次确认） |

### 5.5 对象查询页面

**路由**：`/objects`

```
┌─────────────────────────────────────────────────────────────────────┐
│  桶: [my-bucket ▼]  前缀: [data/____]  类别: [全部 ▼]  状态: [全部 ▼] │
│                                                        [🔍 查询]    │
├─────────────────────────────────────────────────────────────────────┤
│  Key              │ Size    │ StorageClass │ Restore   │ 操作       │
│─────────────────────────────────────────────────────────────────────│
│  data/file1.bin   │ 100 MB  │ Cold 🏔      │ Completed │ [详情]     │
│  data/file2.bin   │ 50 MB   │ Cold 🏔      │ —         │ [详情]     │
│  data/file3.bin   │ 200 MB  │ ColdPending ⏳│ —         │ [详情]     │
│  data/file4.bin   │ 1 GB    │ Cold 🏔      │ InProgress│ [详情]     │
├─────────────────────────────────────────────────────────────────────┤
│  共 4 条  [< 1 >]                                                   │
└─────────────────────────────────────────────────────────────────────┘
```

**对象详情弹窗**：

| Tab | 内容 |
|-----|------|
| 基本信息 | bucket、key、size、checksum、content_type、etag、版本 |
| 归档信息 | archive_id、tape_id、tape_set、tape_block_offset、Bundle 状态 |
| 取回信息 | restore_status、expire_at、tier、recall_task_id、历史 |
| 时间线 | 创建时间、归档时间、取回时间、状态流转时间线 |

### 5.6 磁带介质管理页面

**路由**：`/tapes`

```
┌─────────────────────────────────────────────────────────────────────┐
│  状态: [全部 ▼]  格式: [全部 ▼]                [🔍 筛选]  [+ 注册磁带] │
├─────────────────────────────────────────────────────────────────────┤
│  磁带 ID    │ 条码        │ 格式  │ 状态    │ 容量使用     │ Bundle │ 操作│
│─────────────────────────────────────────────────────────────────────│
│  TAPE001    │ TAPE001L9  │ LTO-9 │ ✅ Online│ ████░░ 70%  │ 245   │ ... │
│  TAPE002    │ TAPE002L9  │ LTO-9 │ ✅ Online│ ██████ 95%  │ 380   │ ... │
│  TAPE042    │ TAPE042L9  │ LTO-9 │ ❌ Offline│ ███░░░ 45%  │ 120   │ ... │
│  TAPE099    │ TAPE099L9  │ LTO-9 │ ⚠ Error │ █████░ 82%  │ 290   │ ... │
└─────────────────────────────────────────────────────────────────────┘
```

**操作菜单**：详情、校验、上线/下线、退役

**磁带详情页（点击进入）**：

| 区域 | 内容 |
|------|------|
| 基本信息 | ID、条码、格式、状态、位置 |
| 容量可视化 | 已用/剩余条形图、Bundle 数量 |
| Bundle 列表 | 该磁带上所有 Bundle（ID、对象数、大小、状态、时间） |
| 校验历史 | 最近校验结果、失败块数、时间 |
| 错误记录 | 错误类型、时间、处理状态 |

### 5.7 驱动管理页面

**路由**：`/drives`

每个驱动显示为卡片：

```
┌─ Drive 0 (/dev/nst0) ─────────────────┐  ┌─ Drive 1 (/dev/nst1) ──────────┐
│  状态: 🟢 InUse                        │  │  状态: 🟡 Available              │
│  当前磁带: TAPE001                     │  │  当前磁带: —                     │
│  当前任务: Archive a1b2... (60%)       │  │  等待队列: 2 个任务              │
│  利用率 (1h): ████████░░ 85%          │  │  利用率 (1h): █░░░░░░░░░ 12%   │
│  累计写入: 1.0 TB                      │  │  累计读取: 250 GB               │
│  [弹出磁带]  [清洁]                    │  │  [弹出磁带]  [清洁]              │
└────────────────────────────────────────┘  └─────────────────────────────────┘
```

### 5.8 取回任务页面

**路由**：`/tasks/recall`

| 区域 | 内容 |
|------|------|
| 队列概览 | 三级队列深度柱状图、WaitingForMedia 数量 |
| 任务列表表格 | ID、bucket/key、tier（标签颜色）、状态、磁带、等待时间、操作 |
| 筛选 | 状态、优先级、磁带 ID |
| 操作 | 重试、取消、调整优先级 |

**状态标签颜色**：

| 状态 | 颜色 |
|------|------|
| Pending | 蓝色 |
| WaitingForMedia | 橙色 |
| InProgress | 绿色动画 |
| Completed | 灰色 |
| Failed | 红色 |

### 5.9 离线请求页面

**路由**：`/tapes/offline`

| 区域 | 内容 |
|------|------|
| 待处理请求列表 | tape_id、条码、原因、请求时间、等待时长、关联任务数 |
| 操作 | 确认已上线（触发 `confirm_media_online`） |
| 历史记录 | 已处理的离线请求、处理耗时 |

### 5.10 告警页面

**路由**：`/alerts`

| Tab | 内容 |
|-----|------|
| 活跃告警 | 严重级别（P1 红/P2 橙/P3 黄）、告警名、触发时间、持续时间、确认按钮 |
| 历史告警 | 时间范围筛选、告警名、触发→恢复时间、处理人 |

### 5.11 审计日志页面

**路由**：`/audit`

可搜索、可按时间范围/用户/操作类型筛选的表格：

| 时间 | 用户 | 操作 | 资源 | 详情 | 结果 |
|------|------|------|------|------|------|
| 10:15:30 | admin | retry_recall_task | recall/c3d4... | {"old_status":"Failed"} | ✅ |
| 10:12:00 | admin | confirm_online | tape/TAPE042 | {} | ✅ |
| 09:50:00 | system | evict_cache | cache/batch | {"count":64} | ✅ |

---

## 6. 后端模块结构

```
src/admin/
├── mod.rs                    # pub use，Router 构建
├── router.rs                 # Axum Router 定义（/api/v1/admin/*）
├── auth/
│   ├── mod.rs
│   ├── jwt.rs                # JWT 签发/验证
│   ├── middleware.rs          # 认证中间件
│   └── models.rs             # AdminUser, AdminRole
├── handlers/
│   ├── mod.rs
│   ├── cluster.rs            # 集群管理 handler
│   ├── bucket.rs             # 桶管理 handler
│   ├── object.rs             # 对象查询 handler
│   ├── archive_task.rs       # 归档任务 handler
│   ├── recall_task.rs        # 取回任务 handler
│   ├── tape.rs               # 磁带管理 handler
│   ├── drive.rs              # 驱动管理 handler
│   ├── cache.rs              # 缓存监控 handler
│   ├── metrics.rs            # 监控指标 handler
│   ├── alert.rs              # 告警 handler
│   ├── config.rs             # 配置管理 handler
│   └── audit.rs              # 审计日志 handler
├── websocket.rs              # WebSocket 推送
├── dto/                      # 请求/响应 DTO
│   ├── mod.rs
│   ├── common.rs             # ApiResponse, PageRequest, PageResponse
│   ├── cluster.rs
│   ├── bucket.rs
│   ├── object.rs
│   ├── task.rs
│   ├── tape.rs
│   ├── drive.rs
│   ├── cache.rs
│   └── alert.rs
└── audit.rs                  # 审计日志写入逻辑
```

### 6.1 前端项目结构

```
console/
├── package.json
├── vite.config.ts
├── tsconfig.json
├── src/
│   ├── main.tsx
│   ├── App.tsx
│   ├── routes.tsx             # React Router 路由定义
│   ├── api/                   # API 封装
│   │   ├── client.ts          # Axios 实例（含 JWT 拦截器）
│   │   ├── cluster.ts
│   │   ├── bucket.ts
│   │   ├── object.ts
│   │   ├── task.ts
│   │   ├── tape.ts
│   │   ├── drive.ts
│   │   ├── cache.ts
│   │   ├── metrics.ts
│   │   ├── alert.ts
│   │   └── auth.ts
│   ├── stores/                # Zustand 状态
│   │   ├── useAuthStore.ts
│   │   ├── useWebSocketStore.ts   # WS 连接 + 事件分发
│   │   └── useNotificationStore.ts
│   ├── pages/                 # 页面组件
│   │   ├── Dashboard.tsx
│   │   ├── cluster/
│   │   │   ├── Overview.tsx
│   │   │   └── NodeDetail.tsx
│   │   ├── storage/
│   │   │   ├── BucketList.tsx
│   │   │   ├── BucketDetail.tsx
│   │   │   └── ObjectQuery.tsx
│   │   ├── tape/
│   │   │   ├── TapeList.tsx
│   │   │   ├── TapeDetail.tsx
│   │   │   ├── DriveList.tsx
│   │   │   └── OfflineRequests.tsx
│   │   ├── task/
│   │   │   ├── ArchiveTaskList.tsx
│   │   │   └── RecallTaskList.tsx
│   │   ├── cache/
│   │   │   └── CacheOverview.tsx
│   │   ├── monitor/
│   │   │   ├── Metrics.tsx
│   │   │   └── Alerts.tsx
│   │   └── settings/
│   │       ├── Config.tsx
│   │       ├── UserManage.tsx
│   │       └── AuditLog.tsx
│   ├── components/            # 通用组件
│   │   ├── StatusTag.tsx      # 状态标签（颜色映射）
│   │   ├── CapacityBar.tsx    # 容量条
│   │   ├── DriveCard.tsx      # 驱动卡片
│   │   ├── EventTimeline.tsx  # 事件时间线
│   │   └── ConfirmDialog.tsx  # 二次确认弹窗
│   ├── hooks/                 # 自定义 Hooks
│   │   ├── usePolling.ts      # 轮询数据刷新
│   │   └── useWebSocket.ts    # WS 连接 Hook
│   └── utils/
│       ├── format.ts          # 字节/时间格式化
│       └── constants.ts       # 枚举映射
```

---

## 7. 配置项

```yaml
admin:
  enabled: true
  listen: "0.0.0.0:8080"
  cors_origins: ["http://localhost:5173"]

  # Console 仅需配置 Metadata 地址
  metadata_addrs:
    - "10.0.1.1:21001"
    - "10.0.1.2:21001"
    - "10.0.1.3:21001"

  auth:
    jwt_secret: "${ADMIN_JWT_SECRET}"
    jwt_expire_hours: 24
    api_keys:
      - name: "monitoring"
        key: "${MONITORING_API_KEY}"
        role: "ReadOnly"

  initial_user:
    username: "admin"
    password: "${ADMIN_INITIAL_PASSWORD}"
    role: "SuperAdmin"

  websocket:
    heartbeat_interval_secs: 30
    max_connections: 100

  audit:
    enabled: true
    retention_days: 90

  static_files:
    enabled: true
    path: "./console/dist"     # 前端构建产物，生产环境嵌入
```

---

## 8. 参考资料

- [Axum](https://docs.rs/axum/latest/axum/) — HTTP 框架
- [utoipa](https://docs.rs/utoipa/latest/utoipa/) — OpenAPI 文档生成
- [jsonwebtoken](https://docs.rs/jsonwebtoken) — JWT 实现
- [React 18](https://react.dev/) — 前端框架
- [Ant Design 5](https://ant.design/) — UI 组件库
- [Zustand](https://zustand-demo.pmnd.rs/) — 状态管理
- [React Router 6](https://reactrouter.com/) — 路由
- [ECharts](https://echarts.apache.org/) — 图表库
