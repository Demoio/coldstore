# 协议适配层模块设计

> 所属架构：ColdStore 冷存储系统  
> 参考：[总架构设计](../DESIGN.md)

## 1. 模块概述

协议适配层负责将 S3 冷归档（Glacier）协议语义转换为 ColdStore 内部模型，并生成符合 S3 规范的响应。

### 1.1 职责

- StorageClass 映射（S3 ↔ 内部状态）
- RestoreRequest 解析（Days、Tier）
- x-amz-restore 响应头生成
- 错误码映射（InvalidObjectState、RestoreAlreadyInProgress 等）
- PutObject 默认 ColdPending 语义

### 1.2 在架构中的位置

```
S3 接入层 ──► 协议适配层 ──► Scheduler Worker（业务中枢）
                │
                ├─ StorageClass 映射
                ├─ RestoreRequest 解析
                ├─ x-amz-restore 响应
                └─ 错误码映射
```

---

## 2. StorageClass 映射

ColdStore 是纯冷归档系统，所有对象写入即为 `ColdPending`，归档后变为 `Cold`。与 AWS S3 Glacier Deep Archive 模型一致。

| S3 Storage Class | ColdStore 内部 | 说明 |
|------------------|----------------|------|
| `GLACIER` / `DEEP_ARCHIVE` / `STANDARD` / 任意 | `ColdPending` | 写入即排队归档（不区分热/冷，统一冷存储） |
| 归档完成 | `Cold` | 已归档到磁带 |

---

## 3. RestoreObject 协议适配

### 3.1 请求解析

**输入**：`POST /{bucket}/{key}?restore` + XML Body

```xml
<RestoreRequest xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Days>7</Days>
  <GlacierJobParameters>
    <Tier>Standard</Tier>
  </GlacierJobParameters>
</RestoreRequest>
```

**输出**（内部结构）：

```rust
pub struct RestoreRequest {
    pub days: u32,           // 最小 1
    pub tier: RestoreTier,   // Expedited | Standard | Bulk
    pub version_id: Option<String>,
}
```

### 3.2 取回层级 (Tier) 映射

| Tier | 内部优先级 | 调度策略 |
|------|------------|----------|
| Expedited | 高 | 高优先级队列，可配置预留容量 |
| Standard | 中 | 默认队列 |
| Bulk | 低 | 批量合并取回 |

### 3.3 响应规范

| 场景 | HTTP 状态 | 说明 |
|------|-----------|------|
| 首次解冻 | 202 Accepted | 任务已接受 |
| 已解冻且未过期 | 200 OK | 可延长 Days |
| 解冻进行中 | 409 Conflict | RestoreAlreadyInProgress |
| Expedited 容量不足 | 503 Service Unavailable | GlacierExpeditedRetrievalNotAvailable |
| 对象尚未归档（ColdPending） | 409 Conflict | 尚在归档队列中，不可 Restore |

---

## 4. x-amz-restore 响应头

对冷对象执行 HEAD 时，根据解冻状态返回：

| 状态 | 响应头 |
|------|--------|
| 解冻中 | `x-amz-restore: ongoing-request="true"` |
| 已解冻 | `x-amz-restore: ongoing-request="false", expiry-date="Fri, 28 Feb 2025 12:00:00 GMT"` |
| 未解冻 | 不返回该头（或可省略） |

---

## 5. 错误码映射

| AWS 错误码 | HTTP 状态 | 触发条件 |
|------------|-----------|----------|
| InvalidObjectState | 403 | 冷对象未解冻时 GET |
| RestoreAlreadyInProgress | 409 | 重复 Restore 请求 |
| GlacierExpeditedRetrievalNotAvailable | 503 | Expedited 容量不足 |
| ObjectNotYetArchived | 409 | ColdPending 状态对象执行 Restore（尚未归档完成） |

---

## 6. GET Object 冷对象访问控制

| 对象状态 | GET 行为 |
|----------|----------|
| Cold + 未解冻 | 403 InvalidObjectState |
| Cold + 解冻中 | 403 InvalidObjectState |
| Cold + 已解冻 | 从缓存读取并返回 |
| Cold + 解冻已过期 | 403 InvalidObjectState |

---

## 7. PutObject 语义

ColdStore 作为纯冷归档系统，PutObject 写入的对象一律标记为 `ColdPending`，
由调度器扫描后自动归档到磁带。外部热存储系统在需要归档时直接调用 ColdStore 的 PutObject。

---

## 8. 模块结构

```
src/
├── s3/
│   ├── protocol/           # 协议适配（可独立子模块）
│   │   ├── mod.rs
│   │   ├── storage_class.rs  # StorageClass 映射
│   │   ├── restore.rs       # RestoreRequest 解析、响应生成
│   │   ├── errors.rs        # 错误码映射
│   │   └── put_object.rs    # PutObject → ColdPending 语义
```

---

## 9. 依赖关系

| 依赖 | 用途 |
|------|------|
| Scheduler Worker (gRPC) | 全部业务请求：元数据查询、数据读写、Restore 任务提交 |

> 协议适配层运行在 Gateway 进程内，与接入层同进程。
> 所有对外通信都经由 Scheduler Worker，不直连 Metadata/Cache/Tape。

---

## 10. 参考资料

- [AWS S3 RestoreObject API](https://docs.aws.amazon.com/AmazonS3/latest/API/API_RestoreObject.html)
- [S3 Glacier Retrieval Options](https://docs.aws.amazon.com/AmazonS3/latest/userguide/restoring-objects-retrieval-options.html)
