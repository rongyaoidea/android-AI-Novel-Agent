//! Android JNI 入口（`--features jni` 时编译）。
//!
//! Kotlin 侧调用：
//! ```kotlin
//! external fun nativeStart(dataDir: String, port: Int, authToken: String, secretHex: String): Int
//! ```
//! 进程内启动本地引擎（tokio + axum），只监听 127.0.0.1。
//! 日志写入 `{dataDir}/ainovel.log`（每次启动截断），便于用户导出排障。

use std::sync::OnceLock;

use jni::objects::{JClass, JString};
use jni::sys::jint;
use jni::JNIEnv;

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// 初始化文件日志（只生效一次；失败静默，不影响引擎启动）
fn init_logging(data_dir: &str) {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let path = std::path::Path::new(data_dir).join("ainovel.log");
        let Ok(file) = std::fs::File::create(&path) else {
            return;
        };
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file))
            .try_init();
        tracing::info!("engine log file: {}", path.display());
    });
}

#[no_mangle]
pub extern "system" fn Java_com_ainovel_engine_EngineService_nativeStart(
    mut env: JNIEnv,
    _class: JClass,
    data_dir: JString,
    port: jint,
    auth_token: JString,
    secret_hex: JString,
) -> jint {
    let dir: String = match env.get_string(&data_dir) {
        Ok(s) => s.into(),
        Err(_) => return -1,
    };
    init_logging(&dir);
    let token: Option<String> = env
        .get_string(&auth_token)
        .ok()
        .map(|s| String::from(s))
        .filter(|s| !s.trim().is_empty());
    let secret: Option<String> = env
        .get_string(&secret_hex)
        .ok()
        .map(|s| String::from(s))
        .filter(|s| !s.trim().is_empty());
    let port = if port <= 0 || port > 65535 {
        8787u16
    } else {
        port as u16
    };
    let rt = RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime")
    });
    rt.spawn(async move {
        let cfg = crate::config::AppConfig {
            host: "127.0.0.1".into(),
            port,
            data_dir: dir.into(),
            auth_token: token,
            secret_hex: secret,
        };
        if let Err(e) = crate::run_server(cfg).await {
            tracing::error!("engine exited: {e:#}");
        }
    });
    0
}
