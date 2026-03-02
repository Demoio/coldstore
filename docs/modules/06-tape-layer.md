# 磁带管理层模块设计

> 所属架构：ColdStore 冷存储系统  
> 参考：[总架构设计](../DESIGN.md)

## 1. 模块概述

磁带管理层提供磁带设备与磁带库的完整管理能力，通过自研 SDK 抽象层实现与底层硬件的解耦，前期对接 Linux SCSI 协议。

### 1.1 完整职责

| 功能域 | 说明 |
|--------|------|
| 磁带驱动管理 | 驱动发现、状态监控、独占分配、释放 |
| 磁带介质管理 | 介质注册、容量跟踪、格式化、可读性校验、生命周期 |
| 磁带库管理 | 槽位管理、机械臂加载/卸载、盘点、条码扫描 |
| 数据读写 | 顺序写入、顺序/定位读取、FileMark 管理 |
| 离线磁带协同 | 状态跟踪、通知触发、人工确认流程 |
| 可靠性 | 双副本、校验、错误处理、坏带替换 |
| 可观测性 | 驱动利用率、读写吞吐、错误率、换带次数 |

### 1.2 在架构中的位置

```
归档/取回调度器
        │
        ▼
┌─────────────────────────────────────────────────────────────────────┐
│                         磁带管理层                                    │
│  ┌────────────────────────────────────────────────────────────────┐  │
│  │ TapeManager (上层 API，面向调度器)                               │  │
│  │ - 驱动分配、磁带选择、介质跟踪、通知                             │  │
│  └────────────────────────────────────────────────────────────────┘  │
│                           │                                           │
│  ┌────────────────────────┼────────────────────────┐                 │
│  │                        │                        │                 │
│  ▼                        ▼                        ▼                 │
│  ┌──────────┐    ┌───────────────┐    ┌────────────────────┐       │
│  │ TapeDrive│    │ TapeMedia     │    │ TapeLibrary        │       │
│  │ trait    │    │ trait         │    │ trait              │       │
│  │ (驱动)   │    │ (介质)        │    │ (带库/机械臂)      │       │
│  └──────────┘    └───────────────┘    └────────────────────┘       │
│       │                │                       │                    │
│  ┌────┴────────────────┴───────────────────────┴────┐               │
│  │              具体实现层                            │               │
│  │  ScsiTapeDrive | ScsiTapeMedia | ScsiTapeLibrary  │               │
│  │  (前期 Linux SCSI)                                │               │
│  └──────────────────────────────────────────────────┘               │
│       │                                                              │
│  ┌────┴─────────────────────────────────────────────┐               │
│  │              底层封装                              │               │
│  │  mtio.rs (MTIO ioctl) | sg.rs (SCSI Generic)     │               │
│  └──────────────────────────────────────────────────┘               │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 2. 代码架构分层

### 2.1 分层原则

| 层级 | 职责 | 稳定性 |
|------|------|--------|
| **TapeManager** | 面向调度器的业务 API，编排驱动/介质/库 | 最稳定，不随硬件变化 |
| **SDK 抽象层** (trait) | 定义 TapeDrive / TapeMedia / TapeLibrary 接口 | 稳定，接口不频繁变更 |
| **实现层** | 具体硬件对接（SCSI、厂商 SDK） | 可替换，不影响上层 |
| **底层封装** | ioctl / SCSI 命令封装 | 最底层，平台相关 |

### 2.2 扩展策略

- 新硬件后端：实现 TapeDrive / TapeMedia / TapeLibrary trait 即可
- 上层调度器：仅依赖 TapeManager API，不感知底层实现
- 配置切换：通过 `tape.sdk.backend` 配置选择实现

---

## 3. SDK 抽象层：trait 定义

### 3.1 TapeDrive trait（磁带驱动）

驱动是数据读写的执行单元，一次绑定一盘磁带。

```rust
/// 磁带驱动抽象 — 数据 I/O 的执行单元
#[async_trait]
pub trait TapeDrive: Send + Sync {
    // ── 标识 ──
    fn drive_id(&self) -> &str;
    fn device_path(&self) -> &str;

    // ── 数据读写 ──
    async fn write_blocks(&self, data: &[u8]) -> Result<u64>;
    async fn read_blocks(&self, buf: &mut [u8]) -> Result<u64>;

    // ── 定位 ──
    async fn seek_to_filemark(&self, count: i32) -> Result<()>;
    async fn seek_to_record(&self, count: i32) -> Result<()>;
    async fn seek_to_end_of_data(&self) -> Result<()>;
    async fn rewind(&self) -> Result<()>;

    // ── FileMark ──
    async fn write_filemark(&self, count: u32) -> Result<()>;

    // ── 状态 ──
    async fn status(&self) -> Result<DriveStatus>;
    async fn position(&self) -> Result<TapePosition>;
    async fn is_ready(&self) -> Result<bool>;

    // ── 控制 ──
    async fn eject(&self) -> Result<()>;
    async fn set_block_size(&self, size: u32) -> Result<()>;
    async fn set_compression(&self, enabled: bool) -> Result<()>;
}
```

### 3.2 TapeMedia trait（磁带介质）

介质管理覆盖单盘磁带的生命周期。

```rust
/// 磁带介质抽象 — 单盘磁带的元数据与生命周期
#[async_trait]
pub trait TapeMedia: Send + Sync {
    // ── 标识 ──
    fn media_id(&self) -> &str;
    fn barcode(&self) -> Option<&str>;
    fn format(&self) -> &str;  // "LTO-9", "LTO-10"

    // ── 容量 ──
    async fn capacity(&self) -> Result<MediaCapacity>;
    async fn remaining(&self) -> Result<u64>;
    async fn is_full(&self) -> Result<bool>;

    // ── 状态 ──
    fn status(&self) -> MediaStatus;
    fn set_status(&mut self, status: MediaStatus);
    fn location(&self) -> MediaLocation;
    fn set_location(&mut self, location: MediaLocation);

    // ── 生命周期 ──
    async fn verify_readable(&self, drive: &dyn TapeDrive) -> Result<VerifyResult>;
    async fn erase(&self, drive: &dyn TapeDrive) -> Result<()>;

    // ── 元数据 ──
    fn label(&self) -> Option<&str>;
    async fn write_label(&self, drive: &dyn TapeDrive, label: &str) -> Result<()>;
    fn archive_bundles(&self) -> &[Uuid];
    fn add_archive_bundle(&mut self, bundle_id: Uuid);
}
```

### 3.3 TapeLibrary trait（磁带库）

带库管理覆盖机械臂、槽位、盘点等自动化操作。

```rust
/// 磁带库抽象 — 含机械臂的自动化磁带库
#[async_trait]
pub trait TapeLibrary: Send + Sync {
    fn library_id(&self) -> &str;

    // ── 槽位管理 ──
    async fn list_slots(&self) -> Result<Vec<SlotInfo>>;
    async fn list_drives(&self) -> Result<Vec<DriveSlotInfo>>;

    // ── 机械臂操作 ──
    // drive_element 为 SCSI Data Transfer Element Address（u32）
    // TapeManager 负责将字符串 drive_id 映射为 SCSI element address
    async fn load(&self, slot_id: u32, drive_element: u32) -> Result<()>;
    async fn unload(&self, drive_element: u32, slot_id: u32) -> Result<()>;
    async fn transfer(&self, from_slot: u32, to_slot: u32) -> Result<()>;

    // ── 盘点 ──
    async fn inventory(&self) -> Result<LibraryInventory>;

    // ── 导入/导出 ──
    async fn list_import_export_slots(&self) -> Result<Vec<SlotInfo>>;

    // ── 状态 ──
    async fn status(&self) -> Result<LibraryStatus>;
}
```

### 3.4 类型定义与字段说明

#### 3.4.1 DriveStatus（驱动状态）

```rust
pub struct DriveStatus {
    pub drive_id: String,
    pub is_ready: bool,
    pub has_media: bool,
    pub media_id: Option<String>,
    pub block_size: u32,
    pub compression: bool,
    pub write_protected: bool,
    pub error: Option<DriveError>,
}
```

| 字段 | 类型 | 含义 | 来源 |
|------|------|------|------|
| `drive_id` | String | 驱动唯一标识 | 配置文件定义 |
| `is_ready` | bool | 驱动是否就绪可执行 I/O | MTIOCGET 解析 |
| `has_media` | bool | 是否已装载磁带 | MTIOCGET `GMT_DR_OPEN` 取反 |
| `media_id` | Option\<String\> | 当前装载的磁带 ID | MediaRegistry 关联 |
| `block_size` | u32 | 当前块大小（字节） | MTIOCGET `mt_dsreg` 低 24 位 |
| `compression` | bool | 硬件压缩是否开启 | MTIOCGET 驱动标志 |
| `write_protected` | bool | 磁带是否写保护 | MTIOCGET `GMT_WR_PROT` |
| `error` | Option\<DriveError\> | 当前错误（无错误为 None） | 异常检测后设置 |

#### 3.4.2 TapePosition（磁带位置）

```rust
pub struct TapePosition {
    pub filemark_number: u32,
    pub block_number: u64,
    pub at_bot: bool,
    pub at_eod: bool,
    pub at_filemark: bool,
}
```

| 字段 | 类型 | 含义 | 来源 |
|------|------|------|------|
| `filemark_number` | u32 | 当前所在 FileMark 编号（0-based） | MTIOCPOS 或软件计数 |
| `block_number` | u64 | 当前块编号 | MTIOCPOS `mt_blkno` |
| `at_bot` | bool | 是否在磁带起始（Beginning of Tape） | MTIOCGET `GMT_BOT` |
| `at_eod` | bool | 是否在数据末尾（End of Data） | MTIOCGET `GMT_EOD` |
| `at_filemark` | bool | 是否刚经过 FileMark | MTIOCGET `GMT_EOF` |

#### 3.4.3 MediaCapacity（介质容量）

```rust
pub struct MediaCapacity {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub remaining_bytes: u64,
}
```

| 字段 | 类型 | 含义 | 来源 |
|------|------|------|------|
| `total_bytes` | u64 | 磁带总容量（字节） | 介质规格（LTO-9: 18TB 原始） |
| `used_bytes` | u64 | 已使用字节 | MediaRegistry 累计写入跟踪 |
| `remaining_bytes` | u64 | 剩余可用字节 | `total_bytes - used_bytes` |

#### 3.4.4 MediaStatus（介质状态）

```rust
pub enum MediaStatus {
    Online,
    Offline,
    InDrive(String),
    ImportExport,
    Error(String),
    Retired,
}
```

| 值 | 含义 | 触发条件 |
|------|------|----------|
| `Online` | 在库中，空闲可用 | 盘点确认在槽位中 |
| `Offline` | 离线存放，不在库中 | 盘点未发现或人工标记 |
| `InDrive(drive_id)` | 已加载到指定驱动中 | `load()` 成功后设置 |
| `ImportExport` | 在导入/导出槽位 | 盘点发现在 I/E 槽位 |
| `Error(msg)` | 异常状态 | 读写错误、校验失败 |
| `Retired` | 已退役 | 不可读块过多或超龄 |

#### 3.4.5 MediaLocation（介质位置）

```rust
pub enum MediaLocation {
    Slot(u32),
    Drive(String),
    Offsite(String),
    Unknown,
}
```

| 值 | 含义 | 用途 |
|------|------|------|
| `Slot(slot_id)` | 在库中第 N 号槽位 | `load(slot_id, drive_id)` 时定位 |
| `Drive(drive_id)` | 在某驱动中 | 已加载，可直接读写 |
| `Offsite(location)` | 离线存放位置描述 | 人工找回时参考，如 "机房B-柜3-层2" |
| `Unknown` | 未知 | 初始状态或盘点失败 |

#### 3.4.6 DriveError（驱动错误）

```rust
pub enum DriveError {
    MediaNotLoaded,
    WriteProtected,
    HardwareError(String),
    MediaError(String),
    CleaningRequired,
}
```

| 值 | 含义 | 处理策略 |
|------|------|----------|
| `MediaNotLoaded` | 未装载磁带 | 需先 load |
| `WriteProtected` | 磁带写保护开关已开 | 无法写入，需更换磁带或关闭物理开关 |
| `HardwareError(msg)` | 驱动硬件故障 | 标记驱动不可用，切换备用 |
| `MediaError(msg)` | 介质错误（坏块等） | 重试 → 切换副本 → 标记坏带 |
| `CleaningRequired` | 驱动需要清洁 | 发送通知，插入清洁带 |

#### 3.4.7 SlotInfo（槽位信息）

```rust
pub struct SlotInfo {
    pub slot_id: u32,
    pub element_type: SlotType,
    pub is_full: bool,
    pub media_id: Option<String>,
    pub barcode: Option<String>,
}
```

| 字段 | 类型 | 含义 | 来源 |
|------|------|------|------|
| `slot_id` | u32 | SCSI Element Address（槽位编号） | READ ELEMENT STATUS 响应 |
| `element_type` | SlotType | 槽位类型 | SCSI Element Type Code |
| `is_full` | bool | 是否有磁带 | Element Status `Full` 位 |
| `media_id` | Option\<String\> | 磁带 ID（若有） | MediaRegistry 关联 |
| `barcode` | Option\<String\> | 条码读取结果 | Volume Tag（若带库支持） |

#### 3.4.8 SlotType（槽位类型）

```rust
pub enum SlotType {
    Storage,
    DataTransfer,
    ImportExport,
    MediumTransport,
}
```

| 值 | SCSI Element Type | 含义 |
|------|-------------------|------|
| `Storage` | 0x02 | 存储槽位，存放磁带 |
| `DataTransfer` | 0x04 | 驱动位，磁带装载到此处可读写 |
| `ImportExport` | 0x03 | 导入/导出槽位（"邮箱"），人工放入/取出磁带 |
| `MediumTransport` | 0x01 | 机械臂本身 |

#### 3.4.9 VerifyResult（校验结果）

```rust
pub struct VerifyResult {
    pub readable: bool,
    pub blocks_verified: u64,
    pub blocks_failed: u64,
    pub errors: Vec<VerifyError>,
    pub duration_secs: u64,
}
```

| 字段 | 类型 | 含义 | 用途 |
|------|------|------|------|
| `readable` | bool | 整体是否可读 | 快速判断磁带健康 |
| `blocks_verified` | u64 | 成功校验的块数 | 校验覆盖度 |
| `blocks_failed` | u64 | 校验失败的块数 | 超过阈值触发退役 |
| `errors` | Vec\<VerifyError\> | 具体错误列表 | 定位坏块位置 |
| `duration_secs` | u64 | 校验耗时（秒） | 可观测性 |

#### 3.4.10 WriteResult（写入结果）

```rust
pub struct WriteResult {
    pub bytes_written: u64,
    pub blocks_written: u64,
    pub filemark_position: u32,
    pub tape_block_offset: u64,
}
```

| 字段 | 类型 | 含义 | 用途 |
|------|------|------|------|
| `bytes_written` | u64 | 实际写入字节数 | 更新 MediaCapacity.used_bytes |
| `blocks_written` | u64 | 实际写入块数 | 日志/监控 |
| `filemark_position` | u32 | 写入后的 FileMark 编号 | 记录到 BundleEntry 用于取回定位 |
| `tape_block_offset` | u64 | 写入起始的磁带块偏移 | 记录到 BundleEntry.tape_block_offset |

#### 3.4.11 MediaRegistration（介质注册信息）

```rust
pub struct MediaRegistration {
    pub media_id: String,
    pub barcode: Option<String>,
    pub format: String,
    pub capacity_bytes: u64,
    pub label: Option<String>,
    pub initial_location: MediaLocation,
}
```

| 字段 | 类型 | 含义 | 用途 |
|------|------|------|------|
| `media_id` | String | 磁带唯一标识 | 全局唯一，如 "TAPE001" |
| `barcode` | Option\<String\> | 条码（物理粘贴） | 带库自动识别 |
| `format` | String | 介质格式 | "LTO-9"、"LTO-10" |
| `capacity_bytes` | u64 | 标称容量 | LTO-9: 18TB = 18,000,000,000,000 |
| `label` | Option\<String\> | 逻辑标签 | 写入磁带头部，人工可识别 |
| `initial_location` | MediaLocation | 初始位置 | Slot(N) 或 Offsite("...") |

#### 3.4.12 LibraryInventory / DriveSlotInfo / LibraryStatus

```rust
pub struct LibraryInventory {
    pub slots: Vec<SlotInfo>,
    pub drives: Vec<DriveSlotInfo>,
    pub import_export: Vec<SlotInfo>,
    pub total_media: u32,
    pub empty_slots: u32,
}
```

| 字段 | 类型 | 含义 |
|------|------|------|
| `slots` | Vec\<SlotInfo\> | 所有存储槽位信息 |
| `drives` | Vec\<DriveSlotInfo\> | 所有驱动位信息 |
| `import_export` | Vec\<SlotInfo\> | 导入/导出槽位信息 |
| `total_media` | u32 | 库中磁带总数 |
| `empty_slots` | u32 | 空槽位数 |

```rust
pub struct DriveSlotInfo {
    pub drive_id: u32,
    pub device_path: String,
    pub is_loaded: bool,
    pub media_id: Option<String>,
}
```

| 字段 | 类型 | 含义 |
|------|------|------|
| `drive_id` | u32 | SCSI Data Transfer Element Address |
| `device_path` | String | 设备节点路径（如 `/dev/nst0`） |
| `is_loaded` | bool | 是否已装载磁带 |
| `media_id` | Option\<String\> | 已装载磁带的 ID |

```rust
pub struct LibraryStatus {
    pub library_id: String,
    pub is_online: bool,
    pub total_slots: u32,
    pub total_drives: u32,
    pub error: Option<String>,
}
```

| 字段 | 类型 | 含义 |
|------|------|------|
| `library_id` | String | 带库唯一标识 |
| `is_online` | bool | 带库是否在线可操作 |
| `total_slots` | u32 | 存储槽位总数 |
| `total_drives` | u32 | 驱动总数 |
| `error` | Option\<String\> | 当前错误信息 |

#### 3.4.13 OfflineRequest / OfflineRequestStatus

```rust
pub struct OfflineRequest {
    pub request_id: Uuid,
    pub media_id: String,
    pub barcode: Option<String>,
    pub last_known_location: String,
    pub reason: String,
    pub requested_at: DateTime<Utc>,
    pub status: OfflineRequestStatus,
    pub notified_at: Option<DateTime<Utc>>,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub timeout_at: DateTime<Utc>,
}
```

| 字段 | 类型 | 含义 | 用途 |
|------|------|------|------|
| `request_id` | Uuid | 请求唯一标识 | 跟踪去重 |
| `media_id` | String | 需要上线的磁带 ID | 通知内容 |
| `barcode` | Option\<String\> | 磁带条码 | 人工查找用 |
| `last_known_location` | String | 最后已知位置 | 如 "Offsite:机房B-柜3" |
| `reason` | String | 请求原因 | 如 "RecallTask xxx 需要读取" |
| `requested_at` | DateTime\<Utc\> | 请求创建时间 | 审计 |
| `status` | OfflineRequestStatus | 请求状态 | 流程跟踪 |
| `notified_at` | Option\<DateTime\<Utc\>\> | 通知发送时间 | 审计 |
| `confirmed_at` | Option\<DateTime\<Utc\>\> | 确认上线时间 | 计算等待时长 |
| `timeout_at` | DateTime\<Utc\> | 超时截止时间 | 超时后自动标记 RecallTask Failed |

```rust
pub enum OfflineRequestStatus {
    Pending,     // 已创建，未发通知
    Notified,    // 已发送通知
    Confirmed,   // 人工确认磁带已上线
    Cancelled,   // 取消（任务被用户取消等）
    TimedOut,    // 超时未确认
}
```

---

## 4. TapeManager：面向调度器的业务 API

### 4.1 职责

TapeManager 编排 TapeDrive / TapeMedia / TapeLibrary，提供面向调度器的高层 API。

### 4.2 TapeManager 结构体

```rust
pub struct TapeManager {
    drives: Vec<Box<dyn TapeDrive>>,
    media_registry: MediaRegistry,
    library: Option<Box<dyn TapeLibrary>>,
    drive_allocator: DriveAllocator,
    notification: NotificationSender,
}
```

| 字段 | 类型 | 含义 | 说明 |
|------|------|------|------|
| `drives` | Vec\<Box\<dyn TapeDrive\>\> | 所有已注册驱动实例 | 启动时根据配置创建 |
| `media_registry` | MediaRegistry | 介质注册表 | 磁带 ID → 容量/状态/位置/归档包映射 |
| `library` | Option\<Box\<dyn TapeLibrary\>\> | 带库实例（可选） | 前期无带库时为 None |
| `drive_allocator` | DriveAllocator | 驱动分配器 | 独占分配 + 优先级队列 |
| `notification` | NotificationSender | 通知发送器 | 离线请求、错误告警 |

### 4.3 核心接口

```rust

impl TapeManager {
    // ── 驱动分配 ──
    // priority: 影响排队顺序和预留驱动分配
    pub async fn acquire_drive(&self, priority: DrivePriority) -> Result<DriveGuard>;

    // ── 磁带选择与加载 ──
    pub async fn select_writable_tape(&self, min_remaining: u64) -> Result<String>;
    pub async fn load_tape(&self, media_id: &str, drive: &DriveGuard) -> Result<()>;
    pub async fn unload_tape(&self, drive: &DriveGuard) -> Result<()>;

    // ── 数据写入（供归档调度器） ──
    pub async fn sequential_write(
        &self,
        drive: &DriveGuard,
        data: &[u8],
    ) -> Result<WriteResult>;
    pub async fn write_filemark(&self, drive: &DriveGuard) -> Result<()>;

    // ── 数据读取（供取回调度器） ──
    pub async fn seek_and_read(
        &self,
        drive: &DriveGuard,
        filemark: u32,
        offset: u64,
        length: u64,
    ) -> Result<Vec<u8>>;

    // ── 介质管理 ──
    pub async fn register_media(&self, info: MediaRegistration) -> Result<()>;
    pub async fn retire_media(&self, media_id: &str) -> Result<()>;
    pub async fn get_media_info(&self, media_id: &str) -> Result<TapeInfo>;
    pub async fn update_media_status(&self, media_id: &str, status: MediaStatus) -> Result<()>;

    // ── 离线磁带 ──
    pub async fn check_media_online(&self, media_id: &str) -> Result<bool>;
    pub async fn request_offline_media(
        &self,
        media_id: &str,
        reason: &str,
    ) -> Result<OfflineRequest>;
    pub async fn confirm_media_online(&self, media_id: &str) -> Result<()>;

    // ── 库盘点 ──
    pub async fn run_inventory(&self) -> Result<LibraryInventory>;

    // ── 健康与校验 ──
    pub async fn verify_media(&self, media_id: &str) -> Result<VerifyResult>;
    pub async fn get_drive_status(&self, drive_id: &str) -> Result<DriveStatus>;
    pub async fn list_all_drives(&self) -> Result<Vec<DriveStatus>>;
}
```

### 4.4 DriveGuard（驱动独占令牌）

```rust
pub struct DriveGuard {
    pub drive_id: String,
    drive: Arc<Box<dyn TapeDrive>>,
    release_tx: oneshot::Sender<String>,
}
```

| 字段 | 类型 | 含义 | 用途 |
|------|------|------|------|
| `drive_id` | String | 被独占的驱动 ID | 日志追踪 |
| `drive` | Arc\<Box\<dyn TapeDrive\>\> | 驱动实例引用 | 调用 read/write/seek 等操作 |
| `release_tx` | Option\<oneshot::Sender\<String\>\> | 释放通道 | Drop 时发送 drive_id 归还到 DriveAllocator |

> **RAII 设计**：不提供显式 `release_drive` 方法。DriveGuard 在 Drop 时自动通过 `release_tx` 归还驱动（`take()` 语义保证只发送一次），防止忘记释放。调用者只需让 DriveGuard 离开作用域即可。

### 4.5 DriveAllocator（驱动分配器）

```rust
pub struct DriveAllocator {
    available: Mutex<VecDeque<String>>,
    waiters: Mutex<BinaryHeap<DriveWaiter>>,
    reserved_for_expedited: u32,
    total_drives: u32,
}
```

| 字段 | 类型 | 含义 | 用途 |
|------|------|------|------|
| `available` | Mutex\<VecDeque\<String\>\> | 空闲驱动 ID 队列 | 分配时 pop |
| `waiters` | Mutex\<BinaryHeap\<DriveWaiter\>\> | 等待分配的请求（按优先级排序） | 无可用驱动时入堆 |
| `reserved_for_expedited` | u32 | 为 Expedited 预留的驱动数 | 非 Expedited 请求不可占用预留驱动 |
| `total_drives` | u32 | 驱动总数 | 计算利用率 |
| `drive_map` | HashMap\<String, Arc\<Box\<dyn TapeDrive\>\>\> | drive_id → 驱动实例映射 | acquire 时从此获取实例构造 DriveGuard |

```rust
pub enum DrivePriority {
    Expedited,   // 最高，可使用预留驱动
    Archive,     // 归档写入
    Standard,    // 标准取回
    Bulk,        // 批量取回，最低
}
```

### 4.6 TapeInfo（磁带信息 DTO）

`TapeManager.get_media_info()` 返回的只读视图，由 `MediaRecord` 投影而来。

```rust
pub struct TapeInfo {
    pub id: String,
    pub barcode: Option<String>,
    pub format: String,
    pub status: MediaStatus,
    pub location: MediaLocation,
    pub capacity: MediaCapacity,
    pub archive_bundles: Vec<Uuid>,
    pub last_verified_at: Option<DateTime<Utc>>,
    pub error_count: u32,
}
```

> `TapeInfo` 为 `MediaRecord` 的只读子集，不含可变操作接口。元数据层的 `tapes` CF 也存储此结构（由调度层写入）。

### 4.7 MediaRegistry（介质注册表）

```rust
pub struct MediaRegistry {
    media: HashMap<String, MediaRecord>,
}

pub struct MediaRecord {
    pub media_id: String,
    pub barcode: Option<String>,
    pub format: String,
    pub capacity: MediaCapacity,
    pub status: MediaStatus,
    pub location: MediaLocation,
    pub label: Option<String>,
    pub archive_bundles: Vec<Uuid>,
    pub registered_at: DateTime<Utc>,
    pub last_verified_at: Option<DateTime<Utc>>,
    pub error_count: u32,
}
```

| 字段 | 类型 | 含义 | 用途 |
|------|------|------|------|
| `media_id` | String | 磁带唯一标识 | 主键 |
| `barcode` | Option\<String\> | 条码 | 带库自动识别 |
| `format` | String | 介质格式 | "LTO-9" 等 |
| `capacity` | MediaCapacity | 容量信息 | total/used/remaining |
| `status` | MediaStatus | 当前状态 | Online/Offline/InDrive 等 |
| `location` | MediaLocation | 当前位置 | Slot/Drive/Offsite |
| `label` | Option\<String\> | 逻辑标签 | 人工可识别的名称 |
| `archive_bundles` | Vec\<Uuid\> | 已写入的归档包 ID 列表 | 磁带内容追踪 |
| `registered_at` | DateTime\<Utc\> | 注册时间 | 介质寿命追踪 |
| `last_verified_at` | Option\<DateTime\<Utc\>\> | 最近校验时间 | 定期校验调度 |
| `error_count` | u32 | 累计错误次数 | 超阈值触发退役 |

**TapeMedia trait 与 MediaRecord 的关系**：
- `TapeMedia` trait 抽象物理介质操作（需要驱动参与的操作：verify_readable、erase、write_label）
- `MediaRecord` 为内存中的逻辑管理记录，不依赖物理驱动
- `ScsiTapeMedia` 实现 `TapeMedia` trait，同时关联一个 `MediaRecord`
- `MediaRegistry` 仅管理 `MediaRecord`；需要物理操作时，TapeManager 先从 Registry 找到 record，再通过对应的 `TapeMedia` 实例执行

---

## 5. 前期实现：Linux SCSI

### 5.1 ScsiTapeDrive

| 接口 | 实现 |
|------|------|
| 设备节点 | `/dev/nst0`（非自动回卷，推荐） |
| 数据读写 | 标准 `read()` / `write()` 系统调用 |
| 定位 | MTIO ioctl：MTFSF、MTBSF、MTFSR、MTBSR、MTREW、MTEOM |
| 状态 | MTIOCGET |
| 位置 | MTIOCPOS |
| FileMark | MTIOCTOP + MTWEOF |
| 块模式 | MTSETBLK 设置块大小（256KB 推荐） |
| 压缩 | MTCOMPRESSION（MTIOCTOP）；MTSETDRVBUFFER + MT_ST_DEF_COMPRESSION 设默认（需 root） |
| 弹出 | MTOFFL |

### 5.2 MTIO ioctl 封装（mtio.rs）

```rust
pub struct MtioHandle {
    fd: RawFd,
}

impl MtioHandle {
    pub fn open(device: &str) -> Result<Self>;

    // MTIOCTOP 操作
    pub fn op(&self, op: MtOp, count: i32) -> Result<()>;

    // MTIOCGET
    pub fn get_status(&self) -> Result<MtGet>;

    // MTIOCPOS
    pub fn get_position(&self) -> Result<MtPos>;
}

pub enum MtOp {
    Reset, Fsf, Bsf, Fsr, Bsr, Rew, Offl, Nop, Reten, Eom, Erase,
    Weof,            // 写 FileMark
    SetBlk(u32),     // 设置块大小
    SetCompression(bool),
}
```

### 5.3 ScsiTapeLibrary（中期实现）

基于 SCSI Medium Changer 协议（SMC），通过 `/dev/sg*` 下发 SCSI 命令。

| SCSI 命令 | 用途 |
|-----------|------|
| INITIALIZE ELEMENT STATUS | 盘点 |
| READ ELEMENT STATUS | 读取槽位/驱动状态 |
| MOVE MEDIUM | 机械臂搬运（load/unload/transfer） |
| REQUEST VOLUME ELEMENT ADDRESS | 条码读取 |

也可通过 `mtx` 命令行工具封装（开发期）。

### 5.4 sg.rs（SCSI Generic 封装，可选）

```rust
pub struct SgHandle {
    fd: RawFd,
}

impl SgHandle {
    pub fn open(device: &str) -> Result<Self>;
    pub fn send_command(&self, cdb: &[u8], data: &mut [u8]) -> Result<SgResult>;
}
```

---

## 6. 离线磁带管理

### 6.1 离线流程

```
取回调度器请求加载 tape_id
    │
    ▼
TapeManager.check_media_online(tape_id)
    │
    ├─ Online: load_tape → 继续
    │
    └─ Offline:
         1. TapeManager.request_offline_media(tape_id, reason)
         2. 通知服务: 发送工单/告警（tape_id, barcode, 位置, 请求方）
         3. 任务进入 wait_for_media 状态
         4. 人工: 将磁带插入带库 → 扫码确认
         5. TapeManager.confirm_media_online(tape_id)
         6. 自动继续: load_tape → 读取
```

### 6.2 离线请求结构

详见 §3.4.13 `OfflineRequest` / `OfflineRequestStatus` 定义。

---

## 7. 可靠性

### 7.1 双副本写入

| 策略 | 说明 |
|------|------|
| 双磁带副本 | 归档时写入两盘不同磁带，记录 tape_set |
| 跨库分布 | 副本分布在不同带库或离线库，防止单点灾难 |
| 调度器协同 | 调度器负责决定双写，TapeManager 负责执行 |

### 7.2 校验机制

| 机制 | 触发 | 说明 |
|------|------|------|
| 写入后 CRC | 每次写入 | 驱动层记录 CRC，写后对比 |
| 定期读校验 | 定时任务 | `TapeManager.verify_media` 读取并校验 |
| Bundle 校验 | 归档完成后 | 可选读回校验整个 Bundle |

### 7.3 错误处理

| 错误类型 | 处理 |
|----------|------|
| 驱动硬件错误 | 标记驱动不可用，切换备用驱动 |
| 介质读取错误 | 重试 → 切换副本 → 通知人工 |
| 介质写入错误 | 重试 → 换磁带继续写 → 标记坏带 |
| 清洁需求 | DriveError::CleaningRequired → 通知 |

### 7.4 介质退役

```
定期校验 → 发现不可读块增多 → 标记 Retired → 数据迁移到新磁带 → 淘汰
```

---

## 8. 可观测性

| 指标 | 说明 |
|------|------|
| drive_utilization | 驱动利用率 |
| write_throughput_mbps | 写入吞吐 |
| read_throughput_mbps | 读取吞吐 |
| tape_swap_count | 换带次数 |
| tape_swap_duration_ms | 换带耗时 |
| media_error_count | 介质错误数 |
| drive_error_count | 驱动错误数 |
| offline_request_count | 离线请求数 |
| offline_wait_duration_s | 离线等待时长 |
| verify_pass_rate | 校验通过率 |

---

## 9. 模块结构

```
src/tape/
├── mod.rs                      # 模块入口，pub use
│
├── manager.rs                  # TapeManager — 面向调度器的业务 API
├── drive_allocator.rs          # DriveAllocator — 驱动独占分配与排队
├── media_registry.rs           # MediaRegistry — 介质注册、容量、状态、位置
├── offline.rs                  # 离线磁带请求与确认流程
│
├── sdk/                        # ─── SDK 抽象层 (trait) ───
│   ├── mod.rs
│   ├── drive.rs                # TapeDrive trait
│   ├── media.rs                # TapeMedia trait
│   ├── library.rs              # TapeLibrary trait
│   └── types.rs                # DriveStatus, SlotInfo, MediaStatus, ...
│
├── scsi/                       # ─── Linux SCSI 实现 ───
│   ├── mod.rs
│   ├── drive.rs                # ScsiTapeDrive (impl TapeDrive)
│   ├── media.rs                # ScsiTapeMedia (impl TapeMedia)
│   ├── library.rs              # ScsiTapeLibrary (impl TapeLibrary, 中期)
│   ├── mtio.rs                 # MTIO ioctl 封装 (MtioHandle)
│   └── sg.rs                   # SCSI Generic 封装 (SgHandle, 可选)
│
└── vendor/                     # ─── 厂商 SDK 实现 (后期) ───
    └── mod.rs                  # VendorTapeDrive, VendorTapeLibrary
```

---

## 10. 配置项

```yaml
tape:
  sdk:
    backend: "scsi"            # scsi | vendor

  drives:
    - id: "drive0"
      device: "/dev/nst0"
      block_size: 262144       # 256KB
      compression: true
    - id: "drive1"
      device: "/dev/nst1"
      block_size: 262144
      compression: true

  library:
    enabled: false             # 前期单驱动可关闭
    device: "/dev/sg2"         # SCSI Medium Changer 设备
    auto_inventory: true       # 启动时自动盘点

  media:
    supported_formats:
      - "LTO-9"
      - "LTO-10"
    replication_factor: 2      # 双副本
    verify_interval_days: 90   # 每 90 天校验

  offline:
    notification_enabled: true
    notification_timeout_secs: 86400  # 24h 超时
```

---

## 11. 部署模型与依赖关系

磁带层运行在 **Tape Worker** 节点上（独立物理节点，挂载磁带驱动和带库），
通过 gRPC 接收 Scheduler Worker 的远程调用指令。

磁带层是**纯硬件抽象层**，不持有 MetadataClient。所有元数据更新由调度层统一负责。

| 对接方 | 方向 | 协议 | 接口 | 说明 |
|--------|------|------|------|------|
| Scheduler Worker | gRPC（远程） | 调度 → TapeManager | acquire_drive, sequential_write, seek_and_read | 调度层编排 |
| 元数据层 | **无直接依赖** | — | — | TapeInfo 等由 Scheduler Worker 负责更新 |
| Metadata 集群 | gRPC（心跳） | Tape Worker → Metadata | 上报驱动状态、在线状态 |
| 通知服务 | 本地 | TapeManager → 通知 | 离线磁带请求、错误告警 |

---

## 12. 开发测试环境：mhVTL

开发和 CI 环境中无需真实磁带硬件，使用 [mhVTL (Linux Virtual Tape Library)](https://github.com/markh794/mhvtl) 模拟完整的磁带驱动和带库。

### 12.1 mhVTL 概述

mhVTL 通过内核模块（伪 HBA）+ 用户态守护进程（`vtltape`、`vtllibrary`）模拟 SCSI 磁带设备，暴露标准的 `/dev/nst*`、`/dev/sg*` 设备节点。对 ColdStore 的 SCSI 实现层来说，虚拟磁带与真实磁带**接口完全一致**。

| mhVTL 组件 | 作用 | ColdStore 对接点 |
|-----------|------|-----------------|
| `mhvtl.ko` 内核模块 | 伪 SCSI HBA | 暴露 `/dev/nst*`、`/dev/sg*` |
| `vtltape` 守护进程 | SSC target，磁带读写 | `ScsiTapeDrive` 的 mtio ioctl |
| `vtllibrary` 守护进程 | SMC target，带库管理 | `ScsiTapeLibrary` 的 SCSI Medium Changer |
| `mktape` 工具 | 创建虚拟磁带 | 测试前准备介质 |

### 12.2 开发环境配置示例

```bash
# 安装 mhVTL（以 Ubuntu/Debian 为例）
sudo apt-get install mhvtl-utils lsscsi sg3-utils

# 启动虚拟磁带库
sudo systemctl start mhvtl

# 查看模拟设备
lsscsi -g
# [3:0:0:0]  mediumx  SPECTRA  PYTHON           ...  /dev/sch0  /dev/sg5
# [3:0:1:0]  tape     QUANTUM  ULTRIUM-HH7      ...  /dev/st0   /dev/sg6
# [3:0:2:0]  tape     QUANTUM  ULTRIUM-HH7      ...  /dev/st1   /dev/sg7
```

ColdStore 配置（开发模式）：

```yaml
tape:
  sdk:
    backend: "scsi"
  drives:
    - id: "drive0"
      device: "/dev/nst0"     # mhVTL 虚拟驱动
      block_size: 262144
    - id: "drive1"
      device: "/dev/nst1"     # mhVTL 虚拟驱动
      block_size: 262144
  library:
    enabled: true
    device: "/dev/sg5"        # mhVTL 虚拟带库
```

### 12.3 测试用途

| 用途 | 说明 |
|------|------|
| 单元测试 | 验证 mtio ioctl 封装、FileMark 定位、块读写 |
| 集成测试 | 全链路：S3 PUT → 归档调度 → 磁带写入 → 取回 → 缓存 → GET |
| 故障注入 | 停止 vtltape 进程模拟驱动故障，测试错误处理和重试 |
| CI/CD | GitHub Actions / GitLab CI 中安装 mhVTL，自动化端到端测试 |
| 调度验证 | 模拟多驱动、多磁带，验证合并、聚合、优先级等调度算法 |

---

## 13. 实现路径

| 阶段 | 实现 | 范围 | 测试环境 |
|------|------|------|----------|
| **Phase 1** | ScsiTapeDrive + MtioHandle | 单驱动读写、定位、FileMark、状态 | mhVTL 单驱动 |
| **Phase 2** | TapeManager + DriveAllocator | 多驱动分配、独占、MediaRegistry | mhVTL 多驱动 |
| **Phase 3** | ScsiTapeLibrary + SgHandle | 带库盘点、load/unload、条码 | mhVTL 带库 |
| **Phase 4** | 离线流程 + 通知 | request_offline_media、confirm | mhVTL + 手动移除 |
| **Phase 5** | 可靠性 | 双副本、verify_media、退役 | mhVTL 多磁带 |
| **Phase 6** | VendorTapeDrive（可选） | 厂商 SDK 对接 | 真实硬件 |

---

## 14. 参考资料

- [Linux st(4) - SCSI tape device](https://man7.org/linux/man-pages/man4/st.4.html)
- [Linux SCSI tape driver (kernel.org)](https://kernel.org/doc/Documentation/scsi/st.txt)
- [mtx - control SCSI media changer devices](https://manpages.ubuntu.com/manpages/jammy/man1/mtx.1.html)
- [SCSI T-10 SSC (Streaming Commands)](https://www.t10.org/)
- [SCSI T-10 SMC (Medium Changer Commands)](https://www.t10.org/)
- [mhVTL - Linux Virtual Tape Library](https://sites.google.com/site/linuxvtl2/) — 开发测试用虚拟磁带库
- [mhVTL GitHub](https://github.com/markh794/mhvtl) — mhVTL 源码
