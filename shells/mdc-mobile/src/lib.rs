//! Android 壳：完整引擎跑在手机上。
//!
//! Rust 核心作为 cdylib 编进 APK，在本进程内启动 axum 服务
//! （127.0.0.1:9208），WebView 加载内嵌前端（web/dist），API 打到
//! 本机 9208。数据目录（SQLite/配置/密钥）通过 `MDC_CONFIG_PATH`
//! 重定向到应用私有目录（app data dir）。
//!
//! 构建步骤（需要 Android SDK/NDK）：
//!   rustup target add aarch64-linux-android
//!   cargo tauri android init
//!   cargo tauri android build --apk --debug

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // 数据目录必须指到应用私有目录：/data/data/<pkg>/ 下，
            // 再用相对路径 ./data 会落进不可写的安装目录。
            let data = app.path().app_data_dir()?;
            std::env::set_var("MDC_CONFIG_PATH", &data);
            std::env::set_var("MDC_BIND", "127.0.0.1:9208");
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
            // tauri.conf.json 的 windows 为空 ⇒ 必须在代码里显式建窗口，
            // 否则安卓端 wry 不会创建 WebView（现象：白屏 + 控件树只有空 FrameLayout）。
            tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::default())
                .title("MDC-RS")
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// 进程内启动完整引擎：与桌面 exe 完全同套路由/状态（mdc-server 库入口）。
async fn start_engine() -> anyhow::Result<()> {
    let state = mdc_server::build_state().await?;
    mdc_server::serve(state).await
}
