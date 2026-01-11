# ColdStore - Rust 实现

基于 Rust 的 S3 兼容磁带冷存储归档系统。

## 项目结构

```
coldstore/
├── Cargo.toml              # 项目配置和依赖
├── src/
│   ├── main.rs             # 主入口
│   ├── lib.rs              # 库入口
│   ├── s3/                 # S3 接入层
│   │   ├── mod.rs
│   │   ├── server.rs       # S3 HTTP 服务器
│   │   └── handler.rs      # S3 请求处理
│   ├── metadata/           # 元数据服务
│   │   ├── mod.rs
│   │   ├── service.rs      # 元数据服务接口
│   │   └── backend.rs      # 后端实现（PostgreSQL/Etcd）
│   ├── scheduler/          # 调度器
│   │   ├── mod.rs
│   │   ├── archive.rs      # 归档调度器
│   │   └── recall.rs       # 取回调度器
│   ├── cache/              # 缓存层
│   │   ├── mod.rs
│   │   └── manager.rs      # 缓存管理
│   ├── tape/               # 磁带管理
│   │   ├── mod.rs
│   │   ├── manager.rs      # 磁带管理器
│   │   └── driver.rs       # 磁带驱动抽象
│   ├── notification/       # 通知服务
│   │   ├── mod.rs
│   │   └── service.rs      # 通知服务实现
│   ├── config/             # 配置管理
│   │   └── mod.rs
│   ├── error/              # 错误处理
│   │   └── mod.rs
│   └── models/             # 数据模型
│       └── mod.rs
├── config/
│   └── config.yaml.example # 配置示例
└── README_RUST.md          # 本文档
```

## 快速开始

### 1. 安装 Rust

确保已安装 Rust 工具链（推荐使用 rustup）：

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### 2. 配置项目

复制配置示例文件：

```bash
cp config/config.yaml.example config/config.yaml
```

根据实际情况编辑 `config/config.yaml`。

### 3. 设置数据库（如果使用 PostgreSQL）

```bash
createdb coldstore
# 运行数据库迁移（待实现）
```

### 4. 构建项目

```bash
cargo build --release
```

### 5. 运行

```bash
cargo run --release
```

或使用环境变量覆盖配置：

```bash
COLDSTORE_SERVER_PORT=9001 cargo run --release
```

## 开发状态

当前项目处于初始骨架阶段，各模块的基础结构已搭建，但具体实现需要逐步完善：

- ✅ 项目结构和模块划分
- ✅ 配置管理框架
- ✅ 错误处理框架
- ✅ 数据模型定义
- ⏳ S3 协议实现
- ⏳ 元数据后端实现
- ⏳ 归档调度器实现
- ⏳ 取回调度器实现
- ⏳ 磁带驱动集成
- ⏳ 缓存层实现
- ⏳ 通知系统集成

## 下一步开发计划

1. **完善 S3 协议支持**
   - 实现 PUT/GET/HEAD 操作
   - 实现 RestoreObject API
   - 添加认证和授权

2. **实现元数据后端**
   - PostgreSQL 表结构设计
   - 数据库迁移脚本
   - Etcd 键值结构设计

3. **实现归档调度器**
   - 对象扫描逻辑
   - 归档包聚合
   - 磁带写入流程

4. **实现取回调度器**
   - 任务队列管理
   - 任务合并逻辑
   - 磁带读取流程

5. **集成磁带驱动**
   - LTFS 集成
   - 或厂商 SDK 集成
   - 驱动抽象层完善

6. **完善缓存层**
   - LRU/LFU 实现
   - TTL 管理
   - 容量限制和淘汰

7. **测试和文档**
   - 单元测试
   - 集成测试
   - API 文档

## 依赖说明

主要依赖包括：

- **tokio**: 异步运行时
- **axum**: HTTP 服务器框架
- **sqlx**: 异步 SQL 工具
- **etcd-rs**: Etcd 客户端
- **serde**: 序列化/反序列化
- **tracing**: 结构化日志

## 许可证

MIT OR Apache-2.0
