# ColdStore 模块设计文档

本目录保留按架构分层拆分的模块设计文档。注意：这些文档同时包含当前实现和目标态设计，部分内容已经超前于当前代码。

当前实现的权威说明见 [../ARCHITECTURE.md](../ARCHITECTURE.md)。涉及实现判断时，优先以该文档和 `crates/*/src` 为准。

## 文档索引

| 序号 | 模块 | 文档 | 说明 |
|------|------|------|------|
| 01 | 接入层 | [01-access-layer.md](./01-access-layer.md) | S3 HTTP 服务、路由、Axum |
| 02 | 协议适配层 | [02-protocol-adapter.md](./02-protocol-adapter.md) | StorageClass 映射、RestoreRequest、x-amz-restore、错误码 |
| 03 | 元数据集群 | [03-metadata-cluster.md](./03-metadata-cluster.md) | 当前单节点持久化路径；OpenRaft 多节点是目标态 |
| 04 | 数据缓存层 | [04-cache-layer.md](./04-cache-layer.md) | 当前 staging/restored 缓存；完整 async-spdk 是目标态 |
| 05 | 归档取回调度层 | [05-scheduler-layer.md](./05-scheduler-layer.md) | Archive Scheduler、Recall Scheduler |
| 06 | 磁带管理层 | [06-tape-layer.md](./06-tape-layer.md) | 自研 SDK、Linux SCSI |
| 07 | 跨层一致性与性能 | [07-consistency-performance.md](./07-consistency-performance.md) | Saga 模式、并发控制、故障矩阵、性能优化 |
| 08 | 可观测性与链路追踪 | [08-observability.md](./08-observability.md) | OpenTelemetry、Traces、Metrics、Logs、告警 |
| 09 | 管控面 (Admin Console) | [09-admin-console.md](./09-admin-console.md) | Web UI、Admin API、集群/磁带/任务管理 |

## 当前实现联动关系

```
Gateway(01/02)
    |
    v
Scheduler(05)
    |            |           |
    v            v           v
Metadata(03)  Cache(04)   Tape(06)
```

Gateway 不直接连接 Metadata/Cache/Tape。Scheduler 是业务中枢，负责把协议请求编排成 metadata/cache/tape 操作。

## 已知过时点

- 真实多节点 Metadata Raft 仍是目标态；当前多节点 `persistent_raft` 会 fail-fast。
- 完整 SPDK、真实磁带库、Admin Console、OpenTelemetry 全量链路仍是目标态。
- Multipart Upload、ListObjectsV2、完整 S3 兼容仍需继续实现。
