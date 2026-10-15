use anyhow::Context;

/// 用法：
///   ainovel [serve] [--port 8787] [--data-dir ./data]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let mut port: u16 = 8787;
    let mut data_dir = std::path::PathBuf::from("data");
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "serve" => {}
            "--port" => {
                i += 1;
                port = args
                    .get(i)
                    .context("--port 需要一个数字")?
                    .parse()
                    .context("--port 解析失败")?;
            }
            "--data-dir" => {
                i += 1;
                data_dir = args.get(i).context("--data-dir 需要一个路径")?.into();
            }
            other => {
                eprintln!("未知参数: {other}\n用法: ainovel [serve] [--port 8787] [--data-dir ./data]");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    let config = ainovel_engine::config::AppConfig {
        host: "127.0.0.1".into(),
        port,
        data_dir,
        auth_token: std::env::var("AI_NOVEL_AUTH_TOKEN").ok(),
        secret_hex: std::env::var("AI_NOVEL_SECRET_HEX").ok(),
    }
    .with_security(
        std::env::var("AI_NOVEL_AUTH_TOKEN").ok(),
        std::env::var("AI_NOVEL_SECRET_HEX").ok(),
    );
    if config.auth_token.is_some() {
        println!("已启用访问令牌：/api 需要 x-ainovel-token 头或 ?token= 参数");
    }
    if config.secret_hex.is_some() {
        println!("已启用敏感字段加密（AES-256-GCM）");
    }
    ainovel_engine::run_server(config).await
}
