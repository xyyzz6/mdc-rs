//! FC2 官方站（`adult.contents.fc2.com`）—— 补上 javbus 不索引 FC2 的缺口。
//!
//! ## 实测事实（2026-09，改之前必读）
//!
//! | 事实 | 说明 |
//! | --- | --- |
//! | **也需要代理** | 直连实测 `http_code=000` |
//! | **番号就是商品 ID** | `FC2-PPV-4680562` → `https://adult.contents.fc2.com/article/4680562/` |
//! | 🔴 **不存在的 id 返回 HTTP 200 + 错误页** | 标题是「お探しの商品が見つかりませんでした」，**没有 `ld+json`**。必须显式识别，否则就是静默假阳性 |
//! | 🔴 **`<h3>` 标题被投毒** | 标题中间插了 `<span style="zoom:0.01;...">*****zp*jqpp </span>` 反爬噪声。**必须用 `og:title` 或 `ld+json`** |
//! | `ld+json` 是 `Product` | 有 name / image.url / brand / offers.price / aggregateRating |
//! | 其余字段位置 | 上架时间 / 时长 / 商品标签 / 预览图 / 卖家名（见下方选择器） |
//!
//! ⚠️ FC2 是素人（业余）内容，**没有演员** —— 这是事实，不要用卖家名去凑演员。

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use regex::Regex;
use scraper::{Html, Selector};

use crate::model::VideoMeta;

use super::{Provider, ScrapeCtx};

pub struct Fc2;

impl Fc2 {
    pub fn new() -> Self {
        Self
    }

    fn base() -> &'static str {
        "https://adult.contents.fc2.com"
    }

    /// 从任意形态的 FC2 番号里取出商品 ID。
    /// `FC2-PPV-4680562` / `fc2ppv 4680562` → `4680562`。
    ///
    /// ⚠️ 不能简单 filter 数字：`FC2` 里的 `2` 会被一起捡走（得到 `24680562`）。
    pub fn article_id(number: &str) -> Option<String> {
        let re = Regex::new(r"(?i)fc2[-_ ]?(?:ppv)?[-_ ]?(\d{5,8})").ok()?;
        re.captures(number).map(|c| c[1].to_string())
    }

    /// 页面是不是「找不到商品」。
    ///
    /// FC2 对不存在的 id 返回 **200**，所以不能靠状态码判断。
    /// 判据用 `ld+json` 缺失 —— 真实商品页一定有它。
    fn is_not_found(html: &str) -> bool {
        !html.contains("application/ld+json")
    }

    /// `01:10:04` → 70（分钟）
    fn parse_runtime(s: &str) -> Option<u32> {
        let parts: Vec<u32> = s
            .trim()
            .split(':')
            .map(|p| p.trim().parse::<u32>().unwrap_or(0))
            .collect();
        match parts.as_slice() {
            [h, m, _sec] => Some(h * 60 + m),
            [m, _sec] => Some(*m),
            _ => None,
        }
    }

    /// 从 `上架时间 : 2025/05/05` 这类文本里取日期。
    fn parse_date(s: &str) -> Option<chrono::NaiveDate> {
        let re = Regex::new(r"(\d{4})/(\d{1,2})/(\d{1,2})").ok()?;
        let c = re.captures(s)?;
        chrono::NaiveDate::from_ymd_opt(
            c[1].parse().ok()?,
            c[2].parse().ok()?,
            c[3].parse().ok()?,
        )
    }

    /// 协议相对（`//host/x.jpg`）补成 `https:`。
    fn absolutize(url: &str) -> String {
        let u = url.trim();
        if u.starts_with("//") {
            format!("https:{u}")
        } else if u.starts_with('/') {
            format!("{}{}", Self::base(), u)
        } else {
            u.to_string()
        }
    }

    async fn get_text(ctx: &ScrapeCtx, url: &str) -> Result<String> {
        Ok(ctx
            .http
            .get(url)
            .header("accept-language", "zh-CN,zh;q=0.9,ja;q=0.8")
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?)
    }
}

impl Default for Fc2 {
    fn default() -> Self {
        Self::new()
    }
}

fn text_of(html: &Html, selector: &str) -> Option<String> {
    let sel = Selector::parse(selector).ok()?;
    let t = html.select(&sel).next()?.text().collect::<String>().trim().to_string();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

/// 从 `og:title` 里剥掉番号前缀（FC2 的 og:title 形如 `FC2-PPV-4680562 标题`）。
fn strip_number_prefix(title: &str, number: &str) -> String {
    let t = title.trim();
    if let Some(rest) = t.strip_prefix(number) {
        return rest.trim_start_matches([' ', '　', '-', ':']).trim().to_string();
    }
    t.to_string()
}

#[async_trait]
impl Provider for Fc2 {
    fn id(&self) -> &str {
        "fc2"
    }

    fn label(&self) -> &str {
        "FC2（官方站）"
    }

    /// 只处理 FC2 番号 —— 让引擎别为它白跑一次网络请求。
    fn supports(&self, number: &str) -> bool {
        Self::article_id(number).is_some()
    }

    async fn search(&self, ctx: &ScrapeCtx, number: &str) -> Result<Vec<VideoMeta>> {
        let id = Self::article_id(number)
            .ok_or_else(|| anyhow!("不是 FC2 番号：{number}"))?;
        let url = format!("{}/article/{}/", Self::base(), id);
        let html_text = Self::get_text(ctx, &url).await?;

        if Self::is_not_found(&html_text) {
            return Err(anyhow!(
                "FC2 上没有这个商品（{url} 返回 200 但是「找不到」页，没有 ld+json）"
            ));
        }

        let html = Html::parse_document(&html_text);
        let canonical = format!("FC2-PPV-{id}");
        let mut meta = VideoMeta {
            number: canonical.clone(),
            website: Some(url),
            source: Some(self.id().to_string()),
            ..Default::default()
        };

        // 标题：**不能**用 <h3>（被插了反爬噪声 span）。优先 ld+json 的 name，退回 og:title。
        let ld = extract_product_ld_json(&html_text);
        let raw_title = ld
            .as_ref()
            .and_then(|v| v.get("name").and_then(|s| s.as_str()).map(str::to_string))
            .or_else(|| {
                Selector::parse(r#"meta[property="og:title"]"#)
                    .ok()
                    .and_then(|sel| html.select(&sel).next())
                    .and_then(|e| e.value().attr("content").map(str::to_string))
            })
            .map(|t| strip_number_prefix(&t, &canonical))
            .filter(|t| !t.is_empty());
        meta.title = raw_title.clone();
        meta.original_title = raw_title;

        // 封面：ld+json 的 image.url，退回 og:image
        meta.cover_url = ld
            .as_ref()
            .and_then(|v| v.pointer("/image/url").and_then(|s| s.as_str()))
            .map(str::to_string)
            .or_else(|| {
                Selector::parse(r#"meta[property="og:image"]"#)
                    .ok()
                    .and_then(|sel| html.select(&sel).next())
                    .and_then(|e| e.value().attr("content").map(str::to_string))
            })
            .map(|u| Self::absolutize(&u));

        // 时长：<p class="items_article_info">01:10:04</p>
        meta.runtime_min = text_of(&html, "p.items_article_info").and_then(|s| Self::parse_runtime(&s));

        // 上架时间：在 div.items_article_softDevice 的 <p> 里，形如「上架时间 : 2025/05/05」
        if let Ok(sel) = Selector::parse("div.items_article_softDevice p") {
            for p in html.select(&sel) {
                let t = p.text().collect::<String>();
                if t.contains("上架时间") || t.contains("配信開始日") {
                    meta.release_date = Self::parse_date(&t);
                    break;
                }
            }
        }

        // 商品标签：section.items_article_TagArea a.tag
        if let Ok(sel) = Selector::parse("section.items_article_TagArea a.tag") {
            for el in html.select(&sel) {
                let t = el.text().collect::<String>().trim().to_string();
                if !t.is_empty() && !meta.tags.contains(&t) {
                    meta.tags.push(t);
                }
            }
        }

        // 预览图：ul.items_article_SampleImagesArea li a[href]（协议相对）
        if let Ok(sel) = Selector::parse("ul.items_article_SampleImagesArea li a") {
            for el in html.select(&sel) {
                if let Some(href) = el.value().attr("href") {
                    let u = Self::absolutize(href);
                    if !u.is_empty() && !meta.preview_urls.contains(&u) {
                        meta.preview_urls.push(u);
                    }
                }
            }
        }

        // 卖家 → studio（FC2 是素人，没有演员；卖家名放到制作商位，不冒充演员）
        meta.studio = text_of(&html, "li.items_article_writer a");

        // FC2 是素人内容，无码
        meta.uncensored = Some(true);

        Ok(vec![meta])
    }
}

/// 取出 `application/ld+json` 里 `@type == "Product"` 的那个对象。
fn extract_product_ld_json(html: &str) -> Option<serde_json::Value> {
    let doc = Html::parse_document(html);
    let sel = Selector::parse(r#"script[type="application/ld+json"]"#).ok()?;
    for el in doc.select(&sel) {
        let raw = el.text().collect::<String>();
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            if v.get("@type").and_then(|t| t.as_str()) == Some("Product") {
                return Some(v);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn article_id_from_various_forms() {
        assert_eq!(Fc2::article_id("FC2-PPV-4680562").as_deref(), Some("4680562"));
        assert_eq!(Fc2::article_id("fc2ppv 3141592").as_deref(), Some("3141592"));
        assert_eq!(Fc2::article_id("FC2-1234567").as_deref(), Some("1234567"));
        // 🔴 `FC2` 里的 `2` 不能被当成 ID 的一部分
        assert_eq!(Fc2::article_id("FC2-PPV-4680562").unwrap().len(), 7);
        assert_eq!(Fc2::article_id("MIDV-567"), None);
    }

    #[test]
    fn supports_only_fc2() {
        assert!(Fc2::new().supports("FC2-PPV-4680562"));
        assert!(!Fc2::new().supports("MIDV-567"));
    }

    #[test]
    fn runtime_parsing() {
        assert_eq!(Fc2::parse_runtime("01:10:04"), Some(70));
        assert_eq!(Fc2::parse_runtime("00:05:30"), Some(5));
        assert_eq!(Fc2::parse_runtime(""), None);
    }

    #[test]
    fn date_parsing() {
        assert_eq!(
            Fc2::parse_date("上架时间 : 2025/05/05"),
            chrono::NaiveDate::from_ymd_opt(2025, 5, 5)
        );
        assert_eq!(Fc2::parse_date("没有日期"), None);
    }

    /// 🔴 不存在的 id 是 HTTP 200 + 错误页，必须靠「没有 ld+json」识别出来。
    #[test]
    fn detects_not_found_page() {
        let not_found = "<html><head><title>お探しの商品が見つかりませんでした | FC2</title></head><body>…</body></html>";
        assert!(Fc2::is_not_found(not_found), "200 的错误页必须被识别");
        let real = r#"<html><head><script type="application/ld+json">{"@type":"Product"}</script></head></html>"#;
        assert!(!Fc2::is_not_found(real));
    }

    /// 标题必须来自 og:title / ld+json，**不能**来自被投毒的 `<h3>`。
    #[test]
    fn poisoned_h3_title_is_not_used() {
        // 真实页面的 h3 中间插了这个 span
        let poisoned = r#"<div class="items_article_headerInfo"><h3>【女神と巨根】<span style="zoom:0.01;color:#fff;width:1px;height:1px;display:inline-block;overflow:hidden;">*****zp*jqpp </span>独りプールにいた</h3></div>"#;
        let doc = Html::parse_document(poisoned);
        let h3 = text_of(&doc, "div.items_article_headerInfo h3").unwrap();
        assert!(h3.contains("*****"), "前提：h3 确实被投毒（这条测试是为了钉住「别用它」）");

        // 我们的实现取 og:title，所以带不出噪声
        let real_og = "FC2-PPV-4680562 【女神と巨根】独りプールにいた";
        assert_eq!(
            strip_number_prefix(real_og, "FC2-PPV-4680562"),
            "【女神と巨根】独りプールにいた"
        );
    }

    #[test]
    fn strip_prefix_handles_missing_prefix() {
        assert_eq!(strip_number_prefix("纯标题", "FC2-PPV-4680562"), "纯标题");
        assert_eq!(
            strip_number_prefix("FC2-PPV-4680562- 标题", "FC2-PPV-4680562"),
            "标题"
        );
    }
}
