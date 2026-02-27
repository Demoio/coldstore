# ColdStore 模块设计文档

按架构分层拆分的独立模块设计文档，与 [总架构设计](../DESIGN.md) 配套使用。

## 文档索引

| 序号 | 模块 | 文档 | 说明 |
|------|------|------|------|
| 01 | 接入层 | [01-access-layer.md](./01-access-layer.md) | S3 HTTP 服务、路由、Axum |
| 02 | 协议适配层 | [02-protocol-adapter.md](./02-protocol-adapter.md) | StorageClass 映射、RestoreRequest、x-amz-restore、错误码 |
| 03 | 元数据集群 | [03-metadata-cluster.md](./03-metadata-cluster.md) | OpenRaft + RocksDB、强一致性 |
| 04 | 数据缓存层 | [04-cache-layer.md](./04-cache-layer.md) | async-spdk、解冻数据缓存 |
| 05 | 归档取回调度层 | [05-scheduler-layer.md](./05-scheduler-layer.md) | Archive Scheduler、Recall Scheduler |
| 06 | 磁带管理层 | [06-tape-layer.md](./06-tape-layer.md) | 自研 SDK、Linux SCSI |
| 07 | 跨层一致性与性能 | [07-consistency-performance.md](./07-consistency-performance.md) | Saga 模式、并发控制、故障矩阵、性能优化 |
| 08 | 可观测性与链路追踪 | [08-observability.md](./08-observability.md) | OpenTelemetry、Traces、Metrics、Logs、告警 |
| 09 | 管控面 (Admin Console) | [09-admin-console.md](./09-admin-console.md) | Web UI、Admin API、集群/磁带/任务管理 |

## 架构层次关系

```
接入层 (01)
    │
    ▼
协议适配层 (02)
    │
    ├──────────────────┬──────────────────┬──────────────────┐
    ▼                  ▼                  ▼                  ▼
元数据集群 (03)    数据缓存层 (04)    归档取回调度层 (05)
                        │                  │
                        │                  ▼
                        │            磁带管理层 (06)
                        └──────────────────┘
```
