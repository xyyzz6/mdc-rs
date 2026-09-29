//! JavBus 参考实现（有码 + 無碼两个分区）。
//!
//! 选择器基于 2026-09 对线上页面的实测（zh-TW 站，需代理，见下）。
//!
//! ## 实测事实（改这个文件前先看）
//!
//! | 事实 | 说明 |
//! | --- | --- |
//! | **必须走代理** | 国内直连 `www.javbus.com` 连不上（实测 `http_code=000`）。配置 `common.proxy` |
//! | 搜索会 302 一次 | 是设 cookie/年龄门的跳转；`ScrapeCtx` 开了 `cookie_store`，跟随即可 |
//! | **模糊搜索会把别的番号排前面** | 实测搜 `SSIS-4` 返回 30 条，第一条是 `SSIS-984` |
//! | CD/日期变体 slug | `MIDV-012` 的结果 slug 是 `MIDV-012_2025-02-04` |
//! | **不索引 FC2** | 有碼站与 `/uncensored` 搜索 FC2 均为 0 结果 |
//! | **無碼分区结构一致** | 同一套 `a.movie-box` / `<h3>` / `a.bigImage` / `div.col-md-3.info`；<br>但**没有预览图、没有演员**（`star-div` 为空），封面路径是 `/imgs/cover/`（有码是 `/pics/cover/`） |
//!
//! 🔴 因此**必须校验番号**：只取「slug 与请求番号一致」的候选，一个都没有就拒绝。
//! 贴错元数据（错标题/错演员/错海报/错目录名）比失败严重得多，而且是**静默**的。
//!
//! 番号形态：带连字符的（`MIDV-567` / `HEYZO-1673`）两个分区都能搜；
//! 無碼里的**日期式** slug（`092426_001`、`091626-001`）当前 parser 认不出来，会停在「无法识别番号」——
//! 那是 parser 的前缀表问题（v0.2），不是这个源的问题。

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use scraper::{Html, Selector};

use crate::model::VideoMeta;

use super::{Provider, ScrapeCtx};

/// 搜索页最多看多少个候选（够用了，避免误抓整页）
const MAX_CANDIDATES: usize = 30;
/// 报错时列出几个候选 slug
const SHOW_CANDIDATES: usize = 5;

/// javbus 的两个分区。**只有搜索 URL 前缀和「是否无码」不同，解析逻辑完全一样。**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Censored,
    Uncensored,
}

impl Section {
    /// 按顺序尝试的分区：有码内容多得多，先搜它。
    pub const ORDER: [Section; 2] = [Section::Censored, Section::Uncensored];

    fn search_url(self, number: &str) -> String {
        match self {
            Section::Censored => {
                format!("{}/search/{}&type=0&parent=ce", JavBus::base(), number)
            }
            Section::Uncensored => {
                format!("{}/uncensored/search/{}&type=0&parent=ce", JavBus::base(), number)
            }
        }
    }

    fn label(self) -> &'static str {
        match self {
            Section::Censored => "有码分区",
            Section::Uncensored => "無碼分区",
        }
    }

    /// 無碼分区出来的结果**一定**是无码的 —— 这比靠标签里有没有「無碼」二字去猜可靠。
    fn is_uncensored(self) -> bool {
        matches!(self, Section::Uncensored)
    }
}

pub struct JavBus;

impl JavBus {
    pub fn new() -> Self {
        Self
    }

    fn base() -> &'static str {
        "https://www.javbus.com"
    }

    fn absolutize(url: &str) -> String {
        if url.starts_with("http") {
            url.to_string()
        } else {
            format!("{}{}", Self::base(), url)
        }
    }

    async fn get_text(ctx: &ScrapeCtx, url: &str) -> Result<String> {
        Ok(ctx
            .http
            .get(url)
            .header("accept-language", "zh-CN,zh;q=0.9,en;q=0.6")
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?)
    }

    /// 在某个分区里找一条：搜索 → 挑番号一致的候选 → 抓详情 → 解析。
    async fn try_section(
        ctx: &ScrapeCtx,
        number: &str,
        section: Section,
    ) -> Result<VideoMeta> {
        let search_text = Self::get_text(ctx, &section.search_url(number)).await?;
        let candidates = parse_candidates(&search_text);

        if candidates.is_empty() {
            return Err(anyhow!(
                "{}",
                diagnose_no_result(&search_text, number, section)
            ));
        }

        // 🔴 只认番号对得上的那个候选（模糊搜索的第一条经常是别的番号）
        let Some(link) = pick_candidate(&candidates, number) else {
            let shown: Vec<&str> = candidates
                .iter()
                .take(SHOW_CANDIDATES)
                .map(|u| slug_of(u))
                .collect();
            return Err(anyhow!(
                "搜到的都不是 {number}（共 {} 条，前几条：{}）",
                candidates.len(),
                shown.join("、")
            ));
        };

        let detail_text = Self::get_text(ctx, &link).await?;
        parse_detail(&detail_text, number, &link, section)
    }
}

impl Default for JavBus {
    fn default() -> Self {
        Self::new()
    }
}

/// 番号归一化：只留字母数字、大写，并**去掉每段数字的前导零**。
///
/// 抹平 `SSIS-424` / `ssis424` / `SSIS_424` / `SSIS00424` / `SSIS-0424` 的差异 ——
/// 番号里的前导零只是排版（`MIDV-012` 与 `MIDV-12` 是同一部），
/// 不抹平的话「请求 `MIDV00567`、站点写 `MIDV-567`」会被误判成番号不符而拒掉。
pub fn norm_number(s: &str) -> String {
    fn flush(out: &mut String, digits: &mut String) {
        if digits.is_empty() {
            return;
        }
        let trimmed = digits.trim_start_matches('0');
        out.push_str(if trimmed.is_empty() { "0" } else { trimmed });
        digits.clear();
    }
    let mut out = String::with_capacity(s.len());
    let mut digits = String::new();
    for c in s.chars() {
        if !c.is_ascii_alphanumeric() {
            continue;
        }
        let c = c.to_ascii_uppercase();
        if c.is_ascii_digit() {
            digits.push(c);
        } else {
            flush(&mut out, &mut digits);
            out.push(c);
        }
    }
    flush(&mut out, &mut digits);
    out
}

/// 详情页 URL 的 slug：`https://www.javbus.com/MIDV-012_2025-02-04` → `MIDV-012_2025-02-04`
pub fn slug_of(url: &str) -> &str {
    url.trim_end_matches('/').rsplit('/').next().unwrap_or(url)
}

/// slug 去掉 **CD/日期变体后缀**：`MIDV-012_2025-02-04` → `MIDV-012`
///
/// ⚠️ **不能无条件按 `_` 切**：無碼日期式番号的 `_002` 是**番号本体**
/// （`080918_002` 与 `080918_001` 是两部不同的片子），切掉就永远匹配不上。
/// 所以只剥「长得像日期」的后缀。
pub fn slug_number(slug: &str) -> &str {
    match slug.split_once('_') {
        Some((head, tail)) if is_date_suffix(tail) => head,
        _ => slug,
    }
}

/// `2025-02-04` 这种形态才算变体后缀（长度 10、第 5/8 位是 `-`、其余是数字）。
fn is_date_suffix(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// 从搜索页 HTML 里收集候选详情页链接（绝对地址）。
pub fn parse_candidates(html: &str) -> Vec<String> {
    let page = Html::parse_document(html);
    let Ok(sel) = Selector::parse("a.movie-box") else {
        return Vec::new();
    };
    page.select(&sel)
        .filter_map(|e| e.value().attr("href"))
        .take(MAX_CANDIDATES)
        .map(JavBus::absolutize)
        .collect()
}

/// 从候选里挑出与请求番号**一致**的那个（纯函数，便于离线测试）。
///
/// 不返回 `candidates[0]` 兜底 —— 见文件头的实测事实，第一条经常是别的番号。
pub fn pick_candidate(candidates: &[String], number: &str) -> Option<String> {
    let want = norm_number(number);
    candidates
        .iter()
        .find(|u| norm_number(slug_number(slug_of(u))) == want)
        .cloned()
}

/// FC2 专用的错误信息（javbus 压根不索引它）。
///
/// ⚠️ 必须**独立于页面内容**判断：早退时手上还没有页面，
/// 拿空字符串去走「页面像不像搜索页」的分支，会把 FC2 误报成「被 Cloudflare 拦了」。
fn fc2_message(number: &str) -> String {
    format!(
        "javbus 不索引 FC2 系列（实测有碼站与 /uncensored 搜索均为 0 结果），\
         {number} 需要另配数据源"
    )
}

/// 一条结果都没有时，给个**能诊断**的错误，而不是含糊的「无结果」。
fn diagnose_no_result(search_text: &str, number: &str, section: Section) -> String {
    if norm_number(number).starts_with("FC2") {
        return fc2_message(number);
    }
    let looks_like_search_page =
        search_text.contains("id=\"waterfall\"") || search_text.contains("id=\"search-input\"");
    if !looks_like_search_page {
        return format!(
            "javbus 返回的不是搜索页（{} 字节）—— 多半被 Cloudflare / 年龄验证拦了，\
             或者没配代理（国内直连不通，common.proxy 填 socks5://127.0.0.1:10808 之类）",
            search_text.len()
        );
    }
    format!("{}里搜不到 {number}", section.label())
}

fn sel<'a>(html: &'a Html, selector: &str) -> Option<scraper::ElementRef<'a>> {
    Selector::parse(selector).ok().and_then(|s| html.select(&s).next())
}

fn sel_text(html: &Html, selector: &str) -> Option<String> {
    sel(html, selector).map(|e| e.text().collect::<String>().trim().to_string())
}

fn sel_attr(html: &Html, selector: &str, attr: &str) -> Option<String> {
    sel(html, selector).and_then(|e| e.value().attr(attr).map(|a| a.to_string()))
}

/// 解析详情页。**纯函数**（输入 HTML 文本），所以能离线测。
///
/// 两个分区共用这一份 —— 实测它们的 `<h3>` / `a.bigImage` / `div.col-md-3.info` 结构一致；
/// 無碼页只是没有预览图和演员，那些选择器自然取空，不需要分叉。
pub fn parse_detail(
    detail_text: &str,
    number: &str,
    link: &str,
    section: Section,
) -> Result<VideoMeta> {
    let detail = Html::parse_document(detail_text);
    let mut meta = VideoMeta {
        number: number.to_string(),
        website: Some(link.to_string()),
        source: Some("javbus".to_string()),
        ..Default::default()
    };
    meta.title = sel_text(&detail, "h3");
    // 封面是 <a class="bigImage" href="/pics/cover/x_b.jpg">（無碼是 /imgs/cover/），相对路径需转绝对
    meta.cover_url = sel_attr(&detail, "a.bigImage", "href").map(|h| JavBus::absolutize(&h));
    // 预览图：a.sample-box href 直链图床（無碼页没有这个区块，自然为空）
    if let Ok(sel) = Selector::parse("a.sample-box") {
        for el in detail.select(&sel) {
            if let Some(href) = el.value().attr("href") {
                meta.preview_urls.push(JavBus::absolutize(href));
            }
        }
    }
    // 信息栏：<div class="col-md-3 info"><p><span class="header">label:</span> value</p>
    let mut site_number: Option<String> = None;
    if let Ok(info_sel) = Selector::parse("div.col-md-3.info p") {
        for p in detail.select(&info_sel) {
            let label = p
                .select(&Selector::parse("span.header").unwrap())
                .next()
                .map(|e| e.text().collect::<String>())
                .unwrap_or_default();
            let value = p
                .text()
                .collect::<String>()
                .replace(&label, "")
                .trim()
                .to_string();
            match label.trim() {
                "識別碼:" | "番号:" | "识别码:" => {
                    if !value.is_empty() {
                        site_number = Some(value);
                    }
                }
                "發行日期:" | "发售日期:" | "发行日期:" => {
                    meta.release_date =
                        chrono::NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d").ok()
                }
                "長度:" | "时长:" | "长度:" => {
                    meta.runtime_min = value
                        .chars()
                        .filter(|c| c.is_ascii_digit())
                        .collect::<String>()
                        .parse()
                        .ok()
                }
                "製作商:" | "制作商:" | "廠商:" => meta.studio = Some(value),
                "系列:" => meta.series = Some(value),
                _ => {}
            }
        }
    }

    // 🔴 再核一次详情页上的識別碼。slug 对不代表页面内容对（站点偶有错链），
    // 而这是唯一一个站点自己声明的权威字段。
    if let Some(site) = &site_number {
        if norm_number(site) != norm_number(number) {
            return Err(anyhow!(
                "详情页番号不符：请求 {number}，页面写的是 {site}（{link}）"
            ));
        }
        meta.number = site.clone();
    }

    // 导演（p 里的 a 链接，上面 text() 已拿到名字但会混入 span；单独取一次）
    if meta.director.is_none() {
        if let Some(d) = sel_text(&detail, "div.col-md-3.info p a[href*='/director/']") {
            meta.director = Some(d);
        }
    }
    // 演员：div.star-name a（無碼页通常没有）
    if let Ok(star_sel) = Selector::parse("div.star-name a") {
        for el in detail.select(&star_sel) {
            let name = el.text().collect::<String>().trim().to_string();
            if !name.is_empty() && !meta.actors.contains(&name) {
                meta.actors.push(name);
            }
        }
    }
    // 标签：span.genre a[href*='/genre/']（演员悬浮行也有 span.genre，但 href 过滤可区分；
    // 导航里的 /genre/hd、/genre/sub 不在 span.genre 内，所以不会被误收）
    if let Ok(genre_sel) = Selector::parse("span.genre a[href*='/genre/']") {
        for el in detail.select(&genre_sel) {
            let t = el.text().collect::<String>().trim().to_string();
            if !t.is_empty() && !meta.tags.contains(&t) {
                meta.tags.push(t);
            }
        }
    }
    // 无码判断：**以分区为准**（無碼分区出来的必然无码），标签只是补充
    if section.is_uncensored()
        || meta
            .tags
            .iter()
            .any(|t| t.contains("無碼") || t.contains("无码"))
    {
        meta.uncensored = Some(true);
    }
    Ok(meta)
}

#[async_trait]
impl Provider for JavBus {
    fn id(&self) -> &str {
        "javbus"
    }

    fn label(&self) -> &str {
        "JavBus（有码 + 無碼）"
    }

    /// javbus **不索引 FC2**（实测有碼站与 `/uncensored` 均为 0 结果），
    /// 所以声明为「不处理」—— 让引擎直接跳过，省一次白跑的请求和一条误导性 WARN。
    /// （`search()` 里仍保留早退分支，供直接调用与线上测试使用。）
    fn supports(&self, number: &str) -> bool {
        !norm_number(number).starts_with("FC2")
    }

    async fn search(&self, ctx: &ScrapeCtx, number: &str) -> Result<Vec<VideoMeta>> {
        // FC2 早点返回，别白跑两次请求再报一句含糊的错
        if norm_number(number).starts_with("FC2") {
            return Err(anyhow!("{}", fc2_message(number)));
        }

        // 先有码、再無碼。命中即返回，所以大多数番号只多花一次搜索。
        let mut tried: Vec<String> = Vec::new();
        for section in Section::ORDER {
            match Self::try_section(ctx, number, section).await {
                Ok(meta) => return Ok(vec![meta]),
                Err(e) => tried.push(format!("{}：{e}", section.label())),
            }
        }
        Err(anyhow!(
            "javbus 有码与無碼分区都没刮到 {number}\n  {}",
            tried.join("\n  ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_number_ignores_case_and_separators() {
        assert_eq!(norm_number("SSIS-424"), "SSIS424");
        assert_eq!(norm_number("ssis424"), "SSIS424");
        assert_eq!(norm_number("SSIS_424"), "SSIS424");
        assert_eq!(norm_number("FC2-PPV-3141592"), "FC2PPV3141592");
    }

    /// 🔴 前导零只是排版：`MIDV-012` ≡ `MIDV-12` ≡ `MIDV0012`。
    /// 不抹平的话，「请求紧凑形态、站点写标准形态」会被误判成番号不符而拒掉。
    #[test]
    fn normalize_number_ignores_leading_zeros() {
        assert_eq!(norm_number("MIDV-012"), norm_number("MIDV-12"));
        assert_eq!(norm_number("MIDV00567"), norm_number("MIDV-567"));
        assert_eq!(norm_number("SSIS00424"), norm_number("SSIS-424"));
        assert_eq!(norm_number("ABC-000"), norm_number("ABC-0"), "全零段保留一个 0");
        // 数字段之间不能互相吃掉
        assert_ne!(norm_number("ABC-12"), norm_number("ABC-120"));
    }

    #[test]
    fn slug_helpers() {
        assert_eq!(slug_of("https://www.javbus.com/MIDV-567"), "MIDV-567");
        assert_eq!(slug_of("https://www.javbus.com/MIDV-567/"), "MIDV-567");
        assert_eq!(
            slug_number("MIDV-012_2025-02-04"),
            "MIDV-012",
            "日期式变体后缀要剥掉"
        );
        assert_eq!(slug_number("MIDV-567"), "MIDV-567");
        // 🔴 無碼日期式番号的 `_002` 是番号本体，**不能**剥
        assert_eq!(
            slug_number("080918_002"),
            "080918_002",
            "把 _002 剥掉就永远匹配不上（080918_001/002 是两部不同的片子）"
        );
        assert_eq!(slug_number("092426_001"), "092426_001");
        assert!(!is_date_suffix("002"));
        assert!(is_date_suffix("2025-02-04"));
        assert!(!is_date_suffix("2025-2-4"));
    }

    /// 無碼日期式番号必须能被选中（这条钉住 `slug_number` 那个坑）。
    #[test]
    fn picks_date_style_uncensored() {
        let html = r#"<div id="waterfall">
            <a class="movie-box" href="https://www.javbus.com/080918_001">a</a>
            <a class="movie-box" href="https://www.javbus.com/080918_002">b</a>
        </div>"#;
        let cands = parse_candidates(html);
        assert_eq!(
            pick_candidate(&cands, "080918_002").as_deref(),
            Some("https://www.javbus.com/080918_002"),
            "必须挑中 002 而不是第一条 001"
        );
        assert_eq!(pick_candidate(&cands, "080918_003"), None);
    }

    /// 🔴 核心断言：模糊搜索的第一条是别的番号时，**不能**拿它硬凑。
    /// 这组数据就是线上实测的 `SSIS-4` 搜索结果（第一条 SSIS-984）。
    #[test]
    fn picks_exact_match_not_first_result() {
        let html = r#"<html><body><div id="waterfall">
            <a class="movie-box" href="https://www.javbus.com/SSIS-984">a</a>
            <a class="movie-box" href="https://www.javbus.com/SSIS-994">b</a>
            <a class="movie-box" href="https://www.javbus.com/SSIS-424">c</a>
            <a class="movie-box" href="https://www.javbus.com/SSIS-414">d</a>
        </div></body></html>"#;
        let cands = parse_candidates(html);
        assert_eq!(cands.len(), 4);
        assert_eq!(
            pick_candidate(&cands, "SSIS-424").as_deref(),
            Some("https://www.javbus.com/SSIS-424"),
            "必须挑中 SSIS-424，而不是第一条 SSIS-984"
        );
    }

    /// 一个都对不上时必须返回 None（让上层拒绝），不能兜底取第一条。
    #[test]
    fn no_exact_match_returns_none() {
        let cands = vec![
            "https://www.javbus.com/SSIS-984".to_string(),
            "https://www.javbus.com/SSIS-994".to_string(),
        ];
        assert_eq!(pick_candidate(&cands, "SSIS-424"), None);
    }

    /// 相对/绝对 href 都要能处理；CD 变体 slug 要认得出来。
    #[test]
    fn candidates_and_variants() {
        let html = r#"<div id="waterfall">
            <a class="movie-box" href="/MIDV-012_2025-02-04">x</a>
        </div>"#;
        let cands = parse_candidates(html);
        assert_eq!(cands, vec!["https://www.javbus.com/MIDV-012_2025-02-04"]);
        assert_eq!(
            pick_candidate(&cands, "MIDV-012").as_deref(),
            Some("https://www.javbus.com/MIDV-012_2025-02-04")
        );
        // 反向**不该**命中：番号带日期后缀时不能被裸番号糊弄过去（宁可失败）
        assert_eq!(pick_candidate(&cands, "MIDV-013"), None);
    }

    /// 两个分区的搜索 URL 前缀不同，其余相同。
    #[test]
    fn section_search_urls() {
        assert_eq!(
            Section::Censored.search_url("MIDV-567"),
            "https://www.javbus.com/search/MIDV-567&type=0&parent=ce"
        );
        assert_eq!(
            Section::Uncensored.search_url("HEYZO-1673"),
            "https://www.javbus.com/uncensored/search/HEYZO-1673&type=0&parent=ce"
        );
        assert!(!Section::Censored.is_uncensored());
        assert!(Section::Uncensored.is_uncensored());
        // 有码先试（内容多得多）
        assert_eq!(Section::ORDER, [Section::Censored, Section::Uncensored]);
    }

    /// FC2 早退必须**在发任何请求之前**就报出可诊断的错误。
    ///
    /// 这条能离线跑：对 FC2，`search()` 第一件事就是返回。
    /// 🔴 它专门钉住一个真实踩过的 bug：早退时手上还没有页面，
    /// 若把空字符串丢进「页面像不像搜索页」的判断，FC2 会被误报成「被 Cloudflare 拦了」——
    /// 当时只有**线上实测**发现了它，离线测试全绿。
    #[tokio::test]
    async fn fc2_short_circuits_before_any_request() {
        let ctx = ScrapeCtx {
            http: reqwest::Client::new(),
            flaresolverr: None,
        };
        let err = JavBus::new()
            .search(&ctx, "FC2-PPV-3141592")
            .await
            .expect_err("javbus 不索引 FC2，应当报错")
            .to_string();
        assert!(err.contains("FC2"), "要说明是 FC2 不支持，实际：{err}");
        assert!(
            !err.contains("Cloudflare") && !err.contains("代理"),
            "不该被误报成被拦截 / 没配代理，实际：{err}"
        );
    }

    /// 被拦截时要给出能诊断的提示，而不是一句「无结果」。
    #[test]
    fn diagnose_distinguishes_blocked_from_empty() {
        let blocked = "<html><body>Just a moment... Cloudflare</body></html>";
        let msg = diagnose_no_result(blocked, "MIDV-567", Section::Censored);
        assert!(msg.contains("代理") || msg.contains("拦"), "实际：{msg}");

        let real_search = r#"<div id="waterfall"></div><input id="search-input">"#;
        let msg2 = diagnose_no_result(real_search, "MIDV-567", Section::Censored);
        assert!(msg2.contains("有码分区里搜不到 MIDV-567"), "实际：{msg2}");

        let msg3 = diagnose_no_result(real_search, "FC2-PPV-3141592", Section::Uncensored);
        assert!(msg3.contains("FC2"), "FC2 要单独说明，实际：{msg3}");

        let msg4 = diagnose_no_result(real_search, "HEYZO-1673", Section::Uncensored);
        assert!(msg4.contains("無碼分区"), "要说清是哪个分区，实际：{msg4}");
    }

    // ─────────── 详情页解析（离线，用合成的 HTML）───────────

    const CENSORED_DETAIL: &str = r#"<html><body>
<h3>MIDV-567 テスト标题</h3>
<div class="row movie">
  <div class="col-md-9 screencap">
    <a class="bigImage" href="/pics/cover/a4xk_b.jpg"><img src="/pics/cover/a4xk_b.jpg"></a>
  </div>
  <div class="col-md-3 info">
    <p><span class="header">識別碼:</span> <span style="color:#CC0000;">MIDV-567</span></p>
    <p><span class="header">發行日期:</span> 2023-12-15</p>
    <p><span class="header">長度:</span> 120分鐘</p>
    <p><span class="header">製作商:</span> <a href="https://www.javbus.com/studio/4v">ムーディーズ</a></p>
    <p><span class="header">導演:</span> <a href="https://www.javbus.com/director/l">HiroA</a></p>
  </div>
</div>
<div class="star-name"><a href="https://www.javbus.com/star/10j8" title="三崎なな">三崎なな</a></div>
<span class="genre"><a href="https://www.javbus.com/genre/1o">口交</a></span>
<span class="genre"><a href="https://www.javbus.com/genre/3">中出</a></span>
<a class="sample-box" href="/pics/sample/1.jpg">s1</a>
<a class="sample-box" href="https://pics.dmm.co.jp/x/2.jpg">s2</a>
</body></html>"#;

    #[test]
    fn parse_censored_detail() {
        let m = parse_detail(
            CENSORED_DETAIL,
            "MIDV-567",
            "https://www.javbus.com/MIDV-567",
            Section::Censored,
        )
        .unwrap();
        assert_eq!(m.number, "MIDV-567");
        assert_eq!(m.title.as_deref(), Some("MIDV-567 テスト标题"));
        assert_eq!(
            m.cover_url.as_deref(),
            Some("https://www.javbus.com/pics/cover/a4xk_b.jpg"),
            "封面必须绝对化"
        );
        assert_eq!(
            m.release_date,
            chrono::NaiveDate::from_ymd_opt(2023, 12, 15)
        );
        assert_eq!(m.runtime_min, Some(120));
        assert_eq!(m.studio.as_deref(), Some("ムーディーズ"));
        assert_eq!(m.director.as_deref(), Some("HiroA"));
        assert_eq!(m.actors, vec!["三崎なな"]);
        assert_eq!(m.tags, vec!["口交", "中出"]);
        assert_eq!(m.preview_urls.len(), 2);
        assert_eq!(m.preview_urls[0], "https://www.javbus.com/pics/sample/1.jpg");
        assert_eq!(m.preview_urls[1], "https://pics.dmm.co.jp/x/2.jpg");
        assert_eq!(m.uncensored, None, "有码分区不该被标成无码");
    }

    /// 無碼页：结构一样，但没有预览图和演员，封面在 `/imgs/cover/`。
    /// 关键是 `uncensored` 必须**由分区决定**（而不是靠标签里有没有「無碼」二字）。
    const UNCENSORED_DETAIL: &str = r#"<html><body>
<h3>HEYZO-1673 美咲愛のパイでズッてあげる！</h3>
<div class="row movie">
  <div class="col-md-9 screencap">
    <a class="bigImage" href="/imgs/cover/11k5_b.jpg"><img src="/imgs/cover/11k5_b.jpg"></a>
  </div>
  <div class="col-md-3 info">
    <p><span class="header">識別碼:</span> <span style="color:#CC0000;">HEYZO-1673</span></p>
    <p><span class="header">發行日期:</span> 2018-02-21</p>
    <p><span class="header">長度:</span> 60分鐘</p>
    <p><span class="header">製作商:</span> <a href="https://www.javbus.com/uncensored/studio/3a">HEYZO</a></p>
    <p class="star-show"><span class="header">演員</span>: 暫無出演者資訊</p>
  </div>
</div>
<div id="star-div"></div>
</body></html>"#;

    #[test]
    fn parse_uncensored_detail() {
        let m = parse_detail(
            UNCENSORED_DETAIL,
            "HEYZO-1673",
            "https://www.javbus.com/HEYZO-1673",
            Section::Uncensored,
        )
        .unwrap();
        assert_eq!(m.number, "HEYZO-1673");
        assert_eq!(m.runtime_min, Some(60));
        assert_eq!(m.studio.as_deref(), Some("HEYZO"));
        assert_eq!(
            m.cover_url.as_deref(),
            Some("https://www.javbus.com/imgs/cover/11k5_b.jpg")
        );
        assert!(m.actors.is_empty(), "無碼页没有演员，这是事实不是缺陷");
        assert!(m.preview_urls.is_empty(), "無碼页没有预览图");
        assert_eq!(
            m.uncensored,
            Some(true),
            "無碼分区出来的必须标为无码（靠分区，不靠标签）"
        );
    }

    /// 🔴 详情页番号与请求不符时必须报错 —— 否则就是把别人的元数据贴上去。
    #[test]
    fn detail_number_mismatch_is_rejected() {
        let err = parse_detail(
            CENSORED_DETAIL,
            "SSIS-424",
            "https://www.javbus.com/MIDV-567",
            Section::Censored,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("番号不符"), "实际：{err}");
    }

    /// 请求是紧凑形态（`SSIS00424`）而页面写标准形态时，不该被误判为不符。
    #[test]
    fn detail_number_matches_across_forms() {
        let m = parse_detail(
            CENSORED_DETAIL,
            "MIDV00567",
            "https://www.javbus.com/MIDV-567",
            Section::Censored,
        )
        .unwrap();
        assert_eq!(m.number, "MIDV-567", "应回填站点声明的那份");
    }
}
