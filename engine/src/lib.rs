pub mod api;
pub mod config;
pub mod db;
pub mod export;
pub mod llm;
pub mod models;
pub mod pipeline;
pub mod prompts;
pub mod webui;

#[cfg(feature = "jni")]
pub mod jni;

use std::sync::Arc;

/// 引擎实例：数据库 + 配置 + 事件总线。
pub struct Engine {
    pub db: db::Db,
    pub config: config::AppConfig,
    pub hub: pipeline::EventHub,
}

impl Engine {
    pub fn new(config: config::AppConfig) -> anyhow::Result<Arc<Self>> {
        config.ensure_dirs()?;
        let db = db::Db::open(&config.db_path())?;
        Ok(Arc::new(Self {
            db,
            config,
            hub: pipeline::EventHub::new(),
        }))
    }
}

/// 启动本地服务（桌面 CLI 与 Android JNI 共用入口）。
pub async fn run_server(config: config::AppConfig) -> anyhow::Result<()> {
    let addr = config.serve_addr();
    let engine = Engine::new(config)?;
    let app = api::build_router(engine);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| anyhow::anyhow!("监听 {addr} 失败: {e}"))?;
    println!("AI Novel Agent 引擎已启动: http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
