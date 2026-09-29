//! 目录监控：性能模式（inotify/实时）与兼容模式（轮询，适配网盘挂载）。

use anyhow::Result;
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchKind {
    /// 实时监听，适合本地磁盘
    Performance,
    /// 轮询扫描，适合网盘挂载
    Compatible,
}

pub struct DirectoryWatcher {
    /// 持有 watcher 本体防止被 drop 后停止监控
    #[allow(dead_code)]
    watcher: RecommendedWatcher,
    pub roots: Vec<PathBuf>,
}

impl DirectoryWatcher {
    /// 监听多个目录，新/改动的视频文件路径会推送进返回的 channel。
    pub fn start(
        dirs: &[PathBuf],
        kind: WatchKind,
        tx: mpsc::UnboundedSender<PathBuf>,
    ) -> Result<Self> {
        let (fs_tx, fs_rx) = std::sync::mpsc::channel::<notify::Result<notify::Event>>();
        let mut watcher: RecommendedWatcher = match kind {
            WatchKind::Performance => {
                RecommendedWatcher::new(fs_tx, Config::default())
            }
            WatchKind::Compatible => RecommendedWatcher::new(
                fs_tx,
                Config::default().with_poll_interval(Duration::from_secs(30)),
            ),
        }?;

        for d in dirs {
            if Path::new(d).exists() {
                watcher.watch(d, RecursiveMode::Recursive)?;
                tracing::info!(dir = %d.display(), kind = ?kind, "开始监控");
            } else {
                tracing::warn!(dir = %d.display(), "监控目录不存在，跳过");
            }
        }

        // 阻塞线程里转发到 tokio channel（notify 是同步回调）
        std::thread::spawn(move || {
            while let Ok(event) = fs_rx.recv() {
                match event {
                    Ok(ev) => {
                        for p in ev.paths {
                            if crate::parser::is_video_file(&p) && p.exists() {
                                let _ = tx.send(p);
                            }
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "文件系统事件错误"),
                }
            }
        });

        Ok(Self {
            watcher,
            roots: dirs.to_vec(),
        })
    }
}
