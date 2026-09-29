//! 自定义刮削源：用户在 `<数据目录>/providers/*.yaml` 放声明式配置即可接入新站点。
//!
//! YAML 规范（选择器用 CSS 语法，`@属性名` 表示取属性，缺省取文本）：
//!
//! ```yaml
//! name: mysite
//! base_url: https://example.com
//! search:
//!   url: "{base_url}/search?q={number}"
//!   result_selector: "div.item a"          # 详情页链接（自动取 href）
//! detail:
//!   title: "h1.title"
//!   cover: "div.cover img@src"
//!   actors: "span.actor a"                 # 多值：全部匹配项
//!   tags: "div.tags a"
//!   release_date: "span.date"              # 自动解析 YYYY-MM-DD
//!   studio: "span.studio"
//!   number: "span.number"
//! ```

use crate::config::AppConfig;
use crate::model::VideoMeta;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct CustomProviderDef {
    pub name: String,
    pub base_url: String,
    pub search: SearchDef,
    pub detail: DetailDef,
}

#[derive(Debug, Deserialize)]
pub struct SearchDef {
    pub url: String,
    pub result_selector: String,
    /// 详情链接数量上限（防止误抓全站）
    #[serde(default = "default_limit")]
    pub max_results: usize,
}

fn default_limit() -> usize {
    3
}

#[derive(Debug, Deserialize)]
pub struct DetailDef {
    pub title: Option<String>,
    pub cover: Option<String>,
    pub poster: Option<String>,
    pub number: Option<String>,
    pub release_date: Option<String>,
    pub studio: Option<String>,
    pub series: Option<String>,
    #[serde(default)]
    pub actors: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

pub fn load_custom_providers() -> Result<Vec<CustomProvider>> {
    let dir = AppConfig::providers_dir();
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        // `_` 开头的是示例/停用文件，不当作真实源加载
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with('_'))
            .unwrap_or(false)
        {
            continue;
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("读取 {}", path.display()))?;
        let def: CustomProviderDef = serde_yaml::from_str(&raw)
            .with_context(|| format!("解析 {}", path.display()))?;
        out.push(CustomProvider {
            id: format!("custom:{}", def.name),
            def,
        });
    }
    Ok(out)
}

pub struct CustomProvider {
    id: String,
    def: CustomProviderDef,
}

fn extract_one(html: &scraper::Html, spec: &str) -> Option<String> {
    let (selector, attr) = match spec.split_once('@') {
        Some((s, a)) => (s, Some(a)),
        None => (spec, None),
    };
    let sel = scraper::Selector::parse(selector).ok()?;
    let el = html.select(&sel).next()?;
    let value = match attr {
        Some(a) => el.value().attr(a)?.to_string(),
        None => el.text().collect::<String>().trim().to_string(),
    };
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn extract_many(html: &scraper::Html, spec: &str) -> Vec<String> {
    let (selector, attr) = match spec.split_once('@') {
        Some((s, a)) => (s, Some(a)),
        None => (spec, None),
    };
    let Some(sel) = scraper::Selector::parse(selector).ok() else {
        return Vec::new();
    };
    html.select(&sel)
        .filter_map(|el| match attr {
            Some(a) => el.value().attr(a).map(|v| v.to_string()),
            None => Some(el.text().collect::<String>().trim().to_string()),
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// 把抓到的链接补成绝对 URL。
///
/// 🔴 站点给的封面/详情链接**大量是相对路径**（`/img/a.jpg`）或**协议相对**（`//cdn/a.jpg`）。
/// 直接丢给 HTTP 客户端会被拒（`relative URL without a base`），
/// 表现是「刮削成功、元数据都对了，就是海报永远没有」—— 静默、极难查。
pub fn resolve_url(base: &str, raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }
    if raw.starts_with("http://") || raw.starts_with("https://") {
        return raw.to_string();
    }
    match reqwest::Url::parse(base).and_then(|b| b.join(raw)) {
        Ok(u) => u.to_string(),
        // 补不成绝对地址就原样返回：留着至少能在 UI 里看出原始值，比丢掉强
        Err(_) => raw.to_string(),
    }
}

impl CustomProvider {
    fn render_url(&self, template: &str, number: &str) -> String {
        template
            .replace("{base_url}", &self.def.base_url)
            .replace("{number}", number)
    }
}

#[async_trait]
impl super::Provider for CustomProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn label(&self) -> &str {
        &self.def.name
    }

    async fn search(&self, ctx: &super::ScrapeCtx, number: &str) -> Result<Vec<VideoMeta>> {
        // scraper::Html 非 Send：先完成全部网络 IO，再统一同步解析
        let search_url = self.render_url(&self.def.search.url, number);
        let search_body = ctx
            .http
            .get(&search_url)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;

        let links: Vec<String> = {
            let page = scraper::Html::parse_document(&search_body);
            let sel = scraper::Selector::parse(&self.def.search.result_selector)
                .map_err(|e| anyhow!("result_selector 无效: {e}"))?;
            page.select(&sel)
                .filter_map(|el| el.value().attr("href").map(|h| h.to_string()))
                .take(self.def.search.max_results)
                // 同样要走 resolve_url：手写 `format!("{base}/{rest}")` 会把
                // 协议相对的 `//cdn/x` 拼成 `base//cdn/x`（错的）
                .map(|href| resolve_url(&self.def.base_url, &href))
                .filter(|u| !u.is_empty())
                .collect()
        };

        let mut bodies = Vec::new();
        for link in &links {
            let body = match ctx.http.get(link).send().await {
                Ok(r) => r.error_for_status()?.text().await?,
                Err(e) => {
                    tracing::warn!(url = %link, error = %e, "详情页抓取失败");
                    continue;
                }
            };
            bodies.push((link.clone(), body));
        }

        let mut metas = Vec::new();
        for (link, body) in bodies {
            let html = scraper::Html::parse_document(&body);
            let d = &self.def.detail;
            let mut meta = VideoMeta {
                number: extract_one(&html, d.number.as_deref().unwrap_or(""))
                    .unwrap_or_else(|| number.to_string()),
                website: Some(link.clone()),
                source: Some(self.id().to_string()),
                ..Default::default()
            };
            meta.title = d.title.as_deref().and_then(|s| extract_one(&html, s));
            // 封面/海报必须**补成绝对地址**，否则相对路径会让下载静默失败
            meta.cover_url = d
                .cover
                .as_deref()
                .and_then(|s| extract_one(&html, s))
                .map(|s| resolve_url(&link, &s))
                .filter(|s| !s.is_empty());
            meta.poster_url = d
                .poster
                .as_deref()
                .and_then(|s| extract_one(&html, s))
                .map(|s| resolve_url(&link, &s))
                .filter(|s| !s.is_empty());
            meta.studio = d.studio.as_deref().and_then(|s| extract_one(&html, s));
            meta.series = d.series.as_deref().and_then(|s| extract_one(&html, s));
            if let Some(date) = d
                .release_date
                .as_deref()
                .and_then(|s| extract_one(&html, s))
            {
                meta.release_date = parse_date(&date);
            }
            for spec in &d.actors {
                meta.actors.extend(extract_many(&html, spec));
            }
            for spec in &d.tags {
                meta.tags.extend(extract_many(&html, spec));
            }
            metas.push(meta);
        }
        Ok(metas)
    }
}

fn parse_date(s: &str) -> Option<chrono::NaiveDate> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit() || *c == '-').collect();
    chrono::NaiveDate::parse_from_str(digits.trim_matches('-'), "%Y-%m-%d").ok()
}

/// 生成一个示例自定义源配置，首次启动时写入 providers 目录。
pub fn write_example_provider() -> Result<PathBuf> {
    let dir = AppConfig::providers_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("_example.yaml");
    if !path.exists() {
        std::fs::write(
            &path,
            r#"# 自定义刮削源示例：把本文件重命名为 <名字>.yaml 并按目标站点修改即可生效。
# 选择器为 CSS 语法；`元素@属性名` 表示提取属性，缺省提取文本。
name: example
base_url: https://example.com
search:
  url: "{base_url}/search?q={number}"
  result_selector: "div.item a"
  max_results: 3
detail:
  title: "h1.title"
  cover: "div.cover img@src"
  actors: ["span.actor a"]
  tags: ["div.tags a"]
  release_date: "span.date"
  studio: "span.studio"
"#,
        )?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::resolve_url;

    #[test]
    fn absolute_url_untouched() {
        assert_eq!(
            resolve_url("https://a.com/x", "https://cdn.b.com/i.jpg"),
            "https://cdn.b.com/i.jpg"
        );
        assert_eq!(
            resolve_url("https://a.com/x", "http://cdn.b.com/i.jpg"),
            "http://cdn.b.com/i.jpg"
        );
    }

    /// 相对路径：以详情页为基准解析
    #[test]
    fn root_relative_resolved_against_base() {
        assert_eq!(
            resolve_url("https://a.com/detail/1", "/img/cover.jpg"),
            "https://a.com/img/cover.jpg"
        );
        assert_eq!(
            resolve_url("https://a.com/detail/1", "cover.jpg"),
            "https://a.com/detail/cover.jpg"
        );
    }

    /// 协议相对 `//cdn/...` —— 手写拼串会拼成 `base//cdn/...`（错的），这条钉住
    #[test]
    fn protocol_relative_keeps_scheme() {
        assert_eq!(
            resolve_url("https://a.com/detail/1", "//cdn.b.com/i.jpg"),
            "https://cdn.b.com/i.jpg"
        );
        assert_eq!(
            resolve_url("http://a.com/detail/1", "//cdn.b.com/i.jpg"),
            "http://cdn.b.com/i.jpg"
        );
    }

    #[test]
    fn empty_stays_empty_and_bad_base_degrades() {
        assert_eq!(resolve_url("https://a.com/x", "   "), "");
        // base 不是合法 URL 时不能 panic，也不能把原值丢掉
        assert_eq!(resolve_url("not a url", "/img/a.jpg"), "/img/a.jpg");
    }
}
