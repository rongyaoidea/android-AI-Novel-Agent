pub mod api;
pub mod backup;
pub mod config;
pub mod crypto;
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

/// 引擎实例：数据库 + 配置 + 事件总线 + 敏感字段加密盒。
pub struct Engine {
    pub db: db::Db,
    pub config: config::AppConfig,
    pub hub: pipeline::EventHub,
    pub secret: Option<crypto::SecretBox>,
}

impl Engine {
    pub fn new(config: config::AppConfig) -> anyhow::Result<Arc<Self>> {
        config.ensure_dirs()?;
        let db = db::Db::open(&config.db_path())?;
        let secret = config
            .secret_hex
            .as_deref()
            .and_then(crypto::SecretBox::from_hex);
        if config.secret_hex.is_some() && secret.is_none() {
            tracing::warn!("AI_NOVEL_SECRET_HEX 非法（应为 64 位 hex），敏感字段退化为明文存储");
        }
        if config.auth_token.is_none() {
            tracing::warn!("未提供访问令牌（AI_NOVEL_AUTH_TOKEN），本地 API 不设防");
        }
        Ok(Arc::new(Self {
            db,
            config,
            hub: pipeline::EventHub::new(),
            secret,
        }))
    }

    pub fn auth_token(&self) -> Option<&str> {
        self.config.auth_token.as_deref()
    }

    /// 读取模型设置：API Key 自动解密（解密失败视为不可用并告警）
    pub fn llm_settings(&self) -> anyhow::Result<models::LlmSettings> {
        let mut s = self.db.llm_settings()?;
        if !s.api_key.is_empty() {
            if let Some(sb) = &self.secret {
                match sb.decrypt(&s.api_key) {
                    Some(pt) => s.api_key = pt,
                    None => {
                        tracing::warn!("API Key 解密失败（密钥变更或数据损坏），已置空，请重新填写");
                        s.api_key = String::new();
                    }
                }
            }
        }
        Ok(s)
    }

    /// 保存模型设置：API Key 自动加密后落库
    pub fn save_llm_settings(&self, s: &models::LlmSettings) -> anyhow::Result<()> {
        let mut to_store = s.clone();
        if !to_store.api_key.is_empty() {
            if let Some(sb) = &self.secret {
                to_store.api_key = sb.encrypt(&to_store.api_key);
            }
        }
        self.db.set_llm_settings(&to_store)
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
