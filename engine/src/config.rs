use std::path::PathBuf;

/// 引擎运行配置：数据目录 + 本地监听地址。
/// 只绑定 127.0.0.1：引擎在本机进程内提供服务，不暴露局域网。
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub host: String,
    pub port: u16,
    pub data_dir: PathBuf,
    /// 本地服务访问令牌：Some 时 /api 接口需要 x-ainovel-token 头或 ?token=
    pub auth_token: Option<String>,
    /// 敏感字段加密密钥（64 位 hex）；None 时明文存储（仅本地开发）
    pub secret_hex: Option<String>,
}

impl AppConfig {
    pub fn for_data_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8787,
            data_dir: dir.into(),
            auth_token: None,
            secret_hex: None,
        }
    }

    pub fn with_security(mut self, auth_token: Option<String>, secret_hex: Option<String>) -> Self {
        self.auth_token = auth_token.filter(|t| !t.trim().is_empty());
        self.secret_hex = secret_hex.filter(|t| !t.trim().is_empty());
        self
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("ainovel.db")
    }

    pub fn ensure_dirs(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        Ok(())
    }

    pub fn serve_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}
