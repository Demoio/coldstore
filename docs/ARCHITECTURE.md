# ColdStore 当前架构与组件职责

本文档描述当前代码基线，而不是完整目标态。目标态设计仍保留在 `docs/modules/` 与 `docs/DESIGN.md` 中。

## 1. 系统定位

ColdStore 是一个 S3 兼容的磁带冷归档系统。当前实现重点是把以下闭环做成可验证路径：

- S3 HTTP 接入
- Scheduler 业务编排
- Metadata 状态机与持久化
- Cache staging/restored 数据面
- Tape 抽象读写接口
- 归档完成的原子 metadata 提交
- Restore 后从缓存读取

当前不是完整多节点分布式系统。Metadata 多节点 `persistent_raft` 当前会 fail-fast，避免多个本地状态机伪装成安全 Raft 集群。

## 2. 组件总览

```text
S3 Client
   |
   | HTTP/S3
   v
Gateway
   |
   | gRPC SchedulerService
   v
Scheduler
   |                 |                  |
   | MetadataService | CacheService     | TapeService
   v                 v                  v
Metadata          Cache              Tape
```

## 3. 组件职责

| 组件 | crate | 职责 | 不负责 |
|------|-------|------|--------|
| Gateway | `crates/gateway` | S3 HTTP 路由、Range/条件请求解析、Restore XML 解析、S3 错误码映射、转发 Scheduler | 不直接访问 Metadata/Cache/Tape，不做归档决策 |
| Scheduler | `crates/scheduler` | 业务中枢；对象 PUT/GET/HEAD/DELETE/List；Restore 请求；归档扫描；召回调度；编排 Metadata/Cache/Tape | 不持久化对象数据，不直接管理磁盘文件 |
| Metadata | `crates/metadata` | bucket/object/task/tape/worker 元数据状态机；归档完成 CAS；单节点持久化路径；worker 注册与心跳 | 当前不提供真实多节点 Raft 复制 |
| Cache | `crates/cache` | staging 写入暂存；restored 取回缓存；容量预算；restored 淘汰；staging 背压 | 不保存权威元数据，不淘汰 staging |
| Tape | `crates/tape` | TapeService 抽象；bundle 写入/读取；驱动加载/释放接口 | 当前真实硬件接入不是完整生产态 |
| Common | `crates/common` | 配置结构、默认值、共享类型 | 不包含业务状态机 |
| Proto | `crates/proto` | gRPC/protobuf 契约 | 不包含运行时逻辑 |

## 4. 默认拓扑

当前默认配置是安全单节点 metadata 拓扑：

```text
Metadata:  127.0.0.1:21001
Scheduler: 127.0.0.1:22001
Cache:     127.0.0.1:23001
Tape:      127.0.0.1:24001
Gateway:   0.0.0.0:9000
```

默认 `MetadataConfig`：

- `consensus_mode = persistent_raft`
- `cluster = 1:127.0.0.1:21001`
- `raft_state_path = data_path/node-{node_id}/raft`，除非显式配置

多节点 metadata cluster 当前不是默认路径。配置多节点 `persistent_raft` 会拒绝启动，因为真实 distributed Raft runtime 尚未完成。

## 5. 核心数据流

### 5.1 PutObject 写入

```text
Client PUT
  -> Gateway
  -> Scheduler.PutObject
  -> Cache.PutStaging
  -> Metadata.PutObject(ColdPending)
  -> Response(etag/version_id)
```

关键语义：

- 写入后对象是 `ColdPending`。
- 数据进入 Cache 的 staging 区。
- staging 属于写入路径 durable buffer，不能被 eviction 自动淘汰。
- staging 超预算时返回容量背压，上游应重试或等待归档 drain。

### 5.2 Archive 归档

```text
Scheduler archive loop
  -> Cache.ListStagingKeys
  -> Metadata.HeadObject
  -> Cache.GetStaging
  -> 校验 staging identity/size/checksum 与 metadata 一致
  -> Tape.WriteBundle
  -> Metadata.CompleteArchiveObject(expected_*)
  -> Cache.DeleteStaging
```

`CompleteArchiveObject` 是归档完成的关键一致性边界。它在 Metadata 状态机单次 apply 内完成：

- 检查 object version
- 检查 expected size
- 检查 expected checksum
- 检查 expected storage class
- 检查 expected updated_at，作为 generation token
- 写入 archive bundle
- 更新 object archive location
- 将 object storage class 切换为 `Cold`

如果 tape 写成功但 CAS 失败，metadata 不会错误提交归档结果。物理 tape 上可能留下未引用 bundle，这是后续 audit/GC 需要处理的残余问题。

### 5.3 RestoreObject 取回

```text
Client RestoreObject
  -> Gateway
  -> Scheduler.RestoreObject
  -> Metadata 更新 restore 状态 / 创建 RecallTask
  -> Response 202/200/409

Scheduler recall loop
  -> Metadata.ListPendingRecallTasks
  -> Tape.ReadBundle
  -> 校验 checksum/size
  -> Cache.PutRestored
  -> Metadata 更新 task/object restore 状态
```

关键语义：

- Expedited 不可用时映射为 S3 `GlacierExpeditedRetrievalNotAvailable`。
- Cache restored 超预算属于可重试背压。
- Restored 数据有 TTL，到期后可被回收。

### 5.4 GetObject 读取

```text
Client GET
  -> Gateway
  -> Scheduler.GetObject
  -> Metadata.Head/GetObject
  -> 若对象未 Restore completed: InvalidObjectState
  -> Cache.Get
  -> Gateway HTTP response
```

当前限制：

- Gateway 和 Scheduler 的流式接口仍存在部分全量 `Vec` 聚合路径，大对象内存放大仍需后续重构。
- Range 读取当前不是完整下推到后端的生产态实现。

### 5.5 ListObjects

```text
Client ListObjects
  -> Gateway parse prefix/marker/delimiter/max-keys
  -> Scheduler.ListObjects
  -> Metadata.ListObjects
  -> Gateway XML response
```

当前实现支持基础 V1 风格 list、delimiter/common prefixes 和最大 `max-keys` 边界。完整 ListObjectsV2 仍是后续任务。

## 6. 一致性边界

| 场景 | 当前策略 |
------|----------|
| Metadata 多节点 | 未完成真实分布式 Raft，多节点 fail-fast |
| Archive 提交 | 使用 `CompleteArchiveObject(expected_*)` 单状态机命令 |
| Staging 容量压力 | 不淘汰 staging，返回 `ResourceExhausted` |
| Restored 容量压力 | 可淘汰 restored；无 victim 时返回背压 |
| Cache 覆盖写 | 先写新 storage，成功后原子切索引，再 best-effort 删除旧 storage |
| Cache 重建重复 key | 选择较新 entry，duplicate loser best-effort 清理并 warning |
| Gateway 节流 | S3 数据面返回 `503 SlowDown + Retry-After` |

## 7. 组件间接口

### Gateway -> Scheduler

接口：`SchedulerService`

用途：

- PUT/GET/HEAD/DELETE
- RestoreObject
- ListObjects/ListBuckets
- Bucket CRUD

Gateway 只做协议适配，不绕过 Scheduler。

### Scheduler -> Metadata

接口：`MetadataService`

用途：

- bucket/object 元数据读写
- archive bundle/task/recall task 管理
- `CompleteArchiveObject` 原子归档提交
- worker/tape 状态读写

Scheduler 是业务路径中的唯一 metadata 写入编排者。

### Scheduler -> Cache

接口：`CacheService`

用途：

- `PutStaging`：写入待归档数据
- `GetStaging`：归档时读取 staging
- `DeleteStaging`：归档提交成功后清理 staging
- `PutRestored`：召回后写入 restored
- `Get`：GET 已解冻对象
- `Stats`：缓存统计

### Scheduler -> Tape

接口：`TapeService`

用途：

- Acquire/Release drive
- Load/Unload tape
- WriteBundle
- ReadBundle

Tape 不直接写 Metadata。写 tape 成功后由 Scheduler 调用 Metadata 的原子提交接口。

## 8. 状态模型

### Object storage class

```text
ColdPending -> Cold
```

- `ColdPending`：对象已写入 staging，等待归档。
- `Cold`：对象已归档，metadata 中有 archive location。

### Restore status

```text
Pending -> WaitingForMedia -> InProgress -> Completed
                                  |
                                  v
                                Failed
Completed -> Expired
```

当前实现已有基础状态流转，但 task 与 object restore 状态仍需要进一步原子化/reconciler 补强。

### Cache category

```text
Staging:  write path buffer, no eviction
Restored: read cache, evictable
```

## 9. 当前仍需补强的事项

| 优先级 | 事项 |
--------|------|
| P0 | 真正 distributed Raft runtime、leader redirect、quorum commit、log replay |
| P0 | Restore task 与 object restore 状态原子化，避免 pending-without-task |
| P1 | Gateway/Scheduler/Cache 真流式大对象读写，降低内存放大 |
| P1 | Tape orphan bundle audit/GC，处理 tape 写成功但 metadata CAS 失败的残留 |
| P1 | ListObjectsV2、DeleteObject 幂等、更多 S3 兼容细节 |
| P2 | Admin Console、完整 OpenTelemetry、真实 SPDK/真实磁带库生产验证 |

## 10. 文档使用说明

- 判断当前实现时，以本文档和代码为准。
- `docs/modules/*` 保留更完整目标态设计，可能包含当前未实现能力。
- 如果文档与代码冲突，应更新本文档或在模块文档中标记“目标态”。
