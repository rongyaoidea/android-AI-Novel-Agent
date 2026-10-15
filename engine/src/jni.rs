//! Android JNI 入口（`--features jni` 时编译）。
//!
//! Kotlin 侧调用：
//! ```kotlin
//! external fun nativeStart(dataDir: String, port: Int): Int
//! ```
//! 进程内启动本地引擎（tokio + axum），只监听 127.0.0.1。

use std::sync::OnceLock;

use jni::objects::{JClass, JString};
use jni::sys::jint;
use jni::JNIEnv;

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

#[no_mangle]
pub extern "system" fn Java_com_ainovel_engine_EngineService_nativeStart(
    mut env: JNIEnv,
    _class: JClass,
    data_dir: JString,
    port: jint,
) -> jint {
    let dir: String = match env.get_string(&data_dir) {
        Ok(s) => s.into(),
        Err(_) => return -1,
    };
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
        };
        if let Err(e) = crate::run_server(cfg).await {
            tracing::error!("engine exited: {e:#}");
        }
    });
    0
}
