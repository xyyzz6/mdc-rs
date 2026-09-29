//! mdc-core：与界面无关的业务核心库。
//!
//! 设计目标：Docker / Windows exe / Android APK 三端共用同一份核心。
//! HTTP 服务、Tauri 壳都是本库的薄封装。
//!
//! 安全设计（吸取 mdc-ng 逆向分析教训）：
//! - JWT 密钥按安装实例随机生成并落盘，绝不硬编码；
//! - 任何读文件的接口必须过鉴权 + 路径白名单（媒体库目录内）；
//! - 桌面/移动端服务默认只绑定 127.0.0.1。

pub mod cd2;
pub mod config;
pub mod db;
pub mod image;
pub mod model;
pub mod monitor;
pub mod net;
pub mod nfo;
pub mod organize;
pub mod parser;
pub mod pipeline;
pub mod proxy;
pub mod scrape;
pub mod source;
pub mod strm;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
