# 跨层数据一致性与性能设计

> 所属架构：ColdStore 冷存储系统  
> 参考：[总架构设计](../DESIGN.md) | [03-元数据集群](./03-metadata-cluster.md) | [04-缓存层](./04-cache-layer.md) | [05-调度层](./05-scheduler-layer.md) | [06-磁带层](./06-tape-layer.md)

## 1. 设计背景

ColdStore 跨越四个核心层（元数据 → 缓存 → 调度 → 磁带），每次归档或取回操作涉及多步写入，且目标介质（磁带）具有高延迟、不可随机写的特性。在无分布式事务支持的情况下，需要通过 **Saga 模式 + 幂等操作 + 补偿机制** 保证最终一致性，同时通过 **流水线、聚合、并发控制** 保证吞吐性能。

### 1.1 核心约束

| 约束 | 来源 | 影响 |
|------|------|------|
| 磁带不可回滚 | 磁带追加写入，无法撤销 | 归档写入必须"先磁带后元数据"，不可反序 |
| Raft 写入串行 | 所有写入经 Leader 排序 | 元数据写入是全局瓶颈，需批量化 |
| SPDK 用户态 | 缓存层运行在独立线程 | 跨线程通信需 channel/Arc，不能直接共享锁 |
| 磁带换带延迟 | 30s–2min 机械操作 | 调度层必须合并同磁带请求，减少换带 |

### 1.2 一致性层级

| 层 | 一致性保证 | 说明 |
|------|-----------|------|
| 元数据集群 (03) | **强一致（线性一致性）** | Raft 共识，写入多数派确认 |
| 调度层 (05) | **Saga 最终一致性** | 多步操作通过补偿保证最终状态正确 |
| 缓存层 (04) | **弱一致（最终一致）** | 缓存数据是元数据的"投影"，可丢失可重建 |
| 磁带层 (06) | **持久化一致性** | 一旦写入成功即持久，双副本保障 |

---

## 2. 数据一致性模型

### 2.1 元数据强一致（Raft 共识）

```
                写入路径
Client ──▶ Leader ──▶ Raft Propose
                        │
              ┌─────────┼─────────┐
              ▼         ▼         ▼
          Follower1  Follower2  Leader
              │         │         │
              └────┬────┘         │
            多数派 Ack            │
                   └──────▶ Commit ──▶ Apply ──▶ RocksDB
```

**关键保证**：

| 保证 | 实现 | 适用场景 |
|------|------|----------|
| 写入原子性 | 单个 `ColdStoreRequest` 为一条 Raft 日志 | 所有写操作 |
| 写入顺序 | Raft log index 全局有序 | 同一对象的状态流转不乱序 |
| 线性读 | `ensure_linearizable()` 确认 Leader 地位 | `find_active_recall` 检测重复 |
| 最终一致读 | 任意节点本地 RocksDB | GET 查询 restore_status |

**写入合并优化**：

对于归档完成这类需要同时更新多个 CF 的操作，状态机 apply 内使用 RocksDB `WriteBatch` 保证原子性：

```rust
fn apply_archive_complete(
    &self,
    bundle: &ArchiveBundle,
    object_updates: &[(String, String, Uuid, String, Vec<String>, u64)],
) -> Result<()> {
    let mut batch = WriteBatch::default();

    // 1. 写入 ArchiveBundle
    batch.put_cf(cf_bundles, bundle_key, serialize(bundle));

    // 2. 批量更新 ObjectMetadata
    for (bucket, key, archive_id, tape_id, tape_set, offset) in object_updates {
        let obj_key = format!("obj:{}:{}", bucket, key);
        let mut meta: ObjectMetadata = self.get_cf(cf_objects, &obj_key)?;
        meta.storage_class = StorageClass::Cold;
        meta.archive_id = Some(*archive_id);
        meta.tape_id = Some(tape_id.clone());
        meta.tape_set = Some(tape_set.clone());
        meta.tape_block_offset = Some(*offset);
        meta.updated_at = Utc::now();
        batch.put_cf(cf_objects, &obj_key, serialize(&meta));

        // 3. 删除 ColdPending 索引
        let pend_key = format!("pend:{}:{}:{}", meta.created_at.timestamp(), bucket, key);
        batch.delete_cf(cf_idx_pending, &pend_key);

        // 4. 写入反查索引
        let ibo_key = format!("ibo:{}:{}:{}", archive_id, bucket, key);
        batch.put_cf(cf_idx_bundle_objects, &ibo_key, b"");
    }

    // 5. 写入磁带→Bundle 索引
    let itb_key = format!("itb:{}:{}", bundle.tape_id, bundle.id);
    batch.put_cf(cf_idx_tape_bundles, &itb_key, b"");

    self.db.write(batch)?;
    Ok(())
}
```

### 2.2 归档流程一致性（Saga 模式）

归档是不可逆操作（磁带数据已写入），采用**前向恢复**策略：

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                         归档 Saga 流程                                        │
│                                                                              │
│  Step 1          Step 2              Step 3              Step 4              │
│  扫描聚合   ──▶  申请驱动/磁带  ──▶  磁带顺序写入  ──▶  更新元数据          │
│  (调度层)        (磁带层)            (磁带层)            (元数据层)           │
│                                                                              │
│  失败处理:                                                                   │
│  Step 1 失败: 无副作用，直接重试                                              │
│  Step 2 失败: 释放资源，重入队列                                              │
│  Step 3 失败: ┌─ 重试(同磁带/换磁带)                                         │
│               └─ 超限 → Bundle 标记 Failed，对象回退 ColdPending              │
│  Step 4 失败: ┌─ 磁带已写入（不可撤销）                                      │
│               ├─ 重试元数据更新（指数退避，最多 10 次）                        │
│               └─ 超限 → 写入补偿日志，后台修复任务定期扫描重试                  │
└──────────────────────────────────────────────────────────────────────────────┘
```

**关键不变量**：

| 不变量 | 含义 | 违反后果 | 保护机制 |
|--------|------|----------|----------|
| 磁带写入 → 元数据更新 | 先确认磁带持久化，再标记 Cold | 若反序：元数据为 Cold 但磁带无数据，取回时丢数据 | 强制顺序 + Step 4 重试 |
| archive_id 唯一 | 每个 Bundle 全局唯一 | 若重复：覆盖旧 Bundle 信息 | UUID v4 + 幂等写入 |
| ColdPending → Cold 原子 | 状态流转不可跳过 | 若跳过：对象状态异常 | WriteBatch 原子操作 |

**补偿日志**：

Step 4 持续失败时，将未完成的元数据更新写入本地补偿日志：

```rust
pub struct CompensationEntry {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub saga_type: SagaType,
    pub payload: CompensationPayload,
    pub retry_count: u32,
    pub last_retry_at: Option<DateTime<Utc>>,
    pub status: CompensationStatus,
}

pub enum SagaType {
    ArchiveComplete,
    RecallComplete,
}

pub enum CompensationPayload {
    ArchiveComplete {
        bundle: ArchiveBundle,
        object_keys: Vec<(String, String)>,
    },
    RecallComplete {
        recall_task_id: Uuid,
        bucket: String,
        key: String,
        restore_expire_at: DateTime<Utc>,
    },
}

pub enum CompensationStatus {
    Pending,
    Retrying,
    Resolved,
    Abandoned,
}
```

**补偿任务调度器**：

```rust
pub struct CompensationRunner {
    metadata: Arc<MetadataClient>,
    log_store: Arc<CompensationLogStore>,
    config: CompensationConfig,
}

pub struct CompensationConfig {
    pub scan_interval_secs: u64,       // 扫描间隔：60s
    pub max_retries: u32,              // 最大重试次数：100
    pub retry_backoff_base_ms: u64,    // 退避基数：1000ms
    pub retry_backoff_max_ms: u64,     // 退避上限：300_000ms（5 分钟）
    pub abandon_after_hours: u64,      // 超时放弃：72 小时
}
```

### 2.3 取回流程一致性（Saga 模式）

取回流程涉及三个目标（磁带 → 缓存 → 元数据），采用**前向恢复 + 容错降级**：

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                         取回 Saga 流程                                        │
│                                                                              │
│  Step 1           Step 2             Step 3             Step 4              │
│  申请驱动    ──▶  磁带读取     ──▶  写入缓存     ──▶  更新元数据           │
│  定位磁带         (磁带层)           (缓存层)           (元数据层)           │
│  (磁带层)                                                                    │
│                                                                              │
│  失败处理:                                                                   │
│  Step 1 失败: 释放资源，重入队列 / WaitingForMedia（离线）                    │
│  Step 2 失败: ┌─ 重试（同磁带 2 次）                                         │
│               ├─ 切换副本磁带（tape_set 中选下一盘）                          │
│               └─ 超限 → RecallTask Failed                                    │
│  Step 3 失败: ┌─ 触发缓存淘汰后重试（2 次）                                  │
│               └─ 超限 → RecallTask Failed，不更新元数据                       │
│  Step 4 失败: ┌─ 缓存已有数据（不可撤销，等待 TTL 过期自动清理）              │
│               ├─ 重试元数据更新（指数退避，最多 10 次）                        │
│               └─ 超限 → 写入补偿日志；此时缓存有数据但 GET 不可用             │
└──────────────────────────────────────────────────────────────────────────────┘
```

**取回状态机**：

```
                        ┌─────────────────────────────────┐
                        │                                 │
                        ▼                                 │
  RestoreObject ──▶ Pending ──▶ WaitingForMedia ──────────┘
                        │              (离线磁带上线后)
                        ▼
                   InProgress ──▶ Completed
                        │              │
                        ▼              └──▶ Expired (TTL 过期)
                      Failed
```

| 状态流转 | 触发条件 | 写入方 | 幂等性 |
|----------|----------|--------|--------|
| → Pending | RestoreObject API | 协议层 | `find_active_recall` 线性读防重复 |
| Pending → WaitingForMedia | 磁带 Offline | 调度层 | 幂等：重复设置无副作用 |
| WaitingForMedia → Pending | 磁带 Online 确认 | 调度层 | 幂等 |
| Pending → InProgress | 分配驱动、开始读取 | 调度层 | 幂等 |
| InProgress → Completed | 缓存写入 + 元数据更新均成功 | 调度层 | 幂等：重复更新同值 |
| InProgress → Failed | 超过最大重试 | 调度层 | 幂等 |
| Completed → Expired | expire_at < now | 后台扫描 | 幂等 |

### 2.4 缓存-元数据一致性

缓存层不持有 MetadataClient（方案 B），因此缓存数据与元数据之间存在天然的弱一致窗口：

| 场景 | 缓存状态 | 元数据状态 | 用户可见行为 | 自愈方式 |
|------|----------|-----------|-------------|----------|
| 正常取回完成 | 有数据 | Completed | GET 正常返回 | — |
| Step 3 成功，Step 4 待重试 | 有数据 | InProgress/Pending | GET 返回 403（未解冻） | 补偿任务重试元数据更新 |
| 缓存淘汰（TTL/容量） | 无数据 | Completed | GET 返回 503 | 用户重新 RestoreObject |
| 缓存损坏 | 数据校验失败 | Completed | GET 返回 500 | 删除坏 Blob，用户重新 Restore |
| 元数据 Expired 但缓存未淘汰 | 有数据 | Expired | GET 返回 403 | 缓存 TTL 扫描最终删除 |

**设计原则**：元数据是**唯一真相源**（Source of Truth），缓存仅为性能优化层。任何缓存丢失都可通过重新 RestoreObject 恢复。

### 2.5 幂等性设计

所有跨层操作必须幂等，以支持安全重试：

| 操作 | 幂等 key | 幂等实现 |
|------|----------|----------|
| PutObject | `(bucket, key)` | 覆盖写入，同 key 重复 PUT 覆盖旧值 |
| PutArchiveBundle | `bundle.id` (Uuid) | 同 id 重复写入覆盖，CF `bundles` 按 id 去重 |
| UpdateStorageClass | `(bucket, key, class)` | 同 key 重复设置同值无副作用 |
| UpdateArchiveLocation | `(bucket, key)` | 同 key 重复设置同值无副作用 |
| UpdateRestoreStatus | `(bucket, key, status)` | 状态机检查：只允许合法流转 |
| PutRecallTask | `recall.id` (Uuid) | 同 id 重复写入覆盖 |
| put_restored (缓存) | `(bucket, key)` | 同 key 覆盖旧 Blob |

**状态机防护**：

```rust
fn validate_restore_transition(current: &RestoreStatus, target: &RestoreStatus) -> bool {
    matches!(
        (current, target),
        (RestoreStatus::Pending, RestoreStatus::InProgress)
        | (RestoreStatus::Pending, RestoreStatus::WaitingForMedia)
        | (RestoreStatus::Pending, RestoreStatus::Failed)
        | (RestoreStatus::WaitingForMedia, RestoreStatus::Pending)
        | (RestoreStatus::InProgress, RestoreStatus::Completed)
        | (RestoreStatus::InProgress, RestoreStatus::Failed)
        | (RestoreStatus::Completed, RestoreStatus::Expired)
    )
}
```

---

## 3. 并发控制模型

### 3.1 全局并发架构

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              Tokio Runtime                                    │
│                                                                              │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐   │
│  │ S3 Handler   │  │ Archive      │  │ Recall       │  │ Compensation │   │
│  │ (并发请求)   │  │ Scheduler    │  │ Scheduler    │  │ Runner       │   │
│  │              │  │ (单循环)     │  │ (单循环)     │  │ (单循环)     │   │
│  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘   │
│         │                 │                 │                 │             │
│         ▼                 ▼                 ▼                 ▼             │
│  ┌──────────────────────────────────────────────────────────────────────┐  │
│  │                    Arc<MetadataClient>                                │  │
│  │  内部：Raft client（自动转发到 Leader）                               │  │
│  │  并发安全：Raft 日志序列化所有写入                                    │  │
│  └──────────────────────────────────────────────────────────────────────┘  │
│                                                                              │
│  ┌──────────────────────────┐  ┌───────────────────────────────────────┐   │
│  │ Arc<CacheManager>       │  │ Arc<TapeManager>                      │   │
│  │ 内部：RwLock<CacheIndex>│  │ 内部：Mutex<DriveAllocator>           │   │
│  │ + SPDK thread pool      │  │ + per-drive sequential executor       │   │
│  └──────────────────────────┘  └───────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 3.2 锁策略与粒度

| 组件 | 锁类型 | 粒度 | 持锁时间 | 争用场景 |
|------|--------|------|----------|----------|
| MetadataClient | **无锁**（Raft 串行化） | 全局 | N/A | 所有写入经 Raft Leader 排序，无需应用层锁 |
| CacheIndex | `RwLock` | 全局索引 | 微秒级（内存 HashMap 操作） | GET 热路径读锁 + put_restored 写锁 |
| CacheBackend (SPDK) | **无锁**（SPDK reactor 线程模型） | per-thread | N/A | SPDK 内部保证线程安全 |
| DriveAllocator.available | `Mutex` | 驱动池 | 微秒级 | 归档/取回同时申请驱动 |
| DriveAllocator.waiters | `Mutex` | 等待队列 | 微秒级 | 驱动归还时唤醒等待者 |
| CacheStats | `AtomicU64` | per-counter | 纳秒级 | 高频统计更新 |
| RecallQueue | `Mutex` | per-queue | 微秒级 | 入队/出队/合并 |

### 3.3 关键并发路径分析

#### 3.3.1 GET 热路径（最频繁）

```
GET /{bucket}/{key}
    │
    ├─ 1. metadata.get_object()          ← Follower 本地读，无锁，< 100μs
    │
    ├─ 2. 检查 storage_class/restore_status ← 纯内存判断
    │
    └─ 3. cache.get(bucket, key)
              │
              ├─ 3a. index.read()          ← RwLock 读锁，< 1μs
              │
              └─ 3b. backend.read_blob()   ← SPDK 异步 I/O，10-100μs (NVMe)
```

**优化**：GET 路径全程无写锁，读锁开销可忽略。元数据使用 Follower 本地读（最终一致），接受短暂延迟。

#### 3.3.2 put_restored 写路径

```
put_restored(item)
    │
    ├─ 1. backend.create_blob(xattrs, size)   ← SPDK 创建 Blob，异步
    │
    ├─ 2. backend.write_blob(blob_id, data)   ← SPDK 写入数据，异步
    │
    └─ 3. index.write()                        ← RwLock 写锁，< 1μs
```

**优化**：写锁仅在最后一步更新内存索引时持有，持锁时间极短。SPDK I/O 不持有 CacheIndex 锁。

#### 3.3.3 驱动分配竞争

```
acquire_drive(priority)
    │
    ├─ 1. lock(available)                ← Mutex，< 1μs
    │     ├─ 有空闲驱动：pop + return DriveGuard
    │     └─ 无空闲：
    │           lock(waiters)            ← Mutex，< 1μs
    │           push(DriveWaiter { priority, oneshot })
    │           drop(available)
    │           await oneshot.recv()     ← 等待驱动释放，可能数分钟
    │
    └─ DriveGuard Drop:
          lock(available)
          if waiters.peek().priority ≤ current:
              pop waiter, send drive via oneshot
          else:
              push drive back to available
```

**优化**：Mutex 仅保护内存数据结构操作（微秒级），实际等待通过 `oneshot channel` 异步化，不阻塞 Tokio 线程。

### 3.4 死锁预防

| 规则 | 说明 |
|------|------|
| 锁排序 | 若需同时持有多锁，按固定顺序获取：`CacheIndex` → `DriveAllocator` |
| 无嵌套锁 | MetadataClient 无应用层锁，SPDK 内部锁不暴露 |
| Mutex + async | 所有 `Mutex` 在 `await` 前释放，使用 `tokio::sync::Mutex` 仅在必须跨 await 时 |
| 超时保护 | `acquire_drive` 设置超时（`drive_acquire_timeout_secs`），避免无限等待 |

---

## 4. 性能设计

### 4.1 端到端性能目标

| 场景 | 目标 | 瓶颈 | 关键优化 |
|------|------|------|----------|
| **PUT Object** | < 10ms（小对象）| 元数据 Raft 写入 | 异步 Raft，批量合并 |
| **GET Object（热）** | < 1ms | 缓存 SPDK 读取 | Follower 读 + SPDK 用户态 I/O |
| **GET Object（已解冻）** | < 5ms | 缓存 SPDK 读取 | 同上 |
| **归档吞吐** | ≥ 300 MB/s/drive | 磁带写入速度 | 块对齐 + 双缓冲 + 流水线 |
| **取回延迟（Expedited）** | < 5 分钟 | 换带 + seek + 读取 | 预留驱动 + 优先队列 |
| **取回延迟（Standard）** | < 5 小时 | 队列等待 + 换带 | 合并同磁带任务 |
| **元数据写入 QPS** | ≥ 10,000/s | Raft 共识延迟 | WriteBatch + 批量 Propose |

### 4.2 元数据层性能（03）

#### 4.2.1 写入批量化

高频写入场景（如归档完成批量更新 ObjectMetadata）使用复合命令减少 Raft 往返：

```rust
pub enum ColdStoreRequest {
    // 单对象操作（低频）
    PutObject(ObjectMetadata),

    // 批量操作（高频，一条 Raft 日志完成多 CF 更新）
    BatchArchiveComplete {
        bundle: ArchiveBundle,
        tape_info_update: TapeInfo,
        object_updates: Vec<ObjectArchiveUpdate>,
    },
    BatchRecallComplete {
        task_updates: Vec<RecallTaskUpdate>,
        object_updates: Vec<ObjectRestoreUpdate>,
    },
}

pub struct ObjectArchiveUpdate {
    pub bucket: String,
    pub key: String,
    pub archive_id: Uuid,
    pub tape_id: String,
    pub tape_set: Vec<String>,
    pub tape_block_offset: u64,
}

pub struct ObjectRestoreUpdate {
    pub bucket: String,
    pub key: String,
    pub status: RestoreStatus,
    pub expire_at: Option<DateTime<Utc>>,
}

pub struct RecallTaskUpdate {
    pub id: Uuid,
    pub status: RestoreStatus,
    pub completed_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}
```

**收益**：一个 ArchiveBundle（500 个对象）只需 1 条 Raft 日志，而非 500+1 条。

#### 4.2.2 读路径优化

| 优化 | 实现 | 收益 |
|------|------|------|
| Follower 读 | GET/HEAD 直接读本地 RocksDB | 读请求水平扩展，不经 Leader |
| 前缀扫描 | `cf_idx_pending` 按时间排序 key | `scan_cold_pending` 无需全表扫描 |
| Column Family 隔离 | 12 个独立 CF | 读写互不干扰，compaction 独立 |
| Block Cache | RocksDB block_cache 配置 | 热对象元数据缓存在内存 |
| Bloom Filter | 每个 CF 配置 Bloom Filter | 减少不存在 key 的磁盘 I/O |

#### 4.2.3 RocksDB 调优参数

```yaml
rocksdb:
  block_cache_size_mb: 256           # 热数据缓存
  bloom_filter_bits: 10              # 每 key 10 bit Bloom Filter
  write_buffer_size_mb: 64           # MemTable 大小
  max_write_buffer_number: 3         # MemTable 数量（写入平滑）
  level0_file_num_compaction_trigger: 4
  max_background_compactions: 4
  max_background_flushes: 2
  compression: "lz4"                 # L1+ 压缩
  bottommost_compression: "zstd"     # 最底层高压缩比
```

### 4.3 缓存层性能（04）

#### 4.3.1 SPDK 用户态 I/O 优势

```
                传统内核路径                      SPDK 用户态路径
App ──▶ syscall ──▶ VFS ──▶ Block Layer      App ──▶ SPDK Blobstore ──▶ NVMe Driver
         │          │           │                         │
     上下文切换   锁竞争     中断处理                  轮询（polling）
     ~2μs       ~1μs        ~5μs                     < 1μs
```

| 指标 | 内核路径 | SPDK 路径 | 提升 |
|------|---------|-----------|------|
| 4KB 随机读延迟 | 10-20μs | 2-5μs | 3-5x |
| 4KB 随机写延迟 | 15-30μs | 3-8μs | 3-5x |
| IOPS (4KB 随机读) | 200-400K | 800K-1.5M | 3-4x |
| CPU 占用/IOPS | 高（中断+上下文切换） | 低（轮询） | 显著降低 |

#### 4.3.2 缓存读写流水线

```
写入流水线（put_restored 批量）：

  Object A ──▶ [create_blob] ──▶ [write_data] ──▶ [update_index]
  Object B ──▶ [create_blob] ──▶ [write_data] ──▶ [update_index]
  Object C ──▶ [create_blob] ──▶ [write_data] ──▶ [update_index]
              ↑                                    ↑
        SPDK 异步并行                        写锁（顺序，极短）
```

`put_restored_batch` 内部对多个对象的 SPDK I/O 并行提交，仅在最后更新索引时短暂串行。

#### 4.3.3 淘汰对性能的影响

| 淘汰策略 | 读路径影响 | 写路径影响 | 适用场景 |
|----------|-----------|-----------|----------|
| LRU | 无（读时更新 `last_access` 用 AtomicI64） | 无 | 通用 |
| LFU | 无（读时原子递增计数器） | 无 | 热点集中 |
| TTL 扫描 | 无（独立后台任务） | 无 | 解冻过期清理 |
| 容量淘汰 | **微弱**（RwLock 写锁删除索引条目） | **微弱** | 容量接近上限时 |

**关键**：淘汰在独立后台 task 中执行，批量删除 `eviction_batch_size` 个 Blob 后统一更新索引，减少写锁次数。

### 4.4 调度层性能（05）

#### 4.4.1 归档写入流水线

```
┌───────────┐  ┌───────────┐  ┌───────────┐  ┌───────────┐
│ Stage 1   │  │ Stage 2   │  │ Stage 3   │  │ Stage 4   │
│ 对象读取  │─▶│ 序列化    │─▶│ 双缓冲   │─▶│ 磁带写入  │
│           │  │ + Header  │  │ 流水线   │  │           │
└───────────┘  └───────────┘  └───────────┘  └───────────┘
     │              │              │              │
  热存储/缓存    ObjectHeader   Buffer A/B    驱动写入
  async read      构建          交替写入      顺序追加
```

**双缓冲机制**：

```rust
pub struct ArchiveWritePipeline {
    buffer_a: DmaBuf,          // 64-128MB
    buffer_b: DmaBuf,          // 64-128MB
    active: AtomicBool,        // true=A, false=B
    block_size: usize,         // 256KB
}
```

| 阶段 | Buffer A | Buffer B |
|------|----------|----------|
| T1 | 填充对象数据 | 磁带写入上一批 |
| T2 | 磁带写入 | 填充对象数据 |
| T3 | 填充对象数据 | 磁带写入 |

**收益**：磁带驱动持续流式写入，无等待间隙，吞吐接近驱动理论上限。

#### 4.4.2 取回合并收益量化

| 场景 | 无合并 | 有合并 | 收益 |
|------|--------|--------|------|
| 10 个对象，同 Bundle | 10 次换带 + 10 次 seek | 1 次换带 + 1 次顺序读 | **10x** |
| 20 个对象，分 3 盘磁带 | 20 次换带 | 3 次换带 + 3 次顺序读 | **6-7x** |
| 100 个对象，同磁带不同 Bundle | 100 次换带 + 100 次 seek | 1 次换带 + N 次 FSF seek | **~50x** |

合并窗口 `merge_window_secs`（默认 60s）是**延迟与吞吐的权衡**：
- 窗口越大：合并率越高，吞吐越好；但首个请求延迟增加
- 窗口越小：响应快，但合并率低，换带频繁

#### 4.4.3 多驱动并行

```
Drive Pool: [Drive 0] [Drive 1] [Drive 2] [Drive 3]
               │          │          │          │
               ▼          ▼          ▼          ▼
          Archive     Archive     Recall     Recall
          Bundle A    Bundle B    Job X      Job Y
          (Tape001)  (Tape002)  (Tape003)  (Tape004)
```

| 配置 | archive_drives | recall_drives | expedited_reserved |
|------|---------------|---------------|-------------------|
| 4 驱动均衡 | 2 | 2 | 1 (从 recall 预留) |
| 写入优先 | 3 | 1 | 0 |
| 取回优先 | 1 | 3 | 1 |
| 动态调整 | 共享池，按优先级竞争 | — | 1 |

### 4.5 磁带层性能（06）

#### 4.5.1 顺序 I/O 最大化

| 优化 | 实现 | 说明 |
|------|------|------|
| 块对齐写入 | 数据填充到 `block_size`（256KB） | 减少驱动内部碎片和缓冲区刷新 |
| 大块写入 | 每次 `write()` 写入完整块 | 避免小 I/O 触发驱动微停顿 |
| FileMark 定位 | `MTFSF` 跳过整个 Bundle | 比逐块 seek 快 100x |
| 块内定位 | `MTFSR` 跳过块 | 在 Bundle 内精确定位对象 |
| 硬件压缩 | LTO 硬件压缩（可配） | 有效容量提升 2-3x，不占 CPU |

#### 4.5.2 换带优化

```
换带流程：
  unload(current_tape)         ~15s  ─┐
  robot move to slot           ~10s   ├─ 总计 30s-120s
  load(new_tape)               ~15s   │
  locate/rewind                ~5s   ─┘
```

**减少换带的策略**（按优先级）：

1. **同磁带合并**：调度层将同 tape_id 的任务合并为一个 TapeReadJob
2. **磁带亲和**：归档调度器优先使用当前已加载磁带的剩余空间
3. **预加载**：若下一个 Job 的 tape_id 已知，提前 unload/load
4. **驱动保持**：读取完成后保持磁带在驱动中一段时间（`tape_hold_secs`），等待同磁带请求

---

## 5. 故障场景矩阵

### 5.1 归档流程故障

| 序号 | 故障点 | 已完成步骤 | 数据状态 | 恢复策略 | 恢复后状态 |
|------|--------|-----------|----------|----------|-----------|
| A1 | 扫描 ColdPending 失败 | 无 | 无副作用 | 下轮扫描自动重试 | 正常 |
| A2 | 驱动分配失败 | 聚合完成 | Bundle 内存态 | 重入队列等待驱动 | 正常 |
| A3 | 磁带写入部分失败 | 磁带有部分数据 | 磁带脏数据（不可回滚） | 写 FileMark 标记废弃，换新位置重写整个 Bundle | 正常 |
| A4 | 磁带写入全部失败 | 无有效写入 | 驱动错误 | 切换驱动/磁带，重试 | 正常 |
| A5 | 元数据更新失败 | 磁带已写入 | 磁带有数据但元数据未更新 | 补偿任务重试元数据写入 | 正常 |
| A6 | 元数据更新超时 | 磁带已写入 | 不确定元数据状态 | 查询确认后决定重试或跳过 | 正常 |
| A7 | 调度器崩溃 | 不确定 | 磁带可能有部分数据 | 重启后扫描补偿日志 + 重扫 ColdPending | 正常 |

### 5.2 取回流程故障

| 序号 | 故障点 | 已完成步骤 | 数据状态 | 恢复策略 | 恢复后状态 |
|------|--------|-----------|----------|----------|-----------|
| R1 | 驱动分配超时 | 无 | RecallTask Pending | 重入队列，超时后 Failed | 用户可重试 |
| R2 | 磁带加载失败 | 驱动已分配 | 磁带物理故障 | 切换副本磁带（tape_set） | 正常 |
| R3 | 磁带 Offline | 驱动已分配 | 磁带不在库中 | WaitingForMedia + 人工通知 | 人工上线后自动继续 |
| R4 | 磁带读取校验失败 | 部分读取 | 数据损坏 | 重试 → 切换副本 → Failed | 取决于副本 |
| R5 | 缓存写入失败 | 磁带读取完成 | 数据在内存中 | 触发淘汰后重试（2 次） | 正常或 Failed |
| R6 | 缓存空间不足 | 磁带读取完成 | 数据在内存中 | 强制淘汰 → 重试 → Failed | 取决于淘汰效果 |
| R7 | 元数据更新失败 | 缓存已写入 | 缓存有数据，元数据未更新 | 补偿任务重试 | GET 暂不可用，补偿后正常 |
| R8 | 调度器崩溃 | 不确定 | 可能有缓存数据 | 重启后扫描 Pending 任务重试 | 正常 |

### 5.3 元数据集群故障

| 序号 | 故障点 | 影响 | 恢复策略 |
|------|--------|------|----------|
| M1 | Follower 宕机 | 读能力降低 | 自动剔除，恢复后追赶日志 |
| M2 | Leader 宕机 | 写入暂停（秒级） | Raft 自动选举新 Leader |
| M3 | 少数派分区 | 无影响（多数派仍可读写） | 分区恢复后自动追赶 |
| M4 | 多数派分区 | 写入不可用 | 等待恢复 / 人工干预 |
| M5 | RocksDB 损坏 | 单节点数据丢失 | 从其他节点 Snapshot 恢复 |
| M6 | 全集群重启 | 服务暂停 | Raft 日志回放恢复状态 |

### 5.4 缓存层故障

| 序号 | 故障点 | 影响 | 恢复策略 |
|------|--------|------|----------|
| C1 | SPDK 进程崩溃 | 缓存不可用 | 重启 SPDK，从 Blobstore xattrs 重建索引 |
| C2 | NVMe 设备故障 | 缓存全部丢失 | 更换设备，所有已解冻对象需重新 Restore |
| C3 | Blob 损坏 | 单个对象不可读 | 删除坏 Blob，返回 500，用户重新 Restore |
| C4 | 索引与 Blob 不一致 | 部分对象查找异常 | 启动时全量扫描 Blobstore 重建索引 |

---

## 6. 可观测性与告警

> 完整的可观测性设计详见 [08-observability.md](./08-observability.md)，包括 OpenTelemetry 集成、Span 拓扑、Metrics 指标体系、结构化日志、仪表板和告警规则。本节仅列出与一致性/性能直接相关的核心告警。

### 6.1 一致性相关核心告警

| 指标 | 条件 | 含义 |
|------|------|------|
| `compensation_pending` | > 0 持续 > 5min | Saga 补偿任务积压，数据一致性风险 |
| `raft_leader_changes` | > 3/hour | Leader 频繁切换，写入可能中断 |
| `raft_propose_latency_ms` P99 | > 100ms | 元数据写入延迟，影响 Saga 完成速度 |
| `recall_tasks_failed` | increase > 0 | 取回失败，可能需人工介入 |
| `archive_bundles_failed` | increase > 0 | 归档失败，对象需回退 ColdPending |

### 6.2 性能相关核心告警

| 指标 | 条件 | 含义 |
|------|------|------|
| `cache_hit_rate` | < 0.7 持续 > 1h | 缓存命中率过低 |
| `archive_throughput_mbps` | < 200 持续 > 30min | 归档吞吐不达标 |
| `recall_e2e_duration{tier="Expedited"}` P99 | > 300s | Expedited SLA 超标 |
| `drive_utilization` avg | > 0.95 持续 > 1h | 驱动饱和 |
| `recall_queue_depth` | > 5000 | 取回队列积压 |

---

## 7. 配置参数汇总

### 7.1 一致性相关

```yaml
consistency:
  raft:
    heartbeat_interval_ms: 200
    election_timeout_ms: 1000
    snapshot_interval: 10000

  compensation:
    scan_interval_secs: 60
    max_retries: 100
    retry_backoff_base_ms: 1000
    retry_backoff_max_ms: 300000
    abandon_after_hours: 72

  restore_state_machine:
    enable_transition_validation: true
    enable_idempotent_writes: true
```

### 7.2 性能相关

```yaml
performance:
  metadata:
    rocksdb_block_cache_mb: 256
    bloom_filter_bits: 10
    enable_batch_propose: true

  cache:
    spdk_reactor_mask: "0x3"          # CPU 核绑定
    io_unit_size: 4096
    eviction_batch_size: 64
    eviction_low_watermark: 0.8

  scheduler:
    archive:
      write_buffer_mb: 128
      block_size: 262144
      min_archive_size_mb: 100
      max_archive_size_mb: 10240
      aggregation_window_secs: 300
      pipeline_depth: 2               # 双缓冲

    recall:
      read_buffer_mb: 64
      merge_window_secs: 60
      max_concurrent_restores: 10

  tape:
    tape_hold_secs: 300               # 读取后保持磁带在驱动中
    drive_acquire_timeout_secs: 600
    expedited_reserved_drives: 1
```

---

## 8. 参考资料

- [Saga Pattern](https://microservices.io/patterns/data/saga.html) — 分布式事务补偿模式
- [SPDK Performance](https://spdk.io/doc/performance_reports.html) — SPDK 性能报告
- [Raft Consensus](https://raft.github.io/) — Raft 共识协议
- [RocksDB Tuning Guide](https://github.com/facebook/rocksdb/wiki/RocksDB-Tuning-Guide) — RocksDB 调优
