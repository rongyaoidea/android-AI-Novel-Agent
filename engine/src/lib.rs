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
pub mod style_check;
pub mod styles;
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
    /// 请求停止中的作品（手动停止：流式生成即断；其它步骤在下一检查点退出）
    stop_flags: std::sync::Mutex<std::collections::HashSet<String>>,
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
            stop_flags: std::sync::Mutex::new(std::collections::HashSet::new()),
        }))
    }

    /// 请求停止某本作品的创作（配合 UI「停止」按钮与自动连载开关）
    pub fn request_stop(&self, novel_id: &str) {
        if let Ok(mut s) = self.stop_flags.lock() {
            s.insert(novel_id.to_string());
        }
    }

    pub fn stop_requested(&self, novel_id: &str) -> bool {
        self.stop_flags
            .lock()
            .map(|s| s.contains(novel_id))
            .unwrap_or(false)
    }

    pub fn clear_stop(&self, novel_id: &str) {
        if let Ok(mut s) = self.stop_flags.lock() {
            s.remove(novel_id);
        }
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

    /// 当前提示词覆盖（未设置或解析失败时为空集合）
    pub fn prompt_overrides(&self) -> prompts::Overrides {
        prompts::Overrides::from_json(self.db.prompt_overrides_json().ok().flatten().as_deref())
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
