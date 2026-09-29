//! 人工精选的优先级：挑过之后**不再刮削**，即使所有源都挂了也能出片。
//!
//! 全程离线 —— 用一个「一被调用就报错并计数」的假源，
//! 所以能同时证明两件事：① 出片成功了；② 刮削**确实没被调用**。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use mdc_core::{
    cd2::Cd2Config,
    config::AppConfig,
    db,
    model::{OrganizeMode, VideoMeta},
    pipeline,
    scrape::{Engine, Provider, ScrapeCtx},
    strm::StrmConfig,
};

/// 一被调用就报错，并记下被调用了几次。
struct AlwaysFails {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for AlwaysFails {
    fn id(&self) -> &str {
        "always-fails"
    }
    fn label(&self) -> &str {
        "永远失败的源"
    }
    async fn search(&self, _c: &ScrapeCtx, _n: &str) -> anyhow::Result<Vec<VideoMeta>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("这个源永远失败（测试用）")
    }
}

struct Fixture {
    data_dir: PathBuf,
    mount_root: PathBuf,
    out_root: PathBuf,
    src: PathBuf,
}

/// 递归数文件个数（用来断言「失败时不留产物」）。
fn count_files(dir: &Path) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut n = 0;
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            n += count_files(&p);
        } else {
            n += 1;
        }
    }
    n
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("mdc_manual_{tag}_{}", std::process::id()));
        let f = Fixture {
            data_dir: root.join("data"),
            mount_root: root.join("mount/115"),
            out_root: root.join("out"),
            src: root.join("mount/115/看剧/MIDV-567 1080p.mp4"),
        };
        std::fs::create_dir_all(f.src.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&f.out_root).unwrap();
        std::fs::write(&f.src, b"fake").unwrap();
        f
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_pick_wins_and_skips_scraping() {
    let fx = Fixture::new("pick");
    std::env::set_var("MDC_CONFIG_PATH", &fx.data_dir);

    let mut cfg = AppConfig::default();
    cfg.common.timeout_secs = 5;
    cfg.scrape.disabled = vec!["javbus".into(), "fc2".into()];
    cfg.netdisk = Cd2Config {
        mount_root: fx.mount_root.to_string_lossy().to_string(),
        path_prefix: "115".into(),
        ..Default::default()
    };
    cfg.strm = StrmConfig {
        root: fx.out_root.to_string_lossy().to_string(),
        jobs: vec![fx.mount_root.join("看剧").to_string_lossy().to_string()],
        ..Default::default()
    };
    cfg.save().unwrap();

    let cfg = AppConfig::load().unwrap();
    let ctx = ScrapeCtx::from_config(&cfg).unwrap();
    let pool = db::init_pool(&fx.data_dir.join("mdc.db")).await.unwrap();

    let calls = Arc::new(AtomicUsize::new(0));
    let engine = Engine {
        providers: vec![Box::new(AlwaysFails { calls: calls.clone() })],
    };

    let task = db::TaskRow {
        id: 1,
        kind: "scrape".into(),
        status: "pending".into(),
        source_path: fx.src.to_string_lossy().to_string(),
        dest_path: None,
        number: Some("MIDV-567".into()),
        error: None,
        created_at: String::new(),
        updated_at: String::new(),
    };

    // ── 没有人工结果时：源挂了 ⇒ 任务必须失败 ──────────────────
    let out = pipeline::process_task(
        &pool,
        &engine,
        &ctx,
        &cfg,
        &task,
        OrganizeMode::Strm,
        Some(Path::new(&fx.out_root)),
    )
    .await
    .unwrap();
    assert_eq!(out.status, "failed", "源挂了就该失败：{}", out.message);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "这次确实调用了刮削");
    assert_eq!(count_files(&fx.out_root), 0, "失败时不该留下任何产物");

    // ── 存一条人工精选后：不该再刮削，而且要出片 ────────────────
    let manual = VideoMeta {
        number: "MIDV-567".into(),
        title: Some("人工挑的标题".into()),
        actors: vec!["人工演员".into()],
        release_date: chrono::NaiveDate::from_ymd_opt(2024, 5, 6),
        runtime_min: Some(90),
        studio: Some("人工片商".into()),
        source: Some("manual".into()),
        ..Default::default()
    };
    db::set_manual_meta(
        &pool,
        "MIDV-567",
        manual.title.as_deref(),
        &serde_json::to_string(&manual).unwrap(),
    )
    .await
    .unwrap();

    let before = calls.load(Ordering::SeqCst);
    let out2 = pipeline::process_task(
        &pool,
        &engine,
        &ctx,
        &cfg,
        &task,
        OrganizeMode::Strm,
        Some(Path::new(&fx.out_root)),
    )
    .await
    .unwrap();
    assert_eq!(out2.status, "done", "有人工结果就该成功：{}", out2.message);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        before,
        "🔴 人工精选存在时**不该再调用刮削**"
    );

    // 落点、NFO 内容都要来自人工结果
    let dest = out2.dest.expect("应该有落点");
    assert!(dest.exists(), "没写出 strm：{}", dest.display());
    let strm = std::fs::read_to_string(&dest).unwrap();
    assert!(strm.contains("%2F115%2F%E7%9C%8B%E5%89%A7%2F"), "strm 内容不对：{strm}");
    let nfo = dest.with_extension("nfo");
    assert!(nfo.exists(), "没写出 NFO");
    let nfo_txt = std::fs::read_to_string(&nfo).unwrap();
    assert!(nfo_txt.contains("人工挑的标题"), "NFO 没用人工资讯：{nfo_txt}");
    assert!(nfo_txt.contains("人工演员"), "NFO 没用人工资讯");

    // 目录名也该来自人工结果（actor = 人工演员）
    assert!(
        dest.to_string_lossy().contains("人工演员"),
        "落点目录没用人工资讯：{}",
        dest.display()
    );

    // ── 取消人工精选后又该回到刮削（并失败）────────────────────
    db::clear_manual_meta(&pool, "MIDV-567").await.unwrap();
    let before2 = calls.load(Ordering::SeqCst);
    let out3 = pipeline::process_task(
        &pool,
        &engine,
        &ctx,
        &cfg,
        &task,
        OrganizeMode::Strm,
        Some(Path::new(&fx.out_root)),
    )
    .await
    .unwrap();
    assert_eq!(out3.status, "failed", "取消人工后应恢复刮削并失败");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        before2 + 1,
        "取消人工后应重新调用刮削"
    );
}
