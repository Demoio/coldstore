# 归档取回调度层模块设计

> 所属架构：ColdStore 冷存储系统  
> 参考：[总架构设计](../DESIGN.md)

## 1. 模块概述

归档取回调度层负责冷数据的归档与取回调度，是 ColdStore 的核心业务逻辑层。设计需充分考虑**磁带的顺序读写特性**，通过聚合与合并策略最大化吞吐、减少换带与 seek。

> **部署模型**：调度层运行在 **Scheduler Worker** 节点上，是 ColdStore 的**唯一业务中枢**。
> Gateway 的全部 S3 请求都发往 Scheduler Worker 处理。
> Scheduler 通过 gRPC 对接 Cache Worker（同机）和 Tape Worker（远程），
> 通过 gRPC 读写 Metadata 集群。Cache/Tape Worker 不直接接受 Gateway 请求。
>
> **元数据写入协调原则（方案 B）**：Scheduler Worker 是元数据读写的**唯一业务入口**。
> Gateway 不直连 Metadata。缓存层和磁带层不持有 MetadataClient，
> 所有元数据变更由 Scheduler 统一负责。Console 仅用于管控操作（Worker 增删等）。

### 1.1 职责

- **归档调度**：扫描 ColdPending 对象，聚合为 ArchiveBundle，调度磁带顺序写入
- **取回调度**：管理 Restore 队列，按 archive/tape 合并请求，调度磁带顺序读取
- 合并同磁带/同归档包的请求，减少换带与定位
- 控制并发、优先级、超时，保证性能与数据质量

### 1.2 磁带特性约束

| 特性 | 约束 | 调度影响 |
|------|------|----------|
| 顺序写入 | 必须顺序写，不可随机写 | 归档聚合需保证一次写入流 |
| 顺序读取 | 随机读需 seek，成本高 | 取回合并需按物理位置排序 |
| 换带成本 | 机械臂加载/卸载约 30s–2min | 同磁带任务优先合并 |
| 块大小 | 256KB–1MB 常用，LTO 支持 64KB–8MB | 归档块对齐，缓冲匹配 |
| 吞吐 | LTO-9 约 400 MB/s，LTO-10 更高 | 流式写入，避免小 IO |

---

## 2. 归档调度器：写入聚合逻辑

### 2.1 聚合目标

- **适配顺序写入**：一次 ArchiveBundle 对应磁带上一段连续写入
- **最大化吞吐**：聚合足够多对象，避免小 IO、填满驱动流水线
- **块对齐**：与磁带 block_size（如 256KB）对齐，减少驱动内部缓冲碎片

### 2.2 ArchiveBundle 构建策略

#### 2.2.1 聚合维度

| 维度 | 策略 | 说明 |
|------|------|------|
| **大小** | `min_archive_size_mb` ~ `max_archive_size_mb` | 单 Bundle 总大小范围，避免过小（浪费换带）或过大（单任务过长） |
| **对象数** | `batch_size` | 单 Bundle 最大对象数，控制元数据与索引开销 |
| **时间窗口** | `aggregation_window_secs` | 同一窗口内 ColdPending 对象可聚合，避免长时间等待 |
| **桶/前缀** | 可选同 bucket 聚合 | 便于后续按桶管理，非强制 |

#### 2.2.2 聚合算法（伪代码）

```
1. 扫描 ColdPending 对象，按 created_at 或 size 排序
2. 初始化当前 Bundle = []
3. for each object:
     if 加入后 total_size <= max_archive_size_mb AND count <= batch_size:
         Bundle.append(object)
     else:
         提交当前 Bundle 为 ArchiveBundle
         重置 Bundle，将 object 加入
4. 若剩余 Bundle 且 total_size >= min_archive_size_mb: 提交
5. 若剩余 Bundle 且 total_size < min_archive_size_mb: 等待下一轮或超时强制提交
```

#### 2.2.3 磁带上的物理布局

```
┌─────────────────────────────────────────────────────────────────┐
│ 磁带物理布局 (一个 ArchiveBundle)                                 │
├─────────────────────────────────────────────────────────────────┤
│ [FileMark] [Obj1 Header][Obj1 Data] [FileMark] [Obj2 Header]...   │
│     │           │           │          │                         │
│     │           │           │          └─ 对象边界，便于定位      │
│     │           │           └─ 块对齐（256KB）连续写入            │
│     │           └─ 元数据：bucket, key, size, checksum, offset    │
│     └─ 归档包边界，MTFSF/MTBSF 可跳过                            │
└─────────────────────────────────────────────────────────────────┘
```

- **FileMark**：MTIO 文件标记，用于 seek 时按“文件”定位
- **对象块**：固定或可变块，与 `block_size` 对齐
- **索引**：每个对象在 Bundle 内的逻辑偏移写入 Bundle 头部或独立索引区，供取回定位

### 2.3 顺序写入流水线

```
┌──────────┐    ┌──────────┐    ┌──────────┐    ┌──────────┐
│ 对象读取  │───▶│ 块对齐   │───▶│ 双缓冲   │───▶│ 磁带写入  │
│ (热存储)  │    │ 填充     │    │ 流水线   │    │ (顺序)   │
└──────────┘    └──────────┘    └──────────┘    └──────────┘
```

- **块对齐**：不足 block_size 的对象尾部填充（padding），或使用可变块（需驱动支持）
- **双缓冲**：预读下一批数据，写入当前批，保持驱动持续流式写入
- **背压**：若热存储读取慢于磁带写入，可降低并发或增大缓冲

### 2.4 归档流程（细化）

1. **调度层**：从元数据扫描 ColdPending，按策略聚合为 ArchiveBundle
2. **调度层**：选定目标磁带（当前写入头或新磁带）
3. **调度层 → 磁带层**：申请磁带驱动，获取独占
4. **磁带层**：顺序写入：FileMark → [Obj1 Header + Data] → FileMark → [Obj2...]
5. **调度层**：写入完成后计算并存储 Bundle 校验和（可选）
6. **调度层 → 元数据层**：更新 ObjectMetadata（Cold, archive_id, tape_id, tape_block_offset）、ArchiveBundle、TapeInfo
7. **调度层 → 磁带层**：释放驱动；释放在线存储空间

> 步骤 6 由调度层统一负责，磁带层不直接写元数据。

### 2.5 归档关键参数

| 参数 | 推荐值 | 说明 |
|------|--------|------|
| block_size | 262144 (256KB) | 与 LTO 常用块对齐 |
| min_archive_size_mb | 100 | 单 Bundle 最小，避免过小 |
| max_archive_size_mb | 10240 (10GB) | 单 Bundle 最大，控制单任务时长 |
| batch_size | 500–2000 | 单 Bundle 对象数 |
| write_buffer_mb | 64–128 | 写入缓冲，匹配驱动流水线 |
| target_throughput_mbps | 300 | 目标吞吐，LTO-9 可达 400 |

---

## 3. 取回调度器：拉取聚合逻辑

### 3.1 聚合目标

- **减少换带**：同磁带上的多个请求合并为一次加载、一次顺序读取
- **减少 seek**：同 archive 内按物理偏移排序，顺序读取
- **合并同对象请求**：同一对象被多次 Restore 时只读一次，多路分发

### 3.2 取回合并策略

#### 3.2.1 合并层次

```
Level 1: 同 archive_id 的多个对象
         → 一次磁带读取，按 tape_block_offset 排序，顺序读

Level 2: 同 tape_id 的多个 archive
         → 一次换带，按 archive 在磁带上的物理顺序排序

Level 3: 同对象的多个 RecallTask（重复请求）
         → 合并为一个读取，结果分发给多个请求方
```

#### 3.2.2 合并算法（伪代码）

```
1. 从队列取出待调度 RecallTask 集合
2. 按 tape_id 分组
3. for each tape_id:
     tasks = 该磁带上的所有任务
     按 (archive_id, tape_block_offset) 排序，保证顺序读
     若存在同 (bucket, key) 的多个 task，去重，保留一个读取
     生成 TapeReadJob: { tape_id, [(archive_id, [objects])] }
4. 按优先级（Expedited > Standard > Bulk）排序 TapeReadJob
5. 分配可用驱动，执行 TapeReadJob
```

#### 3.2.3 磁带读取顺序

```
磁带物理顺序: Archive1 → Archive2 → Archive3 → ...

取回请求: Obj_A∈Archive2, Obj_B∈Archive1, Obj_C∈Archive2

优化后读取顺序: 
  1. Seek 到 Archive1 起始
  2. 读取 Obj_B
  3. Seek 到 Archive2 起始（或顺序经过则无需 seek）
  4. 顺序读取 Obj_A, Obj_C
```

- 同一 Archive 内对象按 `tape_block_offset` 排序，尽量顺序读
- 跨 Archive 时，按磁带物理顺序排列 Archive，减少回退（MTBSF 成本高于 MTFSF）

### 3.3 取回流程（细化）

1. **协议层 → 调度层**：接收 Restore 请求；协议层查询 ObjectMetadata 获取 `archive_id`、`tape_id`、`tape_block_offset`、`object_size`、`checksum` 等，构造 RecallTask 后提交给调度层入队
2. **调度层**：从队列取任务，按 tape_id + archive_id 合并
3. **调度层 → 磁带层**：检查磁带状态，ONLINE → 继续；OFFLINE → 通知并等待
4. **调度层 → 磁带层**：申请驱动，加载磁带（若未加载）
5. **磁带层**：按物理顺序执行读取：Seek → Read → 返回数据
6. **调度层 → 缓存层**：`cache.put_restored(bucket, key, data, checksum, expire_at)`
7. **调度层 → 元数据层**：更新 restore_status=Completed，restore_expire_at
8. **调度层 → 磁带层**：释放驱动

> 步骤 6-7 体现方案 B 的核心原则：调度层先写缓存，再更新元数据。缓存层和磁带层均不持有 MetadataClient。

### 3.4 取回关键参数

| 参数 | 推荐值 | 说明 |
|------|--------|------|
| queue_size | 10000 | 队列容量 |
| max_concurrent_restores | 驱动数 | 每驱动一个取回流水线 |
| restore_timeout_secs | 3600 | 单任务超时 |
| min_restore_interval_secs | 300 | 最小取回间隔（5 分钟 SLA） |
| read_buffer_mb | 64 | 读取缓冲 |
| merge_window_secs | 60 | 合并窗口，窗口内同磁带任务可合并 |

---

## 4. 调度机制

### 4.1 驱动分配与竞争

| 资源 | 策略 |
|------|------|
| 磁带驱动 | 归档与取回共享驱动池，可配置比例（如 2:1 或独立池） |
| 优先级 | 取回 Expedited > 归档 > 取回 Standard > 取回 Bulk |
| 抢占 | 一般不抢占，当前任务完成后按优先级分配 |
| 预留 | Expedited 可预留 1 个驱动，保证高优先级 |

### 4.2 队列模型

```
                    ┌─────────────────────────────────┐
                    │      Recall 优先级队列            │
                    │  Expedited | Standard | Bulk    │
                    └─────────────────────────────────┘
                                        │
                    ┌───────────────────┼───────────────────┐
                    ▼                   ▼                   ▼
            ┌───────────────┐   ┌───────────────┐   ┌───────────────┐
            │ 合并器         │   │ 合并器         │   │ 合并器         │
            │ (按 tape 聚合) │   │ (按 tape 聚合) │   │ (按 tape 聚合) │
            └───────┬───────┘   └───────┬───────┘   └───────┬───────┘
                    │                   │                   │
                    └───────────────────┼───────────────────┘
                                        ▼
                    ┌─────────────────────────────────┐
                    │      驱动调度器                   │
                    │  Drive 1 | Drive 2 | Drive 3    │
                    └─────────────────────────────────┘
```

### 4.3 归档调度周期

- **扫描周期**：`scan_interval_secs`（如 3600s）
- **触发条件**：定时 + 可选事件触发（ColdPending 积累达阈值）
- **并发**：多驱动时可并行执行多个 ArchiveTask，每个 Task 独占一驱动

---

## 5. 性能设计

### 5.1 吞吐目标

| 场景 | 目标 | 实现要点 |
|------|------|----------|
| 归档写入 | ≥ 300 MB/s | 块对齐、双缓冲、流式写入 |
| 取回 | ≥ 1 任务/5 分钟 | 合并减少换带，多驱动并行 |

### 5.2 性能优化

| 手段 | 说明 |
|------|------|
| 块对齐 | 256KB 块，减少驱动内部碎片 |
| 大缓冲 | 64–128MB 写入/读取缓冲 |
| 预读 | 归档时预读下一批对象 |
| 合并 | 取回时同磁带、同 archive 合并 |
| 并行 | 多驱动时归档与取回可并行 |

### 5.3 背压与限流

- 归档：若热存储读取慢，可暂停新 Bundle 提交
- 取回：队列满时拒绝新 Restore，返回 503
- Expedited：可配置最大并发，超限返回 GlacierExpeditedRetrievalNotAvailable

---

## 6. 质量与可靠性

### 6.1 数据完整性

| 机制 | 说明 |
|------|------|
| 对象校验和 | 归档时写入 CRC32/SHA256，取回时校验 |
| Bundle 校验和 | 可选，整 Bundle 校验 |
| 写入后验证 | 可选，归档完成后读回校验（成本高） |

### 6.2 失败与重试

| 场景 | 策略 |
|------|------|
| 归档写入失败 | 重试 3 次，仍失败则标记 Bundle Failed，对象回退 ColdPending |
| 取回读取失败 | 重试 2 次，仍失败则 RecallTask Failed，通知用户 |
| 驱动故障 | 切换备用驱动，重新加载磁带 |
| 磁带不可读 | 若有副本，切换副本；否则通知人工 |

### 6.3 一致性（调度层统一协调）

调度层作为元数据写入的唯一协调者，需保证以下写入顺序：

**归档**：

| 顺序 | 操作 | 失败处理 |
|------|------|----------|
| 1 | 磁带层顺序写入完成 | 重试/换带 |
| 2 | **调度层 → 元数据层**：更新 ObjectMetadata、ArchiveBundle、TapeInfo | 重试/补偿任务 |

**取回**：

| 顺序 | 操作 | 失败处理 |
|------|------|----------|
| 1 | 磁带层读取完成 | 重试/切换副本 |
| 2 | **调度层 → 缓存层**：put_restored | 重试/标记 Failed |
| 3 | **调度层 → 元数据层**：restore_status=Completed | 重试；若持续失败，缓存有数据但 GET 不可用，需补偿 |

> 缓存层和磁带层均为被编排方，不主动写元数据。

---

## 7. 离线磁带处理

- 磁带状态：ONLINE / OFFLINE / UNKNOWN
- 取回命中 OFFLINE 磁带：
  1. 生成通知事件（tape_id、槽位、archive_id、请求方）
  2. 任务进入等待队列，不占用驱动
  3. 人工加载磁带并确认 ONLINE 后，调度器自动重试
- 合并：多个请求命中同一离线磁带时，合并为一次通知，加载后批量处理

---

## 8. 核心数据结构

### 8.1 ArchiveScheduler

```rust
pub struct ArchiveScheduler {
    metadata: Arc<MetadataClient>,
    tape_manager: Arc<TapeManager>,
    config: ArchiveConfig,
    running: AtomicBool,
}
```

| 字段 | 类型 | 含义 | 说明 |
|------|------|------|------|
| `metadata` | Arc\<MetadataClient\> | 元数据客户端 | 扫描 ColdPending、写入 ArchiveBundle、更新 ObjectMetadata |
| `tape_manager` | Arc\<TapeManager\> | 磁带管理器 | 申请驱动、选磁带、顺序写入 |
| `config` | ArchiveConfig | 归档配置 | 聚合参数、块大小、缓冲等 |
| `running` | AtomicBool | 运行状态标记 | 优雅停止扫描循环 |

### 8.2 RecallScheduler

```rust
pub struct RecallScheduler {
    metadata: Arc<MetadataClient>,
    tape_manager: Arc<TapeManager>,
    cache: Arc<dyn CacheWriteApi>,
    queue: RecallQueue,
    config: RecallConfig,
}
```

| 字段 | 类型 | 含义 | 说明 |
|------|------|------|------|
| `metadata` | Arc\<MetadataClient\> | 元数据客户端 | 读取 RecallTask、更新 restore_status |
| `tape_manager` | Arc\<TapeManager\> | 磁带管理器 | 申请驱动、加载磁带、定位读取 |
| `cache` | Arc\<dyn CacheWriteApi\> | 缓存写接口 | put_restored 写入解冻数据 |
| `queue` | RecallQueue | 取回优先级队列 | 三级优先级 + 合并窗口 |
| `config` | RecallConfig | 取回配置 | 队列大小、超时、合并窗口等 |

### 8.3 ArchiveBundle（归档包）

一批对象在磁带上的连续写入单元，是归档调度的基本粒度。

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

| 字段 | 类型 | 含义 | 用途 |
|------|------|------|------|
| `id` | Uuid | 归档包唯一标识 | 全局唯一，元数据 CF `bundles` 的 key |
| `tape_id` | String | 主副本所在磁带 ID | 取回时定位磁带 |
| `tape_set` | Vec\<String\> | 所有副本磁带 ID 列表 | 双副本场景：`["TAPE001", "TAPE002"]` |
| `entries` | Vec\<BundleEntry\> | 包内对象列表（含偏移信息） | 取回时按 offset 定位单个对象 |
| `total_size` | u64 | 包内所有对象总字节 | 选磁带时判断剩余空间 |
| `filemark_start` | u32 | Bundle 起始 FileMark 编号 | 磁带 seek 时 `MTFSF(filemark_start)` 快速定位 |
| `filemark_end` | u32 | Bundle 结束 FileMark 编号 | 标记 Bundle 边界 |
| `checksum` | Option\<String\> | 整个 Bundle 的 SHA256（可选） | 写入后可选验证 |
| `status` | ArchiveBundleStatus | 状态 | `Pending` → `Writing` → `Completed` / `Failed` |
| `created_at` | DateTime\<Utc\> | 创建时间 | 聚合窗口计算 |
| `completed_at` | Option\<DateTime\<Utc\>\> | 完成时间 | 审计与可观测性 |

### 8.4 BundleEntry（Bundle 内单个对象条目）

描述一个对象在 Bundle 内的物理位置，用于取回时精确定位。

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
```

| 字段 | 类型 | 含义 | 用途 |
|------|------|------|------|
| `bucket` | String | S3 桶名 | 与 key 唯一标识对象 |
| `key` | String | S3 对象键 | 取回后写缓存、更新元数据时需要 |
| `version_id` | Option\<String\> | 对象版本 ID | 多版本场景 |
| `size` | u64 | 对象原始字节数 | 读取时分配缓冲 |
| `offset_in_bundle` | u64 | 对象在 Bundle 内的字节偏移 | 取回时计算磁带读取偏移 |
| `tape_block_offset` | u64 | 对象在磁带上的块偏移（相对 FileMark） | `MTFSR(tape_block_offset)` 精确定位 |
| `checksum` | String | SHA256 hex | 取回时校验数据完整性 |

### 8.5 ObjectHeader（磁带上对象头）

写入磁带时，每个对象前写入 Header，采用**定长前缀 + 变长 bucket/key** 的混合格式。解析时先读定长部分获取 `bucket_len`/`key_len`，再按长度读取变长字符串。

> ObjectHeader 与 BundleEntry 存在冗余：BundleEntry 在元数据中记录同样信息。ObjectHeader 的作用是**磁带端自描述**——用于边界检测（magic）、取回时校验（checksum）、以及元数据丢失后的数据恢复。

```rust
pub struct ObjectHeader {
    pub magic: [u8; 4],
    pub version: u8,
    pub bucket_len: u16,
    pub bucket: String,
    pub key_len: u16,
    pub key: String,
    pub size: u64,
    pub checksum: [u8; 32],
    pub flags: u8,
    pub reserved: [u8; 16],
}
```

| 字段 | 类型 | 含义 | 用途 |
|------|------|------|------|
| `magic` | [u8; 4] | 魔数 `0x43 0x53 0x4F 0x48`（"CSOH"） | 标识 Header 起始，校验格式正确 |
| `version` | u8 | Header 格式版本号 | 向前兼容，当前 `1` |
| `bucket_len` | u16 | bucket 字符串字节长度 | 解析定位 |
| `bucket` | String | S3 桶名 | 取回时还原对象归属 |
| `key_len` | u16 | key 字符串字节长度 | 解析定位 |
| `key` | String | S3 对象键 | 取回时还原对象标识 |
| `size` | u64 | 对象数据字节数 | 取回时读取确切长度 |
| `checksum` | [u8; 32] | SHA256 原始字节 | 取回时逐字节校验 |
| `flags` | u8 | 标志位（bit 0: 压缩, bit 1: 加密） | 预留，当前全 0 |
| `reserved` | [u8; 16] | 保留字段 | 未来扩展，写入时全 0 |

### 8.6 ArchiveTask（归档任务）

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
```

| 字段 | 类型 | 含义 | 用途 |
|------|------|------|------|
| `id` | Uuid | 任务唯一标识 | 元数据 CF `archive_tasks` 的 key |
| `bundle_id` | Uuid | 关联的 ArchiveBundle ID | 追踪任务与 Bundle 的关系 |
| `tape_id` | String | 目标磁带 ID | 记录写入的磁带 |
| `drive_id` | Option\<String\> | 分配的驱动 ID | 执行中才有值 |
| `object_count` | u32 | 本次归档对象数 | 进度监控 |
| `total_size` | u64 | 总字节数 | 进度百分比 = bytes_written / total_size |
| `bytes_written` | u64 | 已写入字节数 | 进度追踪、断点续传参考 |
| `status` | ArchiveTaskStatus | 任务状态 | `Pending` → `InProgress` → `Completed` / `Failed` |
| `retry_count` | u32 | 已重试次数 | 超过 max_retries 则标记 Failed |
| `created_at` | DateTime\<Utc\> | 任务创建时间 | 审计 |
| `started_at` | Option\<DateTime\<Utc\>\> | 开始执行时间 | 获取驱动后设置 |
| `completed_at` | Option\<DateTime\<Utc\>\> | 完成时间 | 计算耗时 |
| `error` | Option\<String\> | 错误信息 | Failed 时记录原因 |

### 8.7 RecallTask（取回任务）

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

| 字段 | 类型 | 含义 | 用途 |
|------|------|------|------|
| `id` | Uuid | 任务唯一标识 | 元数据 CF `recall_tasks` 的 key |
| `bucket` | String | S3 桶名 | 取回后写缓存、更新元数据 |
| `key` | String | S3 对象键 | 与 bucket 唯一标识对象 |
| `version_id` | Option\<String\> | 对象版本 | 多版本场景 |
| `archive_id` | Uuid | 所属 ArchiveBundle ID | 定位 BundleEntry 获取磁带偏移 |
| `tape_id` | String | 主副本磁带 ID | 默认从此磁带读取 |
| `tape_set` | Vec\<String\> | 所有副本磁带列表 | 主磁带不可读时切换副本 |
| `tape_block_offset` | u64 | 对象在磁带上的块偏移 | seek 定位 |
| `object_size` | u64 | 对象字节数 | 读取时分配缓冲 |
| `checksum` | String | SHA256 hex | 读取后校验完整性，写缓存时传入 |
| `tier` | RestoreTier | 取回优先级 | `Expedited` / `Standard` / `Bulk`，影响队列排序 |
| `days` | u32 | 解冻保留天数 | 计算 expire_at = now + days |
| `expire_at` | DateTime\<Utc\> | 解冻过期时间 | 写入缓存 xattr 和元数据 restore_expire_at |
| `status` | RestoreStatus | 任务状态 | `Pending` → `InProgress` → `Completed` / `Failed` |
| `drive_id` | Option\<String\> | 分配的驱动 ID | 执行中才有值 |
| `retry_count` | u32 | 已重试次数 | 超限则标记 Failed |
| `created_at` | DateTime\<Utc\> | 任务创建时间 | 排队顺序参考 |
| `started_at` | Option\<DateTime\<Utc\>\> | 开始执行时间 | 计算 SLA |
| `completed_at` | Option\<DateTime\<Utc\>\> | 完成时间 | 计算耗时 |
| `error` | Option\<String\> | 错误信息 | 失败原因 |

### 8.8 RestoreTier（取回优先级）

```rust
pub enum RestoreTier {
    Expedited,
    Standard,
    Bulk,
}
```

| 值 | 含义 | 对应 SLA | 队列优先级 |
|------|------|----------|-----------|
| `Expedited` | 加急 | 1–5 分钟 | 最高，可预留驱动 |
| `Standard` | 标准 | 3–5 小时 | 中 |
| `Bulk` | 批量 | 5–12 小时 | 最低 |

### 8.9 TapeReadJob（磁带读取作业）

取回合并后的执行单元，一个 TapeReadJob = 一次换带 + 一次顺序读取。

```rust
pub struct TapeReadJob {
    pub job_id: Uuid,
    pub tape_id: String,
    pub drive_id: Option<String>,
    pub segments: Vec<ReadSegment>,
    pub total_objects: u32,
    pub total_bytes: u64,
    pub priority: RestoreTier,
}
```

| 字段 | 类型 | 含义 | 用途 |
|------|------|------|------|
| `job_id` | Uuid | 作业唯一标识 | 日志追踪 |
| `tape_id` | String | 目标磁带 ID | 加载到驱动 |
| `drive_id` | Option\<String\> | 分配的驱动 | 执行时填入 |
| `segments` | Vec\<ReadSegment\> | 按物理顺序排列的读取段列表 | 顺序执行，避免回退 seek |
| `total_objects` | u32 | 本次作业对象总数 | 监控 |
| `total_bytes` | u64 | 总字节数 | 监控 |
| `priority` | RestoreTier | 作业优先级（取组内最高） | 驱动分配排序 |

### 8.10 ReadSegment（读取段）

```rust
pub struct ReadSegment {
    pub archive_id: Uuid,
    pub filemark: u32,
    pub objects: Vec<ReadObject>,
}
```

| 字段 | 类型 | 含义 | 用途 |
|------|------|------|------|
| `archive_id` | Uuid | 所属 ArchiveBundle ID | 日志追踪 |
| `filemark` | u32 | 该 Bundle 的起始 FileMark（来自 `ArchiveBundle.filemark_start`） | `MTFSF(filemark)` 定位 |
| `objects` | Vec\<ReadObject\> | 段内对象列表，按 tape_block_offset 升序 | 顺序读取 |

### 8.11 ReadObject（待读取对象）

```rust
pub struct ReadObject {
    pub recall_task_id: Uuid,
    pub bucket: String,
    pub key: String,
    pub tape_block_offset: u64,
    pub size: u64,
    pub checksum: String,
    pub expire_at: DateTime<Utc>,
}
```

| 字段 | 类型 | 含义 | 用途 |
|------|------|------|------|
| `recall_task_id` | Uuid | 关联的 RecallTask ID | 完成后更新任务状态 |
| `bucket` | String | S3 桶名 | 写缓存时传入 |
| `key` | String | S3 对象键 | 写缓存时传入 |
| `tape_block_offset` | u64 | 磁带上块偏移 | `MTFSR(offset)` 精确定位 |
| `size` | u64 | 对象字节数 | 分配读取缓冲 |
| `checksum` | String | SHA256 hex | 读取后校验 |
| `expire_at` | DateTime\<Utc\> | 解冻过期时间 | 写入缓存 xattr |

### 8.12 TapeWriteJob（磁带写入作业）

```rust
pub struct TapeWriteJob {
    pub job_id: Uuid,
    pub bundle: ArchiveBundle,
    pub tape_id: String,
    pub drive_id: Option<String>,
    pub bytes_written: u64,
    pub objects_written: u32,
}
```

| 字段 | 类型 | 含义 | 用途 |
|------|------|------|------|
| `job_id` | Uuid | 作业唯一标识 | 日志追踪 |
| `bundle` | ArchiveBundle | 待写入的归档包 | 含完整对象列表和元数据 |
| `tape_id` | String | 目标磁带 | 写入到此磁带 |
| `drive_id` | Option\<String\> | 分配的驱动 | 执行时填入 |
| `bytes_written` | u64 | 已写字节 | 进度追踪 |
| `objects_written` | u32 | 已写对象数 | 进度追踪 |

### 8.13 RecallQueue（取回优先级队列）

```rust
pub struct RecallQueue {
    expedited: VecDeque<RecallTask>,
    standard: VecDeque<RecallTask>,
    bulk: VecDeque<RecallTask>,
    pending_by_tape: HashMap<String, Vec<Uuid>>,
    capacity: usize,
}
```

| 字段 | 类型 | 含义 | 用途 |
|------|------|------|------|
| `expedited` | VecDeque\<RecallTask\> | 加急队列 | 最高优先级，优先出队 |
| `standard` | VecDeque\<RecallTask\> | 标准队列 | 中等优先级 |
| `bulk` | VecDeque\<RecallTask\> | 批量队列 | 最低优先级 |
| `pending_by_tape` | HashMap\<String, Vec\<Uuid\>\> | tape_id → 待处理 task_id 列表 | 合并时快速按磁带分组 |
| `capacity` | usize | 队列总容量上限 | 超限拒绝新任务 |

**pending_by_tape 维护规则**：
- **入队**：将 task_id 加入 `pending_by_tape[tape_id]`
- **出队/合并**：合并时从 `pending_by_tape` 按 tape 分组取出任务，结合优先级队列做排序
- **完成/取消**：任务完成或取消后，从 `pending_by_tape` 中移除对应 task_id

### 8.14 枚举汇总

```rust
pub enum ArchiveBundleStatus {
    Pending,     // 聚合完成，等待写入
    Writing,     // 正在写入磁带
    Completed,   // 写入成功
    Failed,      // 写入失败
}

pub enum ArchiveTaskStatus {
    Pending,     // 等待驱动
    InProgress,  // 执行中
    Completed,   // 完成
    Failed,      // 失败
}

pub enum RestoreStatus {
    Pending,          // 已入队，等待调度
    WaitingForMedia,  // 磁带离线，等待人工上线
    InProgress,       // 正在从磁带读取
    Completed,        // 已写入缓存+元数据
    Expired,          // 解冻过期（由定时任务或 GET 时检查 restore_expire_at 触发）
    Failed,           // 失败
}
```

---

## 9. 模块结构

```
src/scheduler/
├── mod.rs
├── archive/
│   ├── mod.rs
│   ├── aggregator.rs    # 归档聚合逻辑
│   ├── writer.rs        # 顺序写入流水线
│   └── task.rs          # ArchiveTask 执行
├── recall/
│   ├── mod.rs
│   ├── merger.rs        # 取回合并逻辑
│   ├── reader.rs        # 顺序读取流水线
│   └── task.rs          # RecallTask 执行
├── queue.rs             # 优先级队列
├── drive_allocator.rs   # 驱动分配
└── types.rs             # TapeReadJob, ArchiveBundle 等
```

---

## 10. 配置项

```yaml
scheduler:
  archive:
    scan_interval_secs: 3600
    batch_size: 1000
    min_archive_size_mb: 100
    max_archive_size_mb: 10240
    aggregation_window_secs: 3600
    block_size: 262144
    write_buffer_mb: 64
    target_throughput_mbps: 300
  recall:
    queue_size: 10000
    max_concurrent_restores: 10
    restore_timeout_secs: 3600
    min_restore_interval_secs: 300
    merge_window_secs: 60
    read_buffer_mb: 64
    expedited_reserved_drives: 1
  drive:
    total_drives: 3
    archive_drives: 2
    recall_drives: 1
```

---

## 11. 依赖关系

**调度层主动依赖**：

| 依赖 | 方向 | 协议 | 说明 |
|------|------|------|------|
| 元数据层 | Scheduler → Metadata | gRPC | **唯一的元数据业务读写入口** |
| 缓存层 | Scheduler → Cache Worker | gRPC（同机） | 数据暂存/读取 |
| 磁带层 | Scheduler → Tape Worker | gRPC（远程） | 磁带读写指令下发 |

**被依赖**：

| 被依赖方 | 协议 | 说明 |
|----------|------|------|
| Gateway | gRPC | **全部** S3 请求（PUT/GET/HEAD/DELETE/Restore/List） |

> **架构要点**：Scheduler Worker 是唯一的业务中枢，同时持有 MetadataClient、
> CacheClient(gRPC)、TapeClient(gRPC) 三个依赖。
> Gateway 所有请求都发往 Scheduler，Gateway 不直连 Metadata/Cache/Tape。
> 缓存层和磁带层均为纯功能组件，不感知元数据。

---

## 12. 实施要点

- 归档与取回可独立扩缩容
- 每个磁带驱动对应一条顺序 I/O 流水线，避免多任务共享同一驱动导致 seek
- 增加驱动可线性提升并发归档与取回能力
- 元数据需记录 `tape_block_offset`，供取回时按物理顺序排序
- **调度层同时注入 MetadataClient + CacheWriteApi + TapeManager**，是系统中唯一的编排者
