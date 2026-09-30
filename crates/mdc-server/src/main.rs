//! mdc-server 可执行入口：初始化日志 → 构建运行态 → 常驻服务。
//! 全部业务在 `lib.rs`（安卓壳复用同一套 `build_state`/`serve`）。

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let state = mdc_server::build_state().await?;
    mdc_server::serve(state).await
}
