# ColdStore 设计文档

## 文档索引

| 文档 | 说明 |
|------|------|
| [DESIGN.md](./DESIGN.md) | **总架构设计** - 协议、元数据集群、缓存层、磁带层完整设计 |
| [modules/](./modules/) | **模块设计** - 按架构分层拆分的独立设计文档 |

## 模块设计文档

| 模块 | 文档 |
|------|------|
| 接入层 | [01-access-layer.md](./modules/01-access-layer.md) |
| 协议适配层 | [02-protocol-adapter.md](./modules/02-protocol-adapter.md) |
| 元数据集群 | [03-metadata-cluster.md](./modules/03-metadata-cluster.md) |
| 数据缓存层 | [04-cache-layer.md](./modules/04-cache-layer.md) |
| 归档取回调度层 | [05-scheduler-layer.md](./modules/05-scheduler-layer.md) |
| 磁带管理层 | [06-tape-layer.md](./modules/06-tape-layer.md) |

## 设计要点摘要

- **协议**：兼容 S3 Glacier 冷归档协议（RestoreObject、x-amz-restore、取回层级）
- **元数据**：[OpenRaft](https://github.com/databendlabs/openraft) + [openraft-rocksstore](https://crates.io/crates/openraft-rocksstore)，强一致性集群
- **缓存**：[async-spdk](https://github.com/madsys-dev/async-spdk) 用户态 NVMe 缓存，原生 async/await
- **磁带**：自研 SDK 抽象层，前期对接 Linux SCSI（st 驱动 + MTIO）

详见 [DESIGN.md](./DESIGN.md) 与 [modules/README.md](./modules/README.md)。
