# ColdStore - Rust 实现

S3 兼容的纯冷归档磁带存储系统。

## 项目定位

ColdStore 是**纯冷归档系统**（类似 AWS Glacier Deep Archive）：
- 所有对象写入即标记为 ColdPending，自动排队归档到磁带
- 不提供在线热存储层，不做生命周期管理
- 外部热存储系统在需要冷归档时调用 ColdStore 的 PutObject API

## Workspace 结构

| Crate | 类型 | 说明 |
|-------|------|------|
| coldstore-proto | lib | gRPC 协议定义（protobuf 生成） |
| coldstore-common | lib | 共享模型、错误、配置 |
| coldstore-metadata | bin | 元数据集群节点（Raft + RocksDB） |
| coldstore-gateway | bin | S3 HTTP 网关（无状态） |
| coldstore-scheduler | bin | 调度 Worker（业务中枢） |
| coldstore-cache | bin | 缓存 Worker（独立进程，HDD/SPDK） |
| coldstore-tape | bin | 磁带 Worker（独立物理节点） |

## 组件间通信

所有组件通过 gRPC 通信，协议定义在 crates/proto/proto/ 下：

- common.proto: 共享枚举和数据结构
- metadata.proto: MetadataService（元数据读写、集群管理、心跳）
- scheduler.proto: SchedulerService（Gateway 转发 S3 请求）
- cache.proto: CacheService（数据暂存和解冻缓存）
- tape.proto: TapeService（磁带读写和驱动管理）

## 快速开始

前置条件: Rust 1.75+, protobuf-compiler

    make build-debug

分别启动各组件:

    cargo run -p coldstore-metadata
    cargo run -p coldstore-cache
    cargo run -p coldstore-scheduler
    cargo run -p coldstore-gateway

## 许可证

MIT OR Apache-2.0
