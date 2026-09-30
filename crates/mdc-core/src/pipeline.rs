//! 完整处理管线：扫描目录 → 解析番号 → 刮削 → 整理 → NFO → 落库。
//!
//! 被 mdc-server（HTTP 任务）和未来的 Tauri 壳共用。
//!
//! 两条入口：
//! - **本地目录**（`create_tasks_for_dir` + `process_pending`）：hard_link/copy/move/symlink/in_place；
//! - **网盘目录**（`run_strm_jobs`）：CD2 挂载 → 写 `.strm` 指针 + NFO + 海报，视频不搬。

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::AppConfig;
use crate::db::{self, TaskRow};
use crate::model::{OrganizeMode, TaskStatus, VideoMeta};
use crate::organize;
use crate::parser::{self, parse_filename};
use crate::scrape::{Engine, ScrapeCtx};
use sqlx::SqlitePool;

/// 单条任务的处理结果。
#[derive(Debug, Clone)]
pub struct ProcessOutcome {
    pub status: &'static str,
    /// 整理后的落点（`.strm` 模式下就是那个 .strm 文件）
    pub dest: Option<PathBuf>,
    pub message: String,
}

/// 扫描目录下所有视频文件（不递归进隐藏目录）。
pub fn scan_video_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            continue;
        }
        if parser::is_video_file(&p) {
            out.push(p);
        }
    }
    out.sort();
    out
}

/// 为目录中每个视频创建一条任务记录，返回创建数量。
pub async fn create_tasks_for_dir(pool: &SqlitePool, dir: &Path) -> Result<usize> {
    let mut n = 0;
    for file in scan_video_files(dir) {
        let parsed = parse_filename(&file.to_string_lossy());
        db::insert_task(pool, &file.to_string_lossy(), parsed.number.as_deref()).await?;
        n += 1;
    }
    Ok(n)
}

/// 无番号文件的 strm 降级生成（douyin-nas 模式）：不刮削，按源文件名写
/// `.strm` + 合成 NFO（标题/number = 文件名，让媒体库聚合各自独立）。
/// 落点：`<strm 根>/<网盘父目录名>/<文件名>.strm`（网盘路径拿不到就用「无番号」）。
/// 媒体库聚合的回退链（NFO uniqueid → 文件名解析 → 父目录名）里 NFO 最优先，
/// 每个文件的 uniqueid 都不同 ⇒ 一文件一条目，不会并成一坨。
async fn no_number_strm(
    pool: &SqlitePool,
    task: &TaskRow,
    cfg: &AppConfig,
    library_root: Option<&Path>,
    strm_cloud: Option<&str>,
) -> Result<ProcessOutcome> {
    use crate::model::VideoMeta;
    let root = library_root
        .ok_or_else(|| anyhow!("strm 模式必须指定落点根目录（strm.root）"))?;
    let source = PathBuf::from(&task.source_path);
    let stem = source
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unnamed")
        .to_string();
    let url = match strm_cloud {
        Some(cp) => crate::cd2::build_url_from_cloud(cp, &cfg.netdisk)?,
        None => crate::cd2::build_url(&task.source_path, &cfg.netdisk)?,
    };
    // 网盘父目录名做子目录（/115open/云下载/x.mp4 → 云下载/）
    let sub = strm_cloud
        .and_then(|cp| {
            crate::source::norm_dav_path(cp)
                .trim_end_matches('/')
                .rsplit('/')
                .nth(1)
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "无番号".to_string());
    let dest_dir = root.join(&sub);
    std::fs::create_dir_all(&dest_dir)?;
    let dest_file = dest_dir.join(format!("{stem}.strm"));
    crate::strm::write_strm(&dest_file, &url, cfg.strm.bom)?;

    // 合成 NFO：标题 = 文件名（number 也是它 —— XML 转义 write_nfo 内部已处理）
    let meta = VideoMeta {
        number: stem.clone(),
        title: Some(stem.clone()),
        ..Default::default()
    };
    let _ = crate::nfo::write_nfo_alongside(&meta, &dest_file);

    let meta_json = serde_json::to_string(&meta)?;
    db::upsert_video(pool, &stem, Some(&stem), &meta_json).await?;
    db::update_task(
        pool,
        task.id,
        TaskStatus::Done.as_str(),
        Some(&dest_file.to_string_lossy()),
        None,
    )
    .await?;
    tracing::info!(file = %stem, "无番号文件降级生成 strm（不刮削）");
    Ok(ProcessOutcome {
        status: TaskStatus::Done.as_str(),
        dest: Some(dest_file),
        message: format!("generated without number: {stem}"),
    })
}

/// 处理单条任务：解析 → 刮削 → 整理 → NFO → 落库。
///
/// `strm_cloud` 是**网盘内路径**（`/115/电影/x.mp4`），只在 `.strm` 模式下用：
/// 给了就直接用它拼直链，不给才从 `source_path` 反推。
/// 为什么必须由调用方给：安卓端网盘**没有本机路径**（无 FUSE），
/// 反推这一步在 APK 上根本无从下手（见 `source.rs` 头注释）。
pub async fn process_task(
    pool: &SqlitePool,
    engine: &Engine,
    ctx: &ScrapeCtx,
    cfg: &AppConfig,
    task: &TaskRow,
    mode: OrganizeMode,
    library_root: Option<&Path>,
    strm_cloud: Option<&str>,
) -> Result<ProcessOutcome> {
    // 配置校验放最前面：配错了就别去刮削了（白打网络请求，还会让报错延迟到最后）
    if mode == OrganizeMode::Strm && library_root.is_none() {
        return Err(anyhow!("strm 模式必须指定落点根目录（strm.root）"));
    }

    let source = PathBuf::from(&task.source_path);
    let parsed = parse_filename(&task.source_path);
    let number = match &parsed.number {
        Some(n) => n.clone(),
        None => {
            // 🔴 无番号文件在 strm 模式下**降级生成**（douyin-nas 模式）：
            //    网盘里大量文件名根本没有番号（网盘转存/素人命名），全跳过
            //    就是「strm 生成不了」。不刮削，按文件名直接写指针 + 合成
            //    NFO（标题=文件名），媒体库照样可看。其他模式维持跳过。
            if mode == OrganizeMode::Strm {
                return no_number_strm(pool, task, cfg, library_root, strm_cloud).await;
            }
            db::update_task(pool, task.id, TaskStatus::Failed.as_str(), None, Some("无法识别番号"))
                .await?;
            return Ok(ProcessOutcome {
                status: TaskStatus::Failed.as_str(),
                dest: None,
                message: "skipped: no number".into(),
            });
        }
    };

    db::update_task(pool, task.id, TaskStatus::Running.as_str(), None, None).await?;

    // 🔴 人工精选优先：用户在多源结果里挑过的那条直接采用，**不再刮削**。
    // 两个好处：① 源站全挂 / 番号谁都搜不到时人工挑一条也能出片；
    //          ② 用户已经明确选过了，就别拿自动结果去覆盖他。
    let meta: VideoMeta = match db::get_manual_meta(pool, &number).await? {
        Some(json) => {
            let m: VideoMeta = serde_json::from_str(&json)
                .map_err(|e| anyhow!("人工精选的元数据解析失败（可能来自旧版本）：{e}"))?;
            tracing::info!(number = %number, "使用人工精选的元数据");
            m
        }
        None => {
            let scrape_result = engine.scrape(ctx, &number).await;
            match scrape_result.into_iter().next() {
                Some(m) => m,
                None => {
                    db::update_task(
                        pool,
                        task.id,
                        TaskStatus::Failed.as_str(),
                        None,
                        Some("所有刮削源未命中"),
                    )
                    .await?;
                    return Ok(ProcessOutcome {
                        status: TaskStatus::Failed.as_str(),
                        dest: None,
                        message: "failed: no metadata".into(),
                    });
                }
            }
        }
    };

    // 目标路径渲染
    // 标题去重：多数站点标题自带番号前缀，模板里再拼 {number} 会重复
    let title_raw = meta
        .title
        .clone()
        .unwrap_or_else(|| parsed.title_hint.clone().unwrap_or_default());
    let title_clean = title_raw
        .strip_prefix(&meta.number)
        .map(|s| s.trim_start_matches(|c: char| c.is_whitespace() || c == '　' || c == '-')
            .to_string())
        .unwrap_or(title_raw);

    let mut vars: HashMap<&str, String> = HashMap::new();
    vars.insert("number", meta.number.clone());
    vars.insert("title", title_clean);
    vars.insert(
        "actor",
        meta.actors.first().cloned().unwrap_or_else(|| "Unknown".into()),
    );
    vars.insert("studio", meta.studio.clone().unwrap_or_default());
    vars.insert("series", meta.series.clone().unwrap_or_default());
    vars.insert(
        "year",
        meta.release_date.map(|d| d.format("%Y").to_string()).unwrap_or_default(),
    );
    let folder = organize::render_name(&cfg.naming.folder_template, &vars)?;
    let file = organize::render_name(&cfg.naming.file_template, &vars)?;
    let ext = source.extension().and_then(|e| e.to_str()).unwrap_or("mp4");

    let dest_dir: PathBuf = match (mode, library_root) {
        (OrganizeMode::InPlace, _) => source
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".")),
        // strm 模式必须知道落点根（上面已校验过，这里只是取值）
        (OrganizeMode::Strm, _) => library_root
            .ok_or_else(|| anyhow!("strm 模式必须指定落点根目录（strm.root）"))?
            .join(&folder),
        (_, Some(root)) => root.join(&folder),
        (_, None) => source
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".")),
    };
    let dest_file = match mode {
        OrganizeMode::Strm => dest_dir.join(format!("{file}.strm")),
        _ => dest_dir.join(format!("{file}.{ext}")),
    };

    match mode {
        // 网盘：视频一个字节都不搬，只写一个几十字节的指针文件
        OrganizeMode::Strm => {
            let url = match strm_cloud {
                Some(cp) => crate::cd2::build_url_from_cloud(cp, &cfg.netdisk)?,
                None => crate::cd2::build_url(&task.source_path, &cfg.netdisk)?,
            };
            crate::strm::write_strm(&dest_file, &url, cfg.strm.bom)?;
        }
        _ => organize::apply(mode, &source, &dest_file)?,
    }

    // NFO + 海报：原地模式写在源文件旁边（跟随源文件名），其余模式跟目标文件
    let anchor = match mode {
        OrganizeMode::InPlace => source.clone(),
        _ => dest_file.clone(),
    };
    let nfo_path = crate::nfo::write_nfo_alongside(&meta, &anchor)?;
    if let Some(cover) = &meta.cover_url {
        let tmp =
            std::env::temp_dir().join(format!("mdc_cover_{}.jpg", uuid::Uuid::new_v4()));
        if crate::image::download(cover, &ctx.http, &tmp, meta.website.as_deref())
            .await
            .is_ok()
        {
            let poster = anchor.with_extension("jpg");
            let _ = crate::image::crop_poster(&tmp, &poster);
            // 网盘场景再写一份 `<片名>-poster.jpg`：Jellyfin/Emby 对这个名字最认，
            // 而用户是在另一个程序里看结果、不好排查，多一个文件换确定性划算。
            // ⚠️ 只按**视频名**命名，绝不用目录级 `poster.jpg` ——
            // 模板扁平化时同一目录会有多个视频，目录级海报会互相覆盖。
            if mode == OrganizeMode::Strm {
                if let Some(stem) = anchor.file_stem().and_then(|s| s.to_str()) {
                    let _ = std::fs::copy(&poster, anchor.with_file_name(format!("{stem}-poster.jpg")));
                }
            }
            let _ = std::fs::remove_file(&tmp);
        } else {
            tracing::warn!(cover = %cover, "封面下载失败（可能被防盗链拦截）");
        }
    }

    // 落库
    let meta_json = serde_json::to_string(&meta)?;
    db::upsert_video(pool, &meta.number, meta.title.as_deref(), &meta_json).await?;
    db::update_task(
        pool,
        task.id,
        TaskStatus::Done.as_str(),
        Some(&dest_file.to_string_lossy()),
        None,
    )
    .await?;

    Ok(ProcessOutcome {
        status: TaskStatus::Done.as_str(),
        dest: Some(dest_file.clone()),
        message: format!(
            "ok: {} -> {} (nfo: {})",
            meta.number,
            dest_file.display(),
            nfo_path.display()
        ),
    })
}

/// 处理所有 pending 任务。
pub async fn process_pending(
    pool: &SqlitePool,
    engine: &Engine,
    ctx: &ScrapeCtx,
    cfg: &AppConfig,
    mode: OrganizeMode,
    library_root: Option<&Path>,
) -> Result<usize> {
    let tasks = db::list_tasks(pool)
        .await?
        .into_iter()
        .filter(|t| t.status == TaskStatus::Pending.as_str())
        .collect::<Vec<_>>();
    let n = tasks.len();
    for task in tasks {
        if let Err(e) =
            process_task(pool, engine, ctx, cfg, &task, mode, library_root, None).await
        {
            tracing::error!(task_id = task.id, error = %e, "任务处理失败");
        }
    }
    Ok(n)
}

/// 网盘（`.strm`）一轮运行的结果统计。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct StrmRunStats {
    /// 扫到的视频总数（一开始就定下来，前端进度条才有分母）
    pub total: usize,
    /// 新写出 `.strm` 的数量
    pub added: usize,
    /// 增量命中、跳过（**既没重刮、也没打网盘**）
    pub skipped: usize,
    /// 处理失败的数量
    pub failed: usize,
    /// 落点根目录
    pub out_root: String,
    /// 前若干条错误信息（UI 展示用，不刷屏）
    pub errors: Vec<String>,
}

/// 扫描配置里的网盘目录 → 生成 `.strm` + NFO + 海报。
///
/// 两阶段（douyin-nas §55 的做法）：先把所有目录扫完拿到 total，
/// 再逐个处理 —— 否则进度条没有分母。
///
/// 目录从哪来由 [`crate::source::DirSource`] 决定：桌面/Docker 是本地挂载，
/// 安卓（无 FUSE）只能走 CD2 的 WebDAV。上层不再碰 `Path::is_dir()`。
///
/// 增量语义：manifest 里签名（= `.strm` 内容）没变、且落点文件还在 → **整条跳过**。
/// 签名只跟网盘路径有关，所以跳过时**连刮削都不做** —— 零网盘流量、零刮削请求。
/// 🔴 manifest 的 key 用**网盘内路径**而不是本机绝对路径：
/// 同一个库从 local 切到 webdav（或换挂载点）时增量依然命中，不必全量重刷。
pub async fn run_strm_jobs(
    runner: &crate::strm::StrmRunner,
    pool: &SqlitePool,
    engine: &Engine,
    ctx: &ScrapeCtx,
    cfg: &AppConfig,
    force: bool,
) -> Result<StrmRunStats> {
    // 配置校验放在抢坑**之前**：配错了不该消耗掉这一轮的调度名额
    // （否则定时任务要干等一个完整周期才重试）。
    if cfg.strm.jobs.is_empty() {
        return Err(anyhow!("还没有配置要监控的网盘目录（strm.jobs 为空）"));
    }
    // 目录源自身做校验：local 必须有挂载根，webdav 必须有 base
    let source = cfg.dir_source()?;

    // 单飞令牌：丢掉即释放（含 panic 展开路径）。定时器和手动触发共用它，
    // 所以两者不会同时打网盘 —— 这也是「反复重扫触发风控」的防线之一。
    let _guard = runner.acquire()?;

    let out_root = cfg.strm_root();
    std::fs::create_dir_all(&out_root)?;
    let manifest_path = cfg.strm_manifest_path();
    let mut manifest = crate::strm::Manifest::load(&manifest_path);

    // ── 阶段一：扫描 ──────────────────────────────────────────
    let mut sources: Vec<crate::source::DirEntry> = Vec::new();
    for job in &cfg.strm.jobs {
        let job = job.trim();
        if job.is_empty() {
            continue;
        }
        if !source.exists_dir(job).await? {
            tracing::warn!(dir = job, kind = %source.kind(), "网盘监控目录不存在（挂载没起来？），跳过");
            continue;
        }
        match source.list(job, cfg.strm.recursive, cfg.strm.max_depth).await {
            Ok(v) => sources.extend(v),
            Err(e) => {
                // 单个 job 列不出来只跳过这一个 —— 一轮里别的目录照样要跑完
                tracing::warn!(dir = job, error = %e, "列出网盘目录失败，跳过");
            }
        }
    }
    sources.sort_by(|a, b| a.path.cmp(&b.path));
    sources.dedup_by(|a, b| a.path == b.path);

    let mut stats = StrmRunStats {
        total: sources.len(),
        out_root: out_root.to_string_lossy().to_string(),
        ..Default::default()
    };

    // ── 阶段二：逐条处理 ──────────────────────────────────────
    // 无番号的文件也照常进 process_task —— strm 模式下降级生成（见其内注释）
    for entry in &sources {
        let src_str = entry.path.clone();
        let parsed = parse_filename(&src_str);

        // 网盘路径算不出来（不在挂载根下 / 服务端没给）就别建任务 —— 建了也是白失败
        let cloud = match source.cloud_path(&src_str) {
            Ok(c) => c,
            Err(e) => {
                stats.failed += 1;
                if stats.errors.len() < 20 {
                    stats.errors.push(format!("{e}"));
                }
                continue;
            }
        };
        let url = match crate::cd2::build_url_from_cloud(&cloud, &cfg.netdisk) {
            Ok(u) => u,
            Err(e) => {
                stats.failed += 1;
                if stats.errors.len() < 20 {
                    stats.errors.push(format!("{e}"));
                }
                continue;
            }
        };

        if !force && manifest.is_fresh(&cloud, &url) {
            stats.skipped += 1;
            continue;
        }

        let task_id = db::insert_task(pool, &src_str, parsed.number.as_deref()).await?;
        // 直接构造（insert 后字段是确定的），别 list_tasks 全表 —— 大目录下这是 O(n²)
        let task = TaskRow {
            id: task_id,
            kind: String::new(),
            status: "pending".into(),
            source_path: src_str.clone(),
            dest_path: None,
            number: parsed.number.clone(),
            error: None,
            created_at: String::new(),
            updated_at: String::new(),
        };

        match process_task(
            pool,
            engine,
            ctx,
            cfg,
            &task,
            OrganizeMode::Strm,
            Some(&out_root),
            Some(&cloud),
        )
        .await
        {
            Ok(outcome) => {
                if let Some(dest) = &outcome.dest {
                    manifest.put(&cloud, &dest.to_string_lossy(), &url);
                    stats.added += 1;
                } else {
                    stats.failed += 1;
                    if stats.errors.len() < 20 {
                        stats.errors.push(format!("{src_str}: {}", outcome.message));
                    }
                }
            }
            Err(e) => {
                stats.failed += 1;
                if stats.errors.len() < 20 {
                    stats.errors.push(format!("{src_str}: {e}"));
                }
            }
        }
    }

    manifest.save(&manifest_path)?;
    // 只有跑到这里才算一轮成功 —— 失败不记时间戳（见 StrmRunner::mark_done 的注释）
    runner.mark_done();
    tracing::info!(
        total = stats.total,
        added = stats.added,
        skipped = stats.skipped,
        failed = stats.failed,
        "strm 一轮完成"
    );
    Ok(stats)
}
