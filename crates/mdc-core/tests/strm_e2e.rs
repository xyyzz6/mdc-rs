//! 端到端（**全离线、确定性**）：假 CD2 挂载 → 本地 mock 刮削源 → `.strm` + NFO → 增量跳过。
//!
//! 为什么要这一层：单元测试只能证明「URL 拼对了」，证明不了
//! 「扫到文件 → 刮削 → 落点渲染 → 写 .strm → 记 manifest → 第二轮跳过」这条链是通的。
//!
//! 两个关键设计：
//! - mock 刮削源是本测试自己起的 `TcpListener`，**不碰 javbus、不碰任何网盘**，
//!   靠 `scrape.disabled = ["javbus","fc2"]` 把内置源全关掉，结果完全确定；
//! - 假挂载根 + 假输出目录都在系统临时目录里，**绝不碰用户真实媒体库**。

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use mdc_core::{
    cd2::Cd2Config, config::AppConfig, db, pipeline, scrape,
    strm::{self, StrmConfig, StrmRunner},
};

/// 环境变量 `MDC_CONFIG_PATH` 是**进程级全局**的，而 cargo 默认多线程跑测试 ——
/// 不加锁的话两个测试会互相把数据目录指到对方那里去（表现为随机的怪失败）。
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn search_html(number: &str) -> String {
    format!(
        r#"<html><body>
<div class="item"><a href="/detail/{number}">{number} テスト</a></div>
</body></html>"#
    )
}

fn detail_html(number: &str) -> String {
    format!(
        r#"<html><body>
<h1 class="title">{number} テスト</h1>
<span class="number">{number}</span>
<span class="studio">MOODYZ</span>
<span class="date">2024-03-15</span>
<span class="actor"><a>三上悠亜</a></span>
<div class="cover"><img src="/img/cover.bmp"></div>
</body></html>"#
    )
}

/// 手搓一张 24 位 BMP（60x90，2:3）。
///
/// 用 BMP 而不是 PNG/JPEG：BMP 的头部是纯手写就能拼对的，
/// 而 `image::open` 认得它 —— 这样海报裁剪那条分支在测试里才真的跑到。
fn bmp_24(w: u32, h: u32) -> Vec<u8> {
    let row = (w * 3).div_ceil(4) * 4;
    let data_size = row * h;
    let file_size = 54 + data_size;
    let mut v: Vec<u8> = Vec::with_capacity(file_size as usize);
    v.extend_from_slice(b"BM");
    v.extend_from_slice(&file_size.to_le_bytes());
    v.extend_from_slice(&[0u8; 4]);
    v.extend_from_slice(&54u32.to_le_bytes());
    v.extend_from_slice(&40u32.to_le_bytes());
    v.extend_from_slice(&w.to_le_bytes());
    v.extend_from_slice(&h.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&24u16.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes());
    v.extend_from_slice(&data_size.to_le_bytes());
    v.extend_from_slice(&[0u8; 16]);
    v.resize(file_size as usize, 0x80);
    v
}

/// 极简 mock 刮削源：**按请求里的番号回不同元数据**。
///
/// 必须按番号区分 —— 否则两个视频刮出同一个 `number`，落点会撞成同一个文件，
/// 测试就测不出「一个落点被删只补一个」这类增量语义了。
fn spawn_mock_provider() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定 mock 端口失败");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 8192];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let target = req
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("/")
                .to_string();
            let number = if let Some(q) = target.split("q=").nth(1) {
                q.split('&').next().unwrap_or("UNKNOWN").to_string()
            } else if let Some(rest) = target.strip_prefix("/detail/") {
                rest.to_string()
            } else {
                "UNKNOWN".to_string()
            };

            // 封面图：走独立的二进制响应
            if target.starts_with("/img/") {
                let img = bmp_24(60, 90);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/bmp\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    img.len()
                );
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(&img);
                let _ = s.flush();
                continue;
            }

            let body = if target.starts_with("/search") {
                search_html(&number)
            } else {
                detail_html(&number)
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.as_bytes().len(),
                body
            );
            let _ = s.write_all(resp.as_bytes());
            let _ = s.flush();
        }
    });
    port
}

/// 自定义刮削源的 YAML（两个测试共用，避免两处写法漂移）。
fn provider_yaml(port: u16) -> String {
    format!(
        "name: local\n\
         base_url: http://127.0.0.1:{port}\n\
         search:\n\
         \x20 url: \"{{base_url}}/search?q={{number}}\"\n\
         \x20 result_selector: \"div.item a\"\n\
         detail:\n\
         \x20 title: \"h1.title\"\n\
         \x20 number: \"span.number\"\n\
         \x20 studio: \"span.studio\"\n\
         \x20 release_date: \"span.date\"\n\
         \x20 cover: \"div.cover img@src\"\n\
         \x20 actors: [\"span.actor a\"]\n"
    )
}

struct Fixture {
    root: PathBuf,
    data_dir: PathBuf,
    mount_root: PathBuf,
    out_root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("mdc_e2e_{tag}_{}", std::process::id()));
        // 不用 remove_dir_all（沙箱里删除会被垫片改道）：直接让开旧目录
        let root = if root.exists() {
            root.with_file_name(format!(
                "{}_b{}",
                root.file_name().unwrap().to_string_lossy(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
            ))
        } else {
            root
        };
        let f = Fixture {
            data_dir: root.join("data"),
            mount_root: root.join("mount/115"),
            out_root: root.join("out"),
            root,
        };
        std::fs::create_dir_all(&f.data_dir).unwrap();
        std::fs::create_dir_all(f.mount_root.join("看剧")).unwrap();
        std::fs::create_dir_all(f.root.join("mount/other")).unwrap();
        std::fs::create_dir_all(&f.out_root).unwrap();
        f
    }
}

fn write_video(p: &Path) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    // 内容无所谓 —— strm 模式一个字节都不读它，只算路径
    std::fs::write(p, b"fake video bytes").unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn strm_end_to_end() {
    let _guard = ENV_LOCK.lock().await;
    let port = spawn_mock_provider();
    let fx = Fixture::new("strm");

    // ── 假 CD2 挂载里的素材 ────────────────────────────────────
    let v1 = fx.mount_root.join("看剧/MIDV-567 1080p.mp4");
    let v2 = fx.mount_root.join("看剧/SSIS-424.mp4");
    write_video(&v1);
    write_video(&v2);
    // 挂在挂载根**之外**：直链算不出来，必须计入 failed 而不是硬拼一个 404 链接
    let outside = fx.root.join("mount/other/ABP-123.mp4");
    write_video(&outside);
    // 不是视频：必须被过滤掉，不能进 total
    std::fs::write(fx.mount_root.join("看剧/readme.txt"), b"x").unwrap();

    // ── 配置 ──────────────────────────────────────────────────
    std::env::set_var("MDC_CONFIG_PATH", &fx.data_dir);
    let mut cfg = AppConfig::default();
    cfg.common.timeout_secs = 5;
    // 关掉**全部内置源** → 全程离线（内置源现在有 javbus + fc2 两个）
    cfg.scrape.disabled = vec!["javbus".into(), "fc2".into()];
    cfg.netdisk = Cd2Config {
        host: "192.168.1.15".into(),
        port: 19798,
        mount_root: fx.mount_root.to_string_lossy().to_string(),
        path_prefix: "115".into(),
        ..Default::default()
    };
    cfg.strm = StrmConfig {
        root: fx.out_root.to_string_lossy().to_string(),
        // 第二个 job 故意在挂载根之外
        jobs: vec![
            fx.mount_root.join("看剧").to_string_lossy().to_string(),
            fx.root.join("mount/other").to_string_lossy().to_string(),
        ],
        recursive: true,
        ..Default::default()
    };
    cfg.save().unwrap();

    // 自定义刮削源（`_` 开头的文件会被跳过，所以这里不能加下划线）
    std::fs::create_dir_all(AppConfig::providers_dir()).unwrap();
    std::fs::write(AppConfig::providers_dir().join("local.yaml"), provider_yaml(port)).unwrap();

    let cfg = AppConfig::load().unwrap();
    let engine = scrape::Engine::load(&cfg);
    assert_eq!(
        engine.providers.len(),
        1,
        "内置源应已被 disabled 全部关掉，只剩自定义源"
    );
    let ctx = scrape::ScrapeCtx::from_config(&cfg).unwrap();
    let pool = db::init_pool(&fx.data_dir.join("mdc.db")).await.unwrap();
    let runner = StrmRunner::new(fx.data_dir.join("strm_last_run"));

    // ── 第一轮：期望 2 成功 + 1 失败（挂载根之外那个）──────────
    let s1 = pipeline::run_strm_jobs(&runner, &pool, &engine, &ctx, &cfg, false)
        .await
        .expect("第一轮失败");
    assert_eq!(s1.total, 3, "total 应为 3 个视频（txt 不该被算进来）");
    assert_eq!(s1.added, 2, "应写出 2 个 strm；错误：{:?}", s1.errors);
    assert_eq!(s1.skipped, 0);
    assert_eq!(s1.failed, 1, "挂载根之外那个必须失败");
    assert!(
        s1.errors.iter().any(|e| e.contains("不在 CD2 挂载根下")),
        "失败原因要说明白，实际：{:?}",
        s1.errors
    );

    // ── 断言 .strm 内容逐字节正确 ─────────────────────────────
    let want_url = "http://192.168.1.15:19798/static/http/localhost:19798/False/\
                    %2F115%2F%E7%9C%8B%E5%89%A7%2FMIDV-567%201080p.mp4";
    let strm1 = fx
        .out_root
        .join("三上悠亜/MIDV-567 テスト/MIDV-567 テスト.strm");
    assert!(
        strm1.exists(),
        "落点不存在：{}\n实际生成：{:?}",
        strm1.display(),
        walk(&fx.out_root)
    );
    let raw = std::fs::read(&strm1).unwrap();
    assert_eq!(
        raw,
        format!("\u{FEFF}{want_url}\n").as_bytes(),
        "strm 内容必须恰好是 BOM + URL + \\n"
    );
    // NFO 必须跟 .strm 同名同目录（Jellyfin/Emby 才认）
    let nfo = fx
        .out_root
        .join("三上悠亜/MIDV-567 テスト/MIDV-567 テスト.nfo");
    assert!(nfo.exists(), "NFO 缺失：{}", nfo.display());
    let nfo_txt = std::fs::read_to_string(&nfo).unwrap();
    assert!(nfo_txt.contains("MIDV-567"), "NFO 里没有番号");
    assert!(nfo_txt.contains("三上悠亜"), "NFO 里没有演员");

    // 海报：`<片名>.jpg` + `<片名>-poster.jpg`（Jellyfin/Emby 最认后者）
    let poster_dir = fx.out_root.join("三上悠亜/MIDV-567 テスト");
    let poster = poster_dir.join("MIDV-567 テスト.jpg");
    let poster2 = poster_dir.join("MIDV-567 テスト-poster.jpg");
    assert!(
        poster.exists(),
        "海报缺失：{}\n目录里：{:?}",
        poster.display(),
        walk(&fx.out_root)
    );
    assert!(poster2.exists(), "`-poster.jpg` 缺失：{}", poster2.display());
    // 必须是**按视频名**命名，不能是目录级 poster.jpg（模板扁平化时会互相覆盖）
    assert!(
        !poster_dir.join("poster.jpg").exists(),
        "不该写目录级 poster.jpg"
    );

    // manifest 必须落盘
    assert!(cfg.strm_manifest_path().exists(), "manifest 没写出来");

    // ── 第二轮：增量，一个都不该重写 ──────────────────────────
    let s2 = pipeline::run_strm_jobs(&runner, &pool, &engine, &ctx, &cfg, false)
        .await
        .expect("第二轮失败");
    assert_eq!(s2.total, 3);
    assert_eq!(s2.added, 0, "第二轮不该重写；错误：{:?}", s2.errors);
    assert_eq!(s2.skipped, 2, "两个已生成的应命中增量跳过");
    assert_eq!(s2.failed, 1);

    // ── 第三轮：force 全量重写 ────────────────────────────────
    let s3 = pipeline::run_strm_jobs(&runner, &pool, &engine, &ctx, &cfg, true)
        .await
        .expect("第三轮失败");
    assert_eq!(s3.added, 2, "force 应全量重写");

    // ── 落点被删 → 增量必须失效并补回来 ───────────────────────
    std::fs::remove_file(&strm1).unwrap();
    let s4 = pipeline::run_strm_jobs(&runner, &pool, &engine, &ctx, &cfg, false)
        .await
        .expect("第四轮失败");
    assert_eq!(s4.added, 1, "落点被删后应补写 1 个");
    assert_eq!(s4.skipped, 1);
    assert!(strm1.exists(), "补写失败");
}

/// 自定义源必须把封面补成**绝对地址** —— 站点普遍给相对路径（`/img/a.jpg`），
/// 不补的话下载会被 HTTP 客户端拒，表现是「元数据全对、海报永远没有」。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn custom_provider_resolves_relative_cover() {
    let _guard = ENV_LOCK.lock().await;
    let port = spawn_mock_provider();
    let fx = Fixture::new("cover");
    std::env::set_var("MDC_CONFIG_PATH", &fx.data_dir);
    std::fs::create_dir_all(AppConfig::providers_dir()).unwrap();
    std::fs::write(AppConfig::providers_dir().join("local.yaml"), provider_yaml(port)).unwrap();

    let mut cfg = AppConfig::default();
    cfg.scrape.disabled = vec!["javbus".into(), "fc2".into()];
    let engine = scrape::Engine::load(&cfg);
    let ctx = scrape::ScrapeCtx::from_config(&cfg).unwrap();

    let metas = engine.scrape(&ctx, "MIDV-567").await;
    assert_eq!(metas.len(), 1, "应该命中 1 条");
    let m = &metas[0];
    assert_eq!(
        m.cover_url.as_deref(),
        Some(format!("http://127.0.0.1:{port}/img/cover.bmp").as_str()),
        "封面没补成绝对地址（HTML 里给的是 /img/cover.bmp）"
    );
    assert_eq!(m.number, "MIDV-567");
    assert_eq!(m.actors, vec!["三上悠亜"]);
}

/// 图片管线单独验一遍：mock 的 BMP 能不能下下来、能不能裁成海报。
/// （端到端测试里这条分支的失败会被 `let _ =` 吞掉，只有这里能看到真实错误。）
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cover_download_and_crop_work() {
    let port = spawn_mock_provider();
    let d = std::env::temp_dir().join(format!("mdc_img_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();

    let cfg = AppConfig::default();
    let ctx = scrape::ScrapeCtx::from_config(&cfg).unwrap();
    let raw = d.join("cover.bmp");
    let url = format!("http://127.0.0.1:{port}/img/cover.bmp");
    mdc_core::image::download(&url, &ctx.http, &raw, None)
        .await
        .expect("下载封面失败");
    let n = std::fs::metadata(&raw).unwrap().len();
    assert!(n > 54, "下载到的不是完整 BMP，只有 {n} 字节");

    let poster = d.join("poster.jpg");
    mdc_core::image::crop_poster(&raw, &poster).expect("裁剪海报失败");
    assert!(poster.exists());
    assert!(std::fs::metadata(&poster).unwrap().len() > 0);
}

fn walk(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p.to_string_lossy().to_string());
        }
    }
    out
}

/// 空配置时必须明确报错，而不是静默「跑完 0 个」——静默会让用户以为功能坏了。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_jobs_is_a_loud_error() {
    let _guard = ENV_LOCK.lock().await;
    let fx = Fixture::new("empty");
    std::env::set_var("MDC_CONFIG_PATH", &fx.data_dir);
    let cfg = AppConfig::default();
    cfg.save().unwrap();
    let cfg = AppConfig::load().unwrap();
    let engine = scrape::Engine::load(&cfg);
    let ctx = scrape::ScrapeCtx::from_config(&cfg).unwrap();
    let pool = db::init_pool(&fx.data_dir.join("mdc.db")).await.unwrap();
    let runner = StrmRunner::new(fx.data_dir.join("strm_last_run"));

    let err = pipeline::run_strm_jobs(&runner, &pool, &engine, &ctx, &cfg, false)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("strm.jobs"), "错误信息要指向缺哪个配置，实际：{err}");
}

/// 🔴 定时器与手动触发共用单飞令牌：已有一轮在跑时，第二次必须**被拒**而不是并发打网盘。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_run_is_rejected() {
    let _guard = ENV_LOCK.lock().await;
    let fx = Fixture::new("busy");
    std::env::set_var("MDC_CONFIG_PATH", &fx.data_dir);
    write_video(&fx.mount_root.join("看剧/MIDV-567.mp4"));

    let mut cfg = AppConfig::default();
    cfg.netdisk.mount_root = fx.mount_root.to_string_lossy().to_string();
    cfg.strm.root = fx.out_root.to_string_lossy().to_string();
    cfg.strm.jobs = vec![fx.mount_root.join("看剧").to_string_lossy().to_string()];
    cfg.save().unwrap();
    let cfg = AppConfig::load().unwrap();
    let engine = scrape::Engine::load(&cfg);
    let ctx = scrape::ScrapeCtx::from_config(&cfg).unwrap();
    let pool = db::init_pool(&fx.data_dir.join("mdc.db")).await.unwrap();
    let runner = StrmRunner::new(fx.data_dir.join("strm_last_run"));

    // 模拟「定时那一轮正在跑」：手动把令牌抢住
    let held = runner.acquire().unwrap();
    let err = pipeline::run_strm_jobs(&runner, &pool, &engine, &ctx, &cfg, false)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("已有一轮"), "实际：{err}");

    // 放掉之后必须能正常跑 —— 证明不是永久卡死
    drop(held);
    let s = pipeline::run_strm_jobs(&runner, &pool, &engine, &ctx, &cfg, false)
        .await
        .expect("释放令牌后应该能跑");
    assert_eq!(s.total, 1);
    // 跑成功后 last_run 必须落盘（定时器靠它算下次时间）
    assert!(runner.last_run().is_some(), "成功一轮后应记 last_run");
    assert!(fx.data_dir.join("strm_last_run").exists(), "last_run 没落盘");
}

/// 配错（jobs 为空）时**不该**消耗掉这一轮的调度名额。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_error_does_not_consume_the_schedule_slot() {
    let _guard = ENV_LOCK.lock().await;
    let fx = Fixture::new("slot");
    std::env::set_var("MDC_CONFIG_PATH", &fx.data_dir);
    let cfg = AppConfig::default();
    cfg.save().unwrap();
    let cfg = AppConfig::load().unwrap();
    let engine = scrape::Engine::load(&cfg);
    let ctx = scrape::ScrapeCtx::from_config(&cfg).unwrap();
    let pool = db::init_pool(&fx.data_dir.join("mdc.db")).await.unwrap();
    let runner = StrmRunner::new(fx.data_dir.join("strm_last_run"));

    let _ = pipeline::run_strm_jobs(&runner, &pool, &engine, &ctx, &cfg, false).await;
    assert_eq!(runner.last_run(), None, "失败不该记 last_run");
    assert!(!runner.is_running(), "失败也必须释放令牌");
}

/// `strm` 模式忘了给落点根必须报错 —— 否则会写到网盘挂载目录里去。
#[test]
fn strm_without_out_root_errors() {
    let fx = Fixture::new("noroot");
    let src = fx.mount_root.join("看剧/MIDV-567.mp4");
    write_video(&src);
    let cfg = AppConfig::default();
    let task = db::TaskRow {
        id: 1,
        kind: "scrape".into(),
        status: "pending".into(),
        source_path: src.to_string_lossy().to_string(),
        dest_path: None,
        number: Some("MIDV-567".into()),
        error: None,
        created_at: String::new(),
        updated_at: String::new(),
    };
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let out = rt.block_on(async {
        let pool = db::init_pool(&fx.data_dir.join("t.db")).await.unwrap();
        let engine = scrape::Engine::load(&cfg);
        let ctx = scrape::ScrapeCtx::from_config(&cfg).unwrap();
        // 注意：没有 library_root
        pipeline::process_task(
            &pool,
            &engine,
            &ctx,
            &cfg,
            &task,
            mdc_core::model::OrganizeMode::Strm,
            None,
            None,
        )
        .await
    });
    let err = out.unwrap_err().to_string();
    assert!(err.contains("落点根目录"), "实际：{err}");
}

/// 直链与 strm 落点的相对路径都要能在 Windows 分隔符下工作。
#[test]
fn windows_separators_ok() {
    let cfg = Cd2Config {
        mount_root: "E:\\cd2\\115".into(),
        ..Default::default()
    };
    let url = mdc_core::cd2::build_url("E:\\cd2\\115\\电影\\ABP-123.mp4", &cfg).unwrap();
    assert!(url.contains("%2F%E7%94%B5%E5%BD%B1%2F"), "实际：{url}");
    let _ = strm::safe_filename_bytes("ABP-123 テスト", 200);
}

// ──────────────────────────────────────────────────────────────
// WebDAV 源：安卓端（无 FUSE，网盘不可能挂成本机目录）的唯一路径。
// 这一组就是「APK 上刮削 115」的离线替身。
// ──────────────────────────────────────────────────────────────

/// 最小 PROPFIND 服务：按「请求路径 → 207 响应体」应答，**不联网**。
fn spawn_fake_dav(routes: std::collections::HashMap<String, String>) -> u16 {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match s.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    Err(_) => break,
                }
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some((head, rest)) = text.split_once("\r\n\r\n") {
                    let len = head
                        .lines()
                        .find_map(|l| {
                            let low = l.to_ascii_lowercase();
                            low.strip_prefix("content-length:")
                                .map(|v| v.trim().to_string())
                        })
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(0);
                    if rest.len() >= len {
                        break;
                    }
                }
            }
            let text = String::from_utf8_lossy(&buf).to_string();
            let head = text.split_once("\r\n\r\n").map(|(h, _)| h).unwrap_or(&text);
            let first = head.lines().next().unwrap_or("").to_string();
            let url_path = first.split_whitespace().nth(1).unwrap_or("/").to_string();
            let empty = "<?xml version=\"1.0\"?><D:multistatus xmlns:D=\"DAV:\"></D:multistatus>";
            let body = routes.get(&url_path).cloned().unwrap_or_else(|| empty.to_string());
            let resp = format!(
                "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = s.write_all(resp.as_bytes());
            let _ = s.flush();
        }
    });
    port
}

/// 造一个 207 响应：`path` 是目录自身（带尾斜杠），`children` 是它的条目。
fn dav_resp(path: &str, children: &[(&str, bool)]) -> String {
    let mut s = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>");
    s.push_str("<D:multistatus xmlns:D=\"DAV:\">");
    s.push_str(&format!(
        "<D:response><D:href>{path}</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"
    ));
    for (name, is_dir) in children {
        let rt = if *is_dir { "<D:collection/>" } else { "" };
        s.push_str(&format!(
            "<D:response><D:href>{path}{name}</D:href><D:propstat><D:prop><D:displayname>{name}</D:displayname><D:resourcetype>{rt}</D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"
        ));
    }
    s.push_str("</D:multistatus>");
    s
}

/// 🔴 目录源 = WebDAV 时，整条管线（扫 → 刮 → 写 .strm → 记 manifest）必须照常跑通，
/// 而且**完全不碰本机挂载根** —— `netdisk.mount_root` 故意留空。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webdav_source_end_to_end() {
    let _guard = ENV_LOCK.lock().await;
    let port = spawn_mock_provider();
    let fx = Fixture::new("dav");

    let mut routes = std::collections::HashMap::new();
    // @eaDir 里塞一个视频：证明「元数据目录必须跳过」这条不是摆设。
    // 注意 key 是**请求行里的 URL**（客户端会把 `@` 编成 `%40`），href 那边用原文。
    routes.insert(
        "/dav/115/%E7%9C%8B%E5%89%A7/%40eaDir/".to_string(),
        dav_resp("/dav/115/%E7%9C%8B%E5%89%A7/@eaDir/", &[("junk.mp4", false)]),
    );
    routes.insert(
        "/dav/115/%E7%9C%8B%E5%89%A7/".to_string(),
        dav_resp(
            "/dav/115/%E7%9C%8B%E5%89%A7/",
            &[
                ("MIDV-567%201080p.mp4", false),
                ("SSIS-424.mp4", false),
                ("readme.txt", false),
                ("@eaDir/", true), // 元数据目录：必须被跳过
            ],
        ),
    );
    let dav_port = spawn_fake_dav(routes);

    std::env::set_var("MDC_CONFIG_PATH", &fx.data_dir);
    let mut cfg = AppConfig::default();
    cfg.common.timeout_secs = 5;
    cfg.scrape.disabled = vec!["javbus".into(), "fc2".into()];
    cfg.netdisk = Cd2Config {
        host: "192.168.1.15".into(),
        port: 19798,
        // 🔴 故意**不配挂载根**：webdav 源不需要它（安卓上根本不存在本机挂载）
        mount_root: String::new(),
        ..Default::default()
    };
    cfg.source = mdc_core::source::SourceConfig {
        kind: "webdav".into(),
        base: format!("http://127.0.0.1:{dav_port}/dav"),
        ..Default::default()
    };
    cfg.strm = StrmConfig {
        root: fx.out_root.to_string_lossy().to_string(),
        jobs: vec!["/115/看剧".into()],
        recursive: true,
        ..Default::default()
    };
    cfg.save().unwrap();

    std::fs::create_dir_all(AppConfig::providers_dir()).unwrap();
    std::fs::write(AppConfig::providers_dir().join("local.yaml"), provider_yaml(port)).unwrap();

    let cfg = AppConfig::load().unwrap();
    assert_eq!(cfg.source.kind().unwrap(), mdc_core::source::SourceKind::WebDav);
    let engine = scrape::Engine::load(&cfg);
    let ctx = scrape::ScrapeCtx::from_config(&cfg).unwrap();
    let pool = db::init_pool(&fx.data_dir.join("mdc.db")).await.unwrap();
    let runner = StrmRunner::new(fx.data_dir.join("strm_last_run"));

    let s = pipeline::run_strm_jobs(&runner, &pool, &engine, &ctx, &cfg, false)
        .await
        .expect("webdav 源这一轮不该失败");
    assert_eq!(s.total, 2, "txt 与 @eaDir 都不该算进来：{:?}", s.errors);
    assert_eq!(s.added, 2, "应写出 2 个 strm：{:?}", s.errors);
    assert_eq!(s.failed, 0);

    // 直链必须按**网盘内路径**拼出来（与本地挂载那条路逐字节一致）
    let want = "http://192.168.1.15:19798/static/http/localhost:19798/False/\
                %2F115%2F%E7%9C%8B%E5%89%A7%2FMIDV-567%201080p.mp4";
    let strm1 = fx
        .out_root
        .join("三上悠亜/MIDV-567 テスト/MIDV-567 テスト.strm");
    assert!(strm1.exists(), "没写出 strm：{}", strm1.display());
    assert_eq!(
        std::fs::read(&strm1).unwrap(),
        format!("\u{FEFF}{want}\n").as_bytes(),
        "webdav 源拼出的直链不对"
    );

    // 🔴 manifest 的 key 必须是**网盘内路径**：换挂载点 / 换目录源后增量依然命中
    let mf = std::fs::read_to_string(cfg.strm_manifest_path()).unwrap();
    assert!(
        mf.contains("\"/115/看剧/MIDV-567 1080p.mp4\""),
        "manifest key 应为网盘内路径，实际：{mf}"
    );

    // 第二轮：manifest 命中，一个都不该重写
    let s2 = pipeline::run_strm_jobs(&runner, &pool, &engine, &ctx, &cfg, false)
        .await
        .expect("第二轮失败");
    assert_eq!(s2.added, 0, "第二轮不该重写：{:?}", s2.errors);
    assert_eq!(s2.skipped, 2);

    // ── 换目录源：webdav → local（同一份库、同样的网盘内路径）─────
    // 🔴 增量必须**照样命中**：manifest 的 key 是网盘内路径，不是本机绝对路径。
    // 换成绝对路径做 key 时，换挂载点 / 换目录源就会全量重刷一遍（网盘流量翻倍）。
    write_video(&fx.mount_root.join("看剧/MIDV-567 1080p.mp4"));
    write_video(&fx.mount_root.join("看剧/SSIS-424.mp4"));
    let mut local = cfg.clone();
    local.netdisk.mount_root = fx.root.join("mount").to_string_lossy().to_string();
    local.source = mdc_core::source::SourceConfig::default();
    local.strm.jobs = vec![fx.mount_root.join("看剧").to_string_lossy().to_string()];
    local.save().unwrap();
    let local = AppConfig::load().unwrap();

    let s3 = pipeline::run_strm_jobs(&runner, &pool, &engine, &ctx, &local, false)
        .await
        .expect("切回本地源后不该失败");
    assert_eq!(s3.total, 2, "本地源也该扫到 2 个：{:?}", s3.errors);
    assert_eq!(s3.added, 0, "🔴 换源后不该全量重写：{:?}", s3.errors);
    assert_eq!(s3.skipped, 2, "换源后增量应仍命中");
}

