# 接入层模块设计

> 所属架构：ColdStore 冷存储系统  
> 参考：[总架构设计](../DESIGN.md)

## 1. 模块概述

接入层是 ColdStore 对外提供对象存储服务的入口，负责接收并分发 S3 兼容的 HTTP 请求。

### 1.1 职责

- 提供 HTTP 服务，监听 S3 端点
- 路由解析：将请求分发到对应 Handler
- 请求/响应透传与基础校验
- 与协议适配层、元数据、缓存、调度器协同

### 1.2 在架构中的位置

```
                    ┌─────────────────────────────────────┐
                    │           S3 接入层 (本模块)           │
                    │  PUT / GET / HEAD / RestoreObject    │
                    │  ListBuckets / DeleteObject          │
                    └─────────────────────────────────────┘
                                        │
                                        ▼
                    协议适配层 / 元数据 / 缓存 / 调度器
```

---

## 2. 技术选型

| 组件 | 选型 | 说明 |
|------|------|------|
| HTTP 框架 | **Axum** | 异步、类型安全、与 Tokio 集成 |
| 运行时 | **Tokio** | 异步运行时 |
| 中间件 | **Tower** / **tower-http** | 中间件栈、CORS、trace |

---

## 3. 接口清单

### 3.1 对象操作

| 方法 | 路径 | 说明 |
|------|------|------|
| PUT | `/{bucket}/{key}` | 上传对象 |
| GET | `/{bucket}/{key}` | 下载对象 |
| HEAD | `/{bucket}/{key}` | 获取对象元数据 |
| DELETE | `/{bucket}/{key}` | 删除对象 |
| POST | `/{bucket}/{key}?restore` | 解冻冷对象（RestoreObject） |

### 3.2 桶操作

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/` | ListBuckets |
| PUT | `/{bucket}` | CreateBucket |
| GET | `/{bucket}` | ListObjects |
| DELETE | `/{bucket}` | DeleteBucket |

### 3.3 路由规则

- 路径参数：`bucket`、`key`（支持多级 key）
- 查询参数：`versionId`、`restore` 等
- 虚拟主机风格与路径风格均需支持（可配置）

---

## 4. 模块结构

```
src/
├── main.rs              # 启动 Axum，加载配置
├── s3/
│   ├── mod.rs
│   ├── server.rs        # Axum Router 定义、路由注册
│   ├── handler.rs       # 请求处理入口，委托给协议适配/业务逻辑
│   └── routes.rs        # 路由常量、路径解析
```

---

## 5. 依赖关系

| 依赖模块 | 通信方式 | 用途 |
|----------|----------|------|
| 协议适配层 | 本地调用（同进程） | 解析 RestoreRequest、生成 x-amz-restore 响应、错误码映射 |
| Scheduler Worker | gRPC（配置地址） | **全部**业务请求代理（PUT/GET/HEAD/DELETE/Restore/List） |

> Gateway **不连接 Metadata**，也**不连接 Cache Worker 和 Tape Worker**。
> 所有业务流量都通过 Scheduler Worker 中转。Gateway 是纯粹的 S3 HTTP → gRPC 协议前端。

---

## 6. 配置项

```yaml
gateway:
  s3:
    host: "0.0.0.0"
    port: 9000
    path_style: true

  # 仅需配置 Scheduler Worker 地址
  scheduler_addrs:
    - "10.0.2.1:22001"
    - "10.0.2.2:22001"

  # 多 Scheduler Worker 时的路由策略
  routing:
    strategy: "round_robin"   # round_robin | hash | least_pending
```

---

## 7. 非功能性要求

- **无状态**：可水平扩展，多实例 + 负载均衡
- **可观测**：请求 trace、耗时、错误率
- **安全**：支持 S3 签名 v4、TLS
