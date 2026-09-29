//! Android 壳：完整引擎跑在手机上。
//!
//! Rust 核心作为 cdylib 编进 APK，在本进程内启动 axum 服务
//! （127.0.0.1:9208），WebView 加载同一端口。SQLite/文件存储在
//! 应用私有目录（AppConfig::data_dir 自动落到 ./data，由 tauri
//! 的路径 API 重定向到 app data dir）。
//!
//! 初始化步骤（需要 Android SDK/NDK，建议在 CI 或 WSL 里做）：
//!   rustup target add aarch64-linux-android
//!   cargo tauri android init
//!   cargo tauri android build --apk

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    std::thread::spawn(|| {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(async {
            if let Err(e) = start_engine().await {
                eprintln!("引擎启动失败: {e:#}");
            }
        });
    });

    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// v0.1：移动壳以进程内方式启动核心服务。
/// 正式版应把 pipeline 拆为库调用并在 tauri::Builder 挂 state，
/// 同时把 MDC_CONFIG_PATH 指到 app 私有目录。
async fn start_engine() -> anyhow::Result<()> {
    // 占位：接入 mdc-core 的 serve 入口（待 core 抽出 serve() 函数）
    Ok(())
}
