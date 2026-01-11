use coldstore::config::Config;
use coldstore::error::Result;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "coldstore=info".into()),
        )
        .init();

    info!("启动 ColdStore 冷存储系统...");

    // 加载配置
    let _config = Config::load()?;
    info!("配置加载完成");

    // TODO: 初始化各个服务组件
    // - S3 接入层
    // - 元数据服务
    // - 归档调度器
    // - 取回调度器
    // - 缓存层
    // - 磁带管理
    // - 通知服务

    info!("ColdStore 系统启动完成");

    // 运行主循环
    tokio::signal::ctrl_c()
        .await
        .expect("无法注册 Ctrl+C 处理器");

    info!("收到停止信号，正在关闭...");
    Ok(())
}
