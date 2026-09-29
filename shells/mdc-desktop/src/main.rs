//! Windows 桌面壳：启动内嵌服务（127.0.0.1:9208），WebView 打开同一端口。
//!
//! 构建前置：MSVC 工具链 + `npm run build`（web/dist）+ `cargo tauri build`。
//! 本 crate 不参与 workspace 日常编译（在根 Cargo.toml 里 exclude）。

fn main() {
    // 先把核心服务拉起来（后台线程）
    std::thread::spawn(|| {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            if let Err(e) = serve_local().await {
                eprintln!("内嵌服务启动失败: {e:#}");
            }
        });
    });

    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// mdc-server 的 main 是二进制；桌面壳直接复用核心库时需要一个可调用的
// serve 入口。v0.1 先用进程方式：编译产物中 mdc-server 与壳放一起，
// 由壳拉起子进程并打开窗口。这里保留 async 钩子位。
async fn serve_local() -> anyhow::Result<()> {
    let exe_dir = std::env::current_exe()?.parent().unwrap().to_path_buf();
    let server = exe_dir.join("mdc-server.exe");
    if server.exists() {
        std::process::Command::new(server)
            .env("MDC_BIND", "127.0.0.1:9208")
            .spawn()?;
    }
    Ok(())
}
