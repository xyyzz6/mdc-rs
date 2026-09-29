//! **各刮削源的线上实测**：默认 `#[ignore]`，手动跑，用来发现站点改版。
//!
//! ```bash
//! # 需要代理 —— javbus 与 FC2 国内直连都不通（实测 http_code=000）
//! MDC_PROXY=socks5://127.0.0.1:10808 \
//!   cargo test -p mdc-core --test providers_live -- --ignored --nocapture --test-threads=1
//! ```
//!
//! 为什么单独一个文件、并且默认 ignore：
//! - 它依赖**外网 + 代理**，放进常规测试会让 `cargo test` 随机红；
//! - 但选择器会随站点改版腐坏，没有它就只能靠用户报「刮不到了」才发现。
//!
//! ⚠️ 没设 `MDC_PROXY` 时**直接失败**而不是跳过 —— 会静默变绿的测试等于没有。

use mdc_core::config::AppConfig;
use mdc_core::scrape::{fc2::Fc2, javbus::JavBus, Provider, ScrapeCtx};

/// 线上测试专用重试。
///
/// 代理链路会偶发 `error decoding response body` / 超时这类**传输层**错误，
/// 那不是产品 bug。不重试的话，测试会随机红，久了就没人看它了。
async fn with_retry<T, F, Fut>(mut f: F) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    const TRIES: usize = 3;
    let mut last = None;
    for i in 1..=TRIES {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                eprintln!("第 {i}/{TRIES} 次失败（传输层，重试）：{e}");
                last = Some(e);
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
    Err(last.expect("TRIES 必须 > 0"))
}

/// 传输层错误（连接/解码/超时）—— 用来把「网络抖」和「产品 bug」区分开。
fn is_transport_error(msg: &str) -> bool {
    const MARKERS: &[&str] = &[
        "error decoding response body",
        "error sending request",
        "timed out",
        "connection",
        "proxy",
        "broken pipe",
        "unexpected eof",
    ];
    let lower = msg.to_lowercase();
    MARKERS.iter().any(|m| lower.contains(m))
}

fn ctx() -> ScrapeCtx {
    let proxy = std::env::var("MDC_PROXY").unwrap_or_else(|_| {
        panic!(
            "需要设置 MDC_PROXY（javbus 与 FC2 国内直连都不通）。\n\
             例：MDC_PROXY=socks5://127.0.0.1:10808 \
             cargo test -p mdc-core --test providers_live -- --ignored"
        )
    });
    let mut cfg = AppConfig::default();
    cfg.common.proxy = Some(proxy);
    cfg.common.timeout_secs = 30;
    ScrapeCtx::from_config(&cfg).expect("构建 HTTP 客户端失败")
}

// ───────────────────────────── javbus ─────────────────────────────

/// 真实番号必须把关键字段都刮到。
#[tokio::test]
#[ignore = "需要外网 + 代理，手动跑：MDC_PROXY=... cargo test --test providers_live -- --ignored"]
async fn javbus_real_number_scrapes_all_key_fields() {
    let p = JavBus::new();
    let ctx = ctx();
    let metas = with_retry(|| p.search(&ctx, "MIDV-567")).await.expect("刮削失败");
    assert_eq!(metas.len(), 1);
    let m = &metas[0];
    println!("{}", serde_json::to_string_pretty(m).unwrap());

    assert_eq!(m.number, "MIDV-567", "番号必须与请求一致");
    assert!(m.title.as_deref().unwrap_or("").contains("MIDV-567"), "标题里应有番号");
    assert!(!m.actors.is_empty(), "演员为空 —— div.star-name a 选择器可能失效了");
    assert!(!m.tags.is_empty(), "标签为空 —— span.genre a[href*='/genre/'] 可能失效了");
    assert!(m.release_date.is_some(), "发行日期为空 —— 信息栏选择器可能失效了");
    assert!(m.runtime_min.is_some(), "时长为空 —— 信息栏选择器可能失效了");
    assert!(m.studio.is_some(), "制作商为空 —— 信息栏选择器可能失效了");

    let cover = m.cover_url.as_deref().expect("封面为空 —— a.bigImage 可能失效了");
    assert!(cover.starts_with("https://"), "封面必须是绝对地址，实际 {cover}");
    assert!(!m.preview_urls.is_empty(), "预览图为空 —— a.sample-box 可能失效了");
}

/// 🔴 防刮错片：无论接受还是拒绝，**绝不能**返回一个番号对不上的结果。
///
/// 这组用的是实测会返回 30 条、且**第一条是别的番号**的模糊番号（搜 `SSIS-4` 第一条是 `SSIS-984`）。
/// 断言的是**安全性质**而不是「必须拒绝」——因为番号前导零只是排版，
/// `SSIS-4` 与 `SSIS-004` 是同一部，接受它是对的；接受 `SSIS-984` 才是灾难。
#[tokio::test]
#[ignore = "需要外网 + 代理"]
async fn javbus_never_returns_a_mismatched_number() {
    let p = JavBus::new();
    let ctx = ctx();
    for q in ["SSIS-4", "MIDV-5"] {
        let r = with_retry(|| p.search(&ctx, q)).await;
        match r {
            Ok(metas) => {
                assert!(!metas.is_empty());
                for m in &metas {
                    assert_eq!(
                        mdc_core::scrape::javbus::norm_number(&m.number),
                        mdc_core::scrape::javbus::norm_number(q),
                        "🔴 返回了番号对不上的结果：请求 {q}，得到 {}（这就是贴错元数据）",
                        m.number
                    );
                }
                println!("{q} → 接受了零填充等价番号 {}", metas[0].number);
            }
            Err(e) => {
                let msg = e.to_string();
                println!("{q} → 按预期拒绝：{msg}");
                assert!(
                    !is_transport_error(&msg),
                    "重试 3 次仍是传输层错误（网络/代理问题，不是产品问题）：{msg}"
                );
            }
        }
    }
}

/// javbus 不索引 FC2 —— 连请求都不该发（`supports()` 为 false）。
#[test]
fn javbus_declines_fc2_without_network() {
    assert!(!JavBus::new().supports("FC2-PPV-4680562"));
    assert!(JavBus::new().supports("MIDV-567"));
}

/// 無碼「日期式」番号（1pondo / Caribbeancom 那类）走通全链路：
/// **解析器从真实文件名抽出的番号 → 源能刮到**。
#[tokio::test]
#[ignore = "需要外网 + 代理"]
async fn date_style_uncensored_number_works_end_to_end() {
    // 先确认解析器能从常见命名里抽出日期式番号（厂牌前缀要能剥掉）
    for (file, want) in [
        ("091626-001.mp4", "091626-001"),
        ("Caribbeancom-091626-001.mp4", "091626-001"),
        ("xxx_091626-001_1080p.mp4", "091626-001"),
    ] {
        assert_eq!(
            mdc_core::parser::parse_filename(file).number.as_deref(),
            Some(want),
            "解析 {file}"
        );
    }

    let p = JavBus::new();
    let ctx = ctx();
    let metas = with_retry(|| p.search(&ctx, "091626-001"))
        .await
        .expect("日期式無碼番号刮削失败");
    assert_eq!(metas.len(), 1);
    let m = &metas[0];
    println!("{}", serde_json::to_string_pretty(m).unwrap());
    assert_eq!(m.number, "091626-001");
    assert_eq!(m.uncensored, Some(true), "無碼分区出来的必须标为无码");
    assert!(m.title.as_deref().unwrap_or("").contains("091626-001"));
    assert!(m.cover_url.is_some());
    assert!(m.release_date.is_some());
}

/// **無碼分区**：同一套选择器就能用（搜索前缀不同），但字段天生更少 ——
/// 没有演员、没有预览图（`star-div` 为空），封面在 `/imgs/cover/`。
#[tokio::test]
#[ignore = "需要外网 + 代理"]
async fn javbus_uncensored_section_scrapes_key_fields() {
    let p = JavBus::new();
    let ctx = ctx();
    let metas = with_retry(|| p.search(&ctx, "HEYZO-1673"))
        .await
        .expect("無碼分区刮削失败");
    assert_eq!(metas.len(), 1);
    let m = &metas[0];
    println!("{}", serde_json::to_string_pretty(m).unwrap());

    assert_eq!(m.number, "HEYZO-1673");
    assert!(m.title.as_deref().unwrap_or("").contains("HEYZO-1673"), "标题里应有番号");
    assert_eq!(
        m.uncensored,
        Some(true),
        "無碼分区出来的必须标为无码（靠分区判定，不靠标签猜）"
    );
    let cover = m.cover_url.as_deref().expect("封面为空 —— a.bigImage 可能失效了");
    assert!(cover.starts_with("https://"), "封面必须绝对地址，实际 {cover}");
    assert!(m.release_date.is_some(), "发行日期为空 —— 信息栏选择器可能失效了");
    assert!(m.runtime_min.is_some(), "时长为空 —— 信息栏选择器可能失效了");
    assert!(m.studio.is_some(), "制作商为空 —— 信息栏选择器可能失效了");
    // 这两条不是「应该为空」的断言，而是记录事实：無碼页确实没有这些区块
    println!(
        "（無碼页事实）演员 {} 个、预览图 {} 张",
        m.actors.len(),
        m.preview_urls.len()
    );
}

// ─────────────────────────────── FC2 ───────────────────────────────

/// FC2 真实商品：官方站必须把字段刮全。
#[tokio::test]
#[ignore = "需要外网 + 代理"]
async fn fc2_real_article_scrapes_key_fields() {
    let p = Fc2::new();
    let ctx = ctx();
    let metas = with_retry(|| p.search(&ctx, "FC2-PPV-4680562"))
        .await
        .expect("刮削失败");
    assert_eq!(metas.len(), 1);
    let m = &metas[0];
    println!("{}", serde_json::to_string_pretty(m).unwrap());

    assert_eq!(m.number, "FC2-PPV-4680562");
    let title = m.title.as_deref().unwrap_or("");
    assert!(!title.is_empty(), "标题为空 —— og:title / ld+json 可能失效了");
    assert!(
        !title.contains("*****"),
        "标题里混进了反爬噪声 —— 说明取到了被投毒的 <h3>，必须改回 og:title/ld+json：{title}"
    );
    assert!(!title.contains("FC2-PPV"), "番号前缀应被剥掉：{title}");

    let cover = m.cover_url.as_deref().expect("封面为空 —— ld+json image / og:image 可能失效了");
    assert!(cover.starts_with("https://"), "封面必须是绝对地址，实际 {cover}");
    assert!(m.release_date.is_some(), "上架时间为空 —— items_article_softDevice 可能失效了");
    assert!(m.runtime_min.is_some(), "时长为空 —— p.items_article_info 可能失效了");
    assert!(!m.tags.is_empty(), "标签为空 —— items_article_TagArea 可能失效了");
    assert!(!m.preview_urls.is_empty(), "预览图为空 —— SampleImagesArea 可能失效了");
    assert!(m.studio.is_some(), "卖家名为空 —— li.items_article_writer 可能失效了");
    assert_eq!(m.uncensored, Some(true), "FC2 是素人内容");
}

/// 🔴 FC2 对不存在的 id 返回 **HTTP 200 + 错误页**。
/// 必须识别出来并报错，否则就是「刮到了一个不存在的商品」这种静默假阳性。
#[tokio::test]
#[ignore = "需要外网 + 代理"]
async fn fc2_nonexistent_id_is_reported_not_silently_accepted() {
    let p = Fc2::new();
    let ctx = ctx();
    let r = with_retry(|| p.search(&ctx, "FC2-PPV-999999999")).await;
    match r {
        Err(e) => {
            let msg = e.to_string();
            println!("按预期报错：{msg}");
            assert!(!is_transport_error(&msg), "不该是传输层错误：{msg}");
            assert!(
                msg.contains("没有这个商品") || msg.contains("找不到"),
                "要说清是商品不存在，实际：{msg}"
            );
        }
        Ok(metas) => panic!(
            "不该返回结果！200 的错误页被当成正常页面了：{:?}",
            metas.iter().map(|m| m.title.clone()).collect::<Vec<_>>()
        ),
    }
}
