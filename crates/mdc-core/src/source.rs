//! 目录源抽象（`DirSource`）：把「扫目录」这件事与「网盘挂在哪」解耦。
//!
//! 为什么必须有这一层（🔴 APK 端硬需求）：
//! 桌面 / Docker / NAS 上，CloudDrive2 能把 115 挂成**本机目录**（`/mnt/clouddrive/115/...`），
//! 所以「扫目录」= `std::fs::read_dir`。但 **Android 没有 FUSE** ——
//! 网盘不可能出现在文件路径里，CD2 安卓端只提供一个 **WebDAV**（`http://127.0.0.1:19798/dav`）。
//! 若管线里到处写 `Path::is_dir()` / `read_dir()`，APK 端就一条路都走不通。
//!
//! 所以：刮削与 `.strm` 生成只依赖 `DirSource`，由实现去回答
//! 「这个目录里有哪些文件」「这个文件的网盘内路径是什么」。
//!
//! 两个实现：
//! - [`LocalFs`] —— 本地挂载（桌面 / Docker / NAS），网盘路径 = 绝对路径剥掉挂载根；
//! - [`WebDav`] —— CD2 的 `/dav`（安卓唯一可行路径，桌面也能用），走 PROPFIND。
//!
//! 三条纪律：
//! 1. **网盘路径由目录源产出**，不由上层从绝对路径反推 —— 反推在安卓上根本无从下手；
//! 2. WebDAV 走 `net::client()`，**内网一律直连**（代理劫持局域网 = douyin-nas 那个
//!    「时好时坏 404」的老坑，见 `net.rs` 头注释）；
//! 3. 只做 `Depth: 1` 逐层 PROPFIND，**不用 `infinity`** —— 多数服务端（含 CD2）
//!    会直接拒绝或截断，表现为「扫到一半莫名少了目录」。

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use crate::cd2::Cd2Config;

/// 目录源种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// 网盘已挂成**本机目录**（CD2 桌面端 / docker 映射）
    Local,
    /// CD2 的 WebDAV（安卓端唯一可行，桌面端同样可用）
    WebDav,
}

impl SourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::WebDav => "webdav",
        }
    }

    /// 未知值**必须报错**，绝不静默回退成 local ——
    /// 静默回退的表现是「配了 webdav 却一直去读本地空目录，跑完 0 个还不报错」。
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "local" | "" => Ok(Self::Local),
            "webdav" | "dav" => Ok(Self::WebDav),
            other => Err(anyhow!("未知的目录源类型：{other}（只能是 local 或 webdav）")),
        }
    }
}

impl std::fmt::Display for SourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 目录源配置。
///
/// `kind = local` 时**不需要任何额外字段** —— 挂载根沿用 `netdisk.mount_root`
/// （两个地方都能配挂载根是脚枪，迟早出现「改了 A 忘了 B」的 404）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceConfig {
    /// `local` | `webdav`
    pub kind: String,
    /// WebDAV 基址，如 `http://127.0.0.1:19798/dav`
    pub base: String,
    pub user: Option<String>,
    pub password: Option<String>,
    /// 网盘内路径前缀：**只有 WebDAV 源用得上**。
    /// CD2 的 `/dav` 根下若已有一层网盘名（`/dav/115/...`）就留空；
    /// 只有 base 直接指到网盘根、而直链却要带 `/115open` 这类前缀时才填。
    pub path_prefix: String,
    pub timeout_secs: u64,
}

impl Default for SourceConfig {
    fn default() -> Self {
        Self {
            kind: SourceKind::Local.as_str().to_string(),
            base: String::new(),
            user: None,
            password: None,
            path_prefix: String::new(),
            timeout_secs: 15,
        }
    }
}

impl SourceConfig {
    pub fn kind(&self) -> Result<SourceKind> {
        SourceKind::parse(&self.kind)
    }
}

/// 目录项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// 源内路径：
    /// - `Local` —— 本机绝对路径（`/mnt/clouddrive/115/a.mp4`）
    /// - `WebDav` —— 相对 base 的路径（`/115/a.mp4`）
    pub path: String,
    pub name: String,
    pub kind: EntryKind,
    pub size: Option<u64>,
    /// unix 秒；服务端没给或解析不出就是 None（**不影响主流程**）
    pub modified: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
}

/// 目录源。**扫目录 + 算网盘路径**两件事的最小界面。
///
/// 带 `Debug`：日志里要把「当前用的哪个源」打出来，
/// 排查「扫到 0 个」时第一件事就是确认源选对了没。
#[async_trait]
pub trait DirSource: Send + Sync + std::fmt::Debug {
    fn kind(&self) -> SourceKind;
    /// 人类可读的源位置（日志 / UI 用）
    fn root(&self) -> String;
    /// 目录是否存在（不存在要能让上层「跳过并告警」，而不是当成空目录）
    async fn exists_dir(&self, dir: &str) -> Result<bool>;
    /// 列出 `dir` 下的**视频文件**。`recursive=false` 只看一层；`max_depth=0` 表示不限。
    async fn list(&self, dir: &str, recursive: bool, max_depth: u32) -> Result<Vec<DirEntry>>;
    /// 列出 `dir` 下的**一层子目录**（UI 的「从网盘选择监控目录」用）。
    async fn list_dirs(&self, dir: &str) -> Result<Vec<String>>;
    /// 网盘内路径（`/115/电影/x.mp4`），喂给 `cd2::build_url_from_cloud`。
    fn cloud_path(&self, path: &str) -> Result<String>;
}

/// 按配置造出目录源。
///
/// `nd`（CD2 配置）给 `Local` 源提供挂载根与前缀；`proxy` / `no_proxy` 传给
/// `net::client()` —— WebDAV 地址通常是局域网，会被默认规则自动判为直连。
pub fn build(
    src: &SourceConfig,
    nd: &Cd2Config,
    proxy: Option<&str>,
    no_proxy: &[String],
) -> Result<std::sync::Arc<dyn DirSource>> {
    match src.kind()? {
        SourceKind::Local => {
            if nd.mount_root.trim().is_empty() {
                return Err(anyhow!(
                    "目录源是 local，但没配 CD2 挂载根（netdisk.mount_root）"
                ));
            }
            Ok(std::sync::Arc::new(LocalFs::new(&nd.mount_root, &nd.path_prefix)))
        }
        SourceKind::WebDav => {
            let base = src.base.trim();
            if base.is_empty() {
                return Err(anyhow!("目录源是 webdav，但没配 WebDAV 基址（source.base）"));
            }
            let http = crate::net::client(crate::net::HttpOpts {
                proxy,
                no_proxy,
                timeout_secs: src.timeout_secs,
            })?;
            Ok(std::sync::Arc::new(WebDav::new(
                base,
                src.user.as_deref(),
                src.password.as_deref(),
                &src.path_prefix,
                http,
            )))
        }
    }
}

/// 🔴 这些目录名**一律跳过**：扫进去纯属浪费请求，还容易触发网盘风控。
/// 两个实现必须用同一份名单（各写一份迟早漏一个）。
pub fn skip_name(name: &str) -> bool {
    name.starts_with('.') || name == "@eaDir" || name == "#recycle"
}

// ──────────────────────────────────────────────────────────────
// 本地挂载源
// ──────────────────────────────────────────────────────────────

/// 网盘已经挂成**本机目录**时的源。
#[derive(Debug)]
pub struct LocalFs {
    mount_root: String,
    path_prefix: String,
}

impl LocalFs {
    pub fn new(mount_root: &str, path_prefix: &str) -> Self {
        Self {
            mount_root: mount_root.to_string(),
            path_prefix: path_prefix.to_string(),
        }
    }
}

#[async_trait]
impl DirSource for LocalFs {
    fn kind(&self) -> SourceKind {
        SourceKind::Local
    }

    fn root(&self) -> String {
        self.mount_root.clone()
    }

    async fn exists_dir(&self, dir: &str) -> Result<bool> {
        Ok(Path::new(dir).is_dir())
    }

    async fn list(&self, dir: &str, recursive: bool, max_depth: u32) -> Result<Vec<DirEntry>> {
        let mut out = Vec::new();
        walk_local(Path::new(dir), 0, recursive, max_depth, &mut out);
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    async fn list_dirs(&self, dir: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                if e.path().is_dir() {
                    out.push(e.path().to_string_lossy().to_string());
                }
            }
        }
        out.sort();
        Ok(out)
    }

    fn cloud_path(&self, path: &str) -> Result<String> {
        crate::cd2::cloud_path_from_local(path, &self.mount_root, &self.path_prefix)
    }
}

fn walk_local(dir: &Path, depth: u32, recursive: bool, max_depth: u32, out: &mut Vec<DirEntry>) {
    if max_depth > 0 && depth > max_depth {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if skip_name(&name) {
            continue;
        }
        let p: PathBuf = entry.path();
        if p.is_dir() {
            if recursive {
                walk_local(&p, depth + 1, recursive, max_depth, out);
            }
            continue;
        }
        if !crate::parser::is_video_file(&p) {
            continue;
        }
        let (size, modified) = entry.metadata().ok().map_or((None, None), |m| {
            let mt = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64);
            (Some(m.len()), mt)
        });
        out.push(DirEntry {
            path: p.to_string_lossy().to_string(),
            name,
            kind: EntryKind::File,
            size,
            modified,
        });
    }
}

// ──────────────────────────────────────────────────────────────
// WebDAV 源（CD2 /dav，安卓唯一可行路径）
// ──────────────────────────────────────────────────────────────

/// PROPFIND 请求体：只要这几个属性（多要一个就多一份解析风险）。
const PROPFIND_BODY: &str = "<?xml version=\"1.0\" encoding=\"utf-8\" ?>\
<D:propfind xmlns:D=\"DAV:\"><D:prop>\
<D:displayname/><D:getcontentlength/><D:getlastmodified/><D:resourcetype/>\
</D:prop></D:propfind>";

#[derive(Debug)]
pub struct WebDav {
    /// 形如 `http://127.0.0.1:19798/dav`（**不带末尾斜杠**）
    base: String,
    /// base 里的 URL 路径部分（`/dav`），用来把 href 还原成网盘路径
    base_path: String,
    user: Option<String>,
    password: Option<String>,
    path_prefix: String,
    http: reqwest::Client,
}

impl WebDav {
    pub fn new(
        base: &str,
        user: Option<&str>,
        password: Option<&str>,
        path_prefix: &str,
        http: reqwest::Client,
    ) -> Self {
        let base = base.trim_end_matches('/').to_string();
        let base_path = url_path_of(&base);
        Self {
            base,
            base_path,
            user: user.map(|s| s.to_string()),
            password: password.map(|s| s.to_string()),
            path_prefix: path_prefix.to_string(),
            http,
        }
    }

    /// 把「相对 base 的路径」拼成完整 URL。**逐段编码**：
    /// 中文与空格必须转义，但 `/` 是分隔符合法字符，不能跟着编成 `%2F`。
    fn url_for(&self, path: &str) -> String {
        let segs: Vec<String> = path
            .split('/')
            .filter(|s| !s.is_empty())
            .map(crate::cd2::quote_path)
            .collect();
        if segs.is_empty() {
            format!("{}/", self.base)
        } else {
            format!("{}/{}", self.base, segs.join("/"))
        }
    }

    /// 一次 `Depth: 1` 的 PROPFIND。
    async fn propfind(&self, dir: &str) -> Result<Vec<DirEntry>> {
        // 🔴 集合（目录）的 URL **必须以 `/` 结尾** —— 这是 WebDAV 的约定，
        // 不带斜杠的服务端要么 301（reqwest 会丢掉 PROPFIND 方法变成 GET → 405），
        // 要么直接 404。表现是「目录明明在，扫出来 0 个」。
        let url = self.url_for(dir);
        let url = if url.ends_with('/') {
            url
        } else {
            format!("{url}/")
        };
        let mut req = self
            .http
            .request(reqwest::Method::from_bytes(b"PROPFIND").unwrap(), &url)
            .header("Depth", "1")
            .header("Content-Type", "application/xml; charset=utf-8");
        if let (Some(u), Some(p)) = (&self.user, &self.password) {
            req = req.basic_auth(u, Some(p));
        }
        let resp = req
            .body(PROPFIND_BODY)
            .send()
            .await
            .with_context(|| format!("PROPFIND 失败：{url}"))?;
        let status = resp.status();
        // 207 Multi-Status 是正解；部分实现用 200/201 带同样的 XML，一并认下
        if !status.is_success() && status.as_u16() != 207 {
            return Err(anyhow!("PROPFIND {url} 返回 {status}（CD2 起来了吗？）"));
        }
        let body = resp.text().await.context("读 PROPFIND 响应失败")?;
        parse_multistatus(&body, &self.base_path)
    }
}

#[async_trait]
impl DirSource for WebDav {
    fn kind(&self) -> SourceKind {
        SourceKind::WebDav
    }

    fn root(&self) -> String {
        self.base.clone()
    }

    async fn exists_dir(&self, dir: &str) -> Result<bool> {
        // 探不到就当不存在（上层会跳过并告警），**不把网络错误吞成「存在」**
        Ok(self.propfind(dir).await.is_ok())
    }

    /// 逐层 `Depth: 1` 广度优先。
    ///
    /// 不用 `Depth: infinity`：CD2 与多数网盘 WebDAV 要么直接 403，
    /// 要么返回被截断的响应 —— 表现是「扫到一半莫名少了目录」，极难查。
    async fn list(&self, dir: &str, recursive: bool, max_depth: u32) -> Result<Vec<DirEntry>> {
        let mut out: Vec<DirEntry> = Vec::new();
        let mut queue: VecDeque<(String, u32)> = VecDeque::new();
        queue.push_back((norm_dav_path(dir), 0));

        while let Some((d, depth)) = queue.pop_front() {
            for e in self.propfind(&d).await? {
                if e.path == d {
                    continue; // 响应第一条是目录自己
                }
                if skip_name(&e.name) {
                    continue;
                }
                match e.kind {
                    EntryKind::Dir => {
                        if recursive && (max_depth == 0 || depth + 1 <= max_depth) {
                            queue.push_back((e.path, depth + 1));
                        }
                    }
                    EntryKind::File => {
                        if crate::parser::is_video_file(Path::new(&e.name)) {
                            out.push(e);
                        }
                    }
                }
            }
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out.dedup_by(|a, b| a.path == b.path);
        Ok(out)
    }

    fn cloud_path(&self, path: &str) -> Result<String> {
        let p = norm_dav_path(path);
        let prefix = self.path_prefix.trim().trim_matches('/');
        if prefix.is_empty() {
            Ok(p)
        } else {
            Ok(format!("/{prefix}{p}"))
        }
    }

    async fn list_dirs(&self, dir: &str) -> Result<Vec<String>> {
        let d = norm_dav_path(dir);
        let mut out = Vec::new();
        for e in self.propfind(&d).await? {
            if e.path == d {
                continue; // 响应第一条是目录自己
            }
            if skip_name(&e.name) {
                continue;
            }
            if e.kind == EntryKind::Dir {
                out.push(e.path);
            }
        }
        out.sort();
        Ok(out)
    }
}

/// 规范化成 `/a/b` 形式（WebDAV 路径永远以 `/` 开头，根是 `/`）。
pub fn norm_dav_path(p: &str) -> String {
    let s = p.trim().replace('\\', "/");
    let mut segs: Vec<&str> = s.split('/').filter(|x| !x.is_empty()).collect();
    segs.retain(|x| *x != ".");
    if segs.is_empty() {
        return "/".to_string();
    }
    format!("/{}", segs.join("/"))
}

/// 取 URL 里的 path 部分（`http://h:19798/dav` → `/dav`），并做百分号解码。
fn url_path_of(base: &str) -> String {
    let rest = match base.split_once("://") {
        Some((_, r)) => r,
        None => base,
    };
    let path = match rest.find('/') {
        Some(i) => &rest[i..],
        None => "/",
    };
    pct_decode(path.trim_end_matches('/'))
}

/// 百分号解码（UTF-8）。WebDAV 的 href 是编码过的，不解码会得到一坨 `%E7%94%B5`。
pub fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

// ──────────────────────────────────────────────────────────────
// 207 Multi-Status 解析
// ──────────────────────────────────────────────────────────────
//
// 手写而不引完整 XML 库：只需要「按 `<*:response>` 切块 → 取 href /
// 是否 collection / 大小 / 修改时间」四个字段；而且命名空间前缀**各实现各不相同**
// （`d:` `D:` `ns0:` 甚至没有前缀），用「忽略前缀、只认本地名」的扫描比按命名空间查询更稳。

/// 一个标签：`<` 与 `>` 的下标（含），以及解析出的本地名与形态。
struct Tag {
    start: usize,
    end: usize,
    name: String,
    closing: bool,
    self_closing: bool,
}

/// 从 `from` 起找下一个标签。`<!--` 注释与 `<?xml?>` 会被跳过（名字里带 `!`/`?`）。
fn next_tag(s: &str, from: usize) -> Option<Tag> {
    let bytes = s.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let gt = s[i + 1..].find('>')? + i + 1;
        let inner = &s[i + 1..gt];
        if inner.starts_with("!--") || inner.starts_with('?') {
            i = gt + 1;
            continue;
        }
        let t = inner.trim_end_matches('/').trim_start_matches('/');
        let name = t
            .split(|c: char| c.is_whitespace() || c == '/' || c == '>')
            .next()
            .unwrap_or("")
            .rsplit(':')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        return Some(Tag {
            start: i,
            end: gt,
            name,
            closing: inner.starts_with('/'),
            self_closing: s[..gt].ends_with('/'),
        });
    }
    None
}

/// 解析 207 响应里的所有 `<*:response>` 块。
pub fn parse_multistatus(xml: &str, base_path: &str) -> Result<Vec<DirEntry>> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(tag) = next_tag(xml, i) {
        if tag.name == "response" && !tag.closing && !tag.self_closing {
            if let Some(close) = matching_close(xml, tag.end + 1, "response") {
                if let Some(e) = parse_response(&xml[tag.end + 1..close.start], base_path) {
                    out.push(e);
                }
                i = close.end + 1;
                continue;
            }
        }
        i = tag.end + 1;
    }
    Ok(out)
}

/// 从 `from` 起找与本地名 `local` 配对的闭标签（按嵌套深度计数）。
fn matching_close(s: &str, from: usize, local: &str) -> Option<Tag> {
    let mut depth = 1i32;
    let mut i = from;
    while let Some(tag) = next_tag(s, i) {
        if tag.name != local {
            i = tag.end + 1;
            continue;
        }
        if tag.closing {
            depth -= 1;
            if depth == 0 {
                return Some(tag);
            }
        } else if !tag.self_closing {
            depth += 1;
        }
        i = tag.end + 1;
    }
    None
}

/// 解析单个 `<D:response>` 块。
fn parse_response(block: &str, base_path: &str) -> Option<DirEntry> {
    let href = tag_text(block, "href")?;
    let path = dav_path_from_href(href.trim(), base_path)?;
    let name = path.rsplit('/').next().unwrap_or("").to_string();
    if name.is_empty() {
        return None;
    }
    // 🔴 `resourcetype`：目录是 `<D:collection/>`（自闭合），文件是 `<D:resourcetype/>`（空）。
    // 只判断「有没有 resourcetype」会把**文件也认成目录**。
    let is_dir = tag_named(block, "collection");
    let size = tag_text(block, "getcontentlength")
        .and_then(|s| s.trim().parse::<u64>().ok());
    let modified = tag_text(block, "getlastmodified")
        .and_then(|s| chrono::DateTime::parse_from_rfc2822(s.trim()).ok())
        .map(|d| d.timestamp());
    Some(DirEntry {
        path,
        name,
        kind: if is_dir {
            EntryKind::Dir
        } else {
            EntryKind::File
        },
        size,
        modified,
    })
}

/// 块里是否存在某个本地名的标签。
fn tag_named(block: &str, local: &str) -> bool {
    let mut i = 0;
    while let Some(tag) = next_tag(block, i) {
        if tag.name == local {
            return true;
        }
        i = tag.end + 1;
    }
    false
}

/// 取 `<*:local>...</*:local>` 的文本（忽略命名空间前缀；自闭合返回空串）。
fn tag_text(block: &str, local: &str) -> Option<String> {
    let mut i = 0;
    while let Some(tag) = next_tag(block, i) {
        if tag.name != local || tag.closing {
            i = tag.end + 1;
            continue;
        }
        if tag.self_closing {
            return Some(String::new());
        }
        let close = matching_close(block, tag.end + 1, local)?;
        return Some(unescape(block[tag.end + 1..close.start].trim()));
    }
    None
}

fn unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

/// 把 href 还原成「相对 base 的路径」。
///
/// 服务端给的 href 可能是绝对路径（`/dav/115/a.mp4`）也可能是完整 URL；
/// 有的实现只给网盘内路径（不含 base）—— 三种都要能吃。
fn dav_path_from_href(href: &str, base_path: &str) -> Option<String> {
    let s = pct_decode(href.trim());
    let path = match s.split_once("://") {
        Some((_, r)) => match r.find('/') {
            Some(i) => &r[i..],
            None => "/",
        },
        None => s.as_str(),
    };
    let path = path.split('?').next().unwrap_or(path);
    let rest = if base_path.is_empty() || base_path == "/" {
        path
    } else {
        path.strip_prefix(base_path).unwrap_or(path)
    };
    Some(norm_dav_path(rest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mdc_src_test_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    // ── 本地源 ──────────────────────────────────────────────
    #[tokio::test]
    async fn local_lists_videos_recursively() {
        let d = tmpdir("local");
        std::fs::create_dir_all(d.join("sub/deep")).unwrap();
        std::fs::create_dir_all(d.join(".hidden")).unwrap();
        std::fs::write(d.join("a.mp4"), b"x").unwrap();
        std::fs::write(d.join("b.txt"), b"x").unwrap();
        std::fs::write(d.join("sub/c.mkv"), b"x").unwrap();
        std::fs::write(d.join("sub/deep/d.mp4"), b"x").unwrap();
        std::fs::write(d.join(".hidden/e.mp4"), b"x").unwrap();

        let src = LocalFs::new(&d.to_string_lossy(), "");
        let got: Vec<String> = src
            .list(&d.to_string_lossy(), true, 0)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(got, vec!["a.mp4", "c.mkv", "d.mp4"], "实际：{got:?}");

        assert_eq!(src.list(&d.to_string_lossy(), false, 0).await.unwrap().len(), 1);
        assert_eq!(src.list(&d.to_string_lossy(), true, 1).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn local_cloud_path_strips_mount_root() {
        let d = tmpdir("local_cp");
        let src = LocalFs::new(&d.to_string_lossy(), "/115open");
        let f = d.join("看剧/x.mp4");
        assert_eq!(src.cloud_path(&f.to_string_lossy()).unwrap(), "/115open/看剧/x.mp4");
        // 挂载根之外必须报错（而不是硬拼出一个必然 404 的链接）
        assert!(src.cloud_path("/elsewhere/x.mp4").is_err());
    }

    #[tokio::test]
    async fn local_exists_dir() {
        let d = tmpdir("local_exists");
        let src = LocalFs::new(&d.to_string_lossy(), "");
        assert!(src.exists_dir(&d.to_string_lossy()).await.unwrap());
        assert!(!src.exists_dir("/definitely/not/here").await.unwrap());
    }

    // ── XML 解析 ────────────────────────────────────────────
    /// 真实 CD2/115 的 207 响应形状：前缀 `d:`、目录是**自闭合** collection、
    /// href 是百分号编码的。
    const FIXTURE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/dav/115/</D:href>
    <D:propstat><D:prop><D:displayname>115</D:displayname>
      <D:resourcetype><D:collection/></D:resourcetype>
    </D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>
  </D:response>
  <D:response>
    <D:href>/dav/115/%E7%9C%8B%E5%89%A7/</D:href>
    <D:propstat><D:prop><D:displayname>看剧</D:displayname>
      <D:resourcetype><D:collection/></D:resourcetype>
    </D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>
  </D:response>
  <D:response>
    <D:href>/dav/115/%E7%9C%8B%E5%89%A7/MIDV-567%201080p.mp4</D:href>
    <D:propstat><D:prop><D:displayname>MIDV-567 1080p.mp4</D:displayname>
      <D:getcontentlength>2147483648</D:getcontentlength>
      <D:getlastmodified>Fri, 15 Mar 2024 08:09:10 GMT</D:getlastmodified>
      <D:resourcetype/>
    </D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>
  </D:response>
  <D:response>
    <D:href>/dav/115/notes.txt</D:href>
    <D:propstat><D:prop><D:displayname>notes.txt</D:displayname>
      <D:getcontentlength>12</D:getcontentlength>
      <D:resourcetype/>
    </D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>
  </D:response>
</D:multistatus>"#;

    #[test]
    fn parses_multistatus_with_namespaced_prefix() {
        let es = parse_multistatus(FIXTURE, "/dav").unwrap();
        let paths: Vec<String> = es.iter().map(|e| e.path.clone()).collect();
        assert_eq!(
            paths,
            vec![
                "/115",
                "/115/看剧",
                "/115/看剧/MIDV-567 1080p.mp4",
                "/115/notes.txt"
            ],
            "实际：{paths:?}"
        );
        assert_eq!(es[0].kind, EntryKind::Dir, "第一条（目录自己）应是目录");
        assert_eq!(es[1].kind, EntryKind::Dir);
        assert_eq!(es[2].kind, EntryKind::File, "🔴 文件不能被认成目录");
        assert_eq!(es[3].kind, EntryKind::File);
        assert_eq!(es[2].size, Some(2_147_483_648));
        assert_eq!(es[2].name, "MIDV-567 1080p.mp4");
        assert!(es[2].modified.unwrap() > 1_700_000_000);
    }

    /// 没有命名空间前缀（有的实现直接返回裸标签）也要能解析。
    #[test]
    fn parses_prefixless_xml() {
        let plain = FIXTURE
            .replace("<D:", "<")
            .replace("</D:", "</")
            .replace("<D:multistatus", "<multistatus");
        let es = parse_multistatus(&plain, "/dav").unwrap();
        assert_eq!(es.len(), 4, "无前缀 XML 也要能解析：{es:?}");
        assert_eq!(es[2].kind, EntryKind::File);
    }

    #[test]
    fn empty_multistatus_is_empty_list() {
        assert!(
            parse_multistatus("<D:multistatus xmlns:D=\"DAV:\"></D:multistatus>", "/dav")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn pct_decode_roundtrip() {
        assert_eq!(pct_decode("/115/%E7%9C%8B%E5%89%A7/a.mp4"), "/115/看剧/a.mp4");
        // 残缺的 % 不该吃掉后面的字符
        assert_eq!(pct_decode("a%zzb"), "a%zzb");
        assert_eq!(pct_decode("100%"), "100%");
    }

    #[test]
    fn url_for_encodes_but_keeps_slashes() {
        let w = WebDav::new("http://127.0.0.1:19798/dav", None, None, "", reqwest::Client::new());
        assert_eq!(w.url_for("/"), "http://127.0.0.1:19798/dav/");
        assert_eq!(
            w.url_for("/115/看剧/a b.mp4"),
            "http://127.0.0.1:19798/dav/115/%E7%9C%8B%E5%89%A7/a%20b.mp4"
        );
    }

    #[test]
    fn webdav_cloud_path_and_prefix() {
        let w = WebDav::new("http://h:19798/dav/", None, None, "/115open", reqwest::Client::new());
        assert_eq!(w.cloud_path("115/看剧/x.mp4").unwrap(), "/115open/115/看剧/x.mp4");
        let w2 = WebDav::new("http://h:19798/dav", None, None, "", reqwest::Client::new());
        assert_eq!(w2.cloud_path("/115/x.mp4").unwrap(), "/115/x.mp4");
        assert_eq!(w2.cloud_path("").unwrap(), "/");
    }

    // ── 假 CD2 服务端（真实走一遍 HTTP）─────────────────────
    /// 一个只认 PROPFIND 的最小 HTTP 服务：**不联网、不依赖任何 crate**。
    ///
    /// 为什么要真起服务：只测 XML 解析证明不了「请求方法 / URL / Depth 发对了」，
    /// 而这几个恰恰最容易写错（写成 GET 就是 405，写成 infinity 就是缺目录）。
    fn spawn_fake_dav(
        routes: std::collections::HashMap<String, String>,
    ) -> (u16, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen2 = seen.clone();
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
                seen2.lock().unwrap().push(first.clone());
                let url_path = first.split_whitespace().nth(1).unwrap_or("/").to_string();
                let body = routes.get(&url_path).cloned().unwrap_or_else(|| {
                    "<?xml version=\"1.0\"?><D:multistatus xmlns:D=\"DAV:\"></D:multistatus>"
                        .to_string()
                });
                let resp = format!(
                    "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = s.write_all(resp.as_bytes());
                let _ = s.flush();
            }
        });
        (port, seen)
    }

    fn resp_for(path: &str, children: &[(&str, bool)]) -> String {
        let mut s = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?><D:multistatus xmlns:D=\"DAV:\">");
        s.push_str(&format!(
            "<D:response><D:href>{path}</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"
        ));
        for (name, is_dir) in children {
            let href = format!("{path}{name}");
            let rt = if *is_dir { "<D:collection/>" } else { "" };
            s.push_str(&format!(
                "<D:response><D:href>{href}</D:href><D:propstat><D:prop><D:displayname>{name}</D:displayname><D:resourcetype>{rt}</D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"
            ));
        }
        s.push_str("</D:multistatus>");
        s
    }

    fn client_for_test() -> reqwest::Client {
        crate::net::client(crate::net::HttpOpts {
            proxy: None,
            no_proxy: &[],
            timeout_secs: 10,
        })
        .unwrap()
    }

    #[tokio::test]
    async fn webdav_list_against_fake_server() {
        let mut routes = std::collections::HashMap::new();
        routes.insert(
            "/dav/115/%E7%9C%8B%E5%89%A7/".to_string(),
            resp_for(
                "/dav/115/%E7%9C%8B%E5%89%A7/",
                &[("a.mp4", false), ("notes.txt", false), ("sub/", true)],
            ),
        );
        routes.insert(
            "/dav/115/%E7%9C%8B%E5%89%A7/sub/".to_string(),
            resp_for(
                "/dav/115/%E7%9C%8B%E5%89%A7/sub/",
                &[("deep.mkv", false)],
            ),
        );
        let (port, seen) = spawn_fake_dav(routes);
        let w = WebDav::new(&format!("http://127.0.0.1:{port}/dav"), None, None, "", client_for_test());

        let got: Vec<String> = w
            .list("/115/看剧", true, 0)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.path)
            .collect();
        assert_eq!(
            got,
            vec!["/115/看剧/a.mp4", "/115/看剧/sub/deep.mkv"],
            "实际：{got:?}"
        );

        // 🔴 请求必须是 PROPFIND（写成 GET 会 405）；递归时才会去打子目录
        let reqs = seen.lock().unwrap().clone();
        assert!(reqs.iter().all(|l| l.starts_with("PROPFIND ")), "实际：{reqs:?}");
        assert_eq!(reqs.len(), 2, "递归应打两次（自己 + sub）：{reqs:?}");

        // 不递归：只第一层，且**只发一次请求**
        let before = seen.lock().unwrap().len();
        let flat = w.list("/115/看剧", false, 0).await.unwrap();
        assert_eq!(flat.len(), 1, "不递归只该拿到 a.mp4：{flat:?}");
        let after = seen.lock().unwrap().clone();
        assert_eq!(after.len(), before + 1, "不递归不该多打子目录：{after:?}");
    }

    #[tokio::test]
    async fn webdav_max_depth_limits_recursion() {
        let mut routes = std::collections::HashMap::new();
        routes.insert(
            "/dav/115/".to_string(),
            resp_for("/dav/115/", &[("sub/", true), ("x.mp4", false)]),
        );
        routes.insert(
            "/dav/115/sub/".to_string(),
            resp_for("/dav/115/sub/", &[("y.mp4", false)]),
        );
        let (port, _seen) = spawn_fake_dav(routes);
        let w = WebDav::new(&format!("http://127.0.0.1:{port}/dav"), None, None, "", client_for_test());

        let names: Vec<String> = w
            .list("/115", true, 1)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.path)
            .collect();
        assert_eq!(names, vec!["/115/sub/y.mp4", "/115/x.mp4"], "限深 1：{names:?}");
        assert_eq!(w.list("/115", false, 0).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn webdav_exists_dir_is_false_when_server_down() {
        // 端口 1 必然连不上：必须报「不存在」，**不能把错误吞成存在**
        let w = WebDav::new("http://127.0.0.1:1/dav", None, None, "", client_for_test());
        assert!(!w.exists_dir("/115").await.unwrap());
    }

    // ── 工厂 ────────────────────────────────────────────────
    #[test]
    fn factory_dispatches_by_kind() {
        let nd = Cd2Config {
            mount_root: "/mnt/cd2".into(),
            ..Default::default()
        };
        assert_eq!(
            build(&SourceConfig::default(), &nd, None, &[]).unwrap().kind(),
            SourceKind::Local
        );
        let mut dav = SourceConfig::default();
        dav.kind = "webdav".into();
        dav.base = "http://127.0.0.1:19798/dav".into();
        assert_eq!(build(&dav, &nd, None, &[]).unwrap().kind(), SourceKind::WebDav);
    }

    /// 🔴 配了 local 却没挂载根 —— 必须**明确报错**，不能静静跑完 0 个。
    #[test]
    fn local_without_mount_root_is_loud_error() {
        let err = build(&SourceConfig::default(), &Cd2Config::default(), None, &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("mount_root"), "实际：{err}");
    }

    #[test]
    fn webdav_without_base_is_loud_error() {
        let mut dav = SourceConfig::default();
        dav.kind = "webdav".into();
        let err = build(&dav, &Cd2Config::default(), None, &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("source.base"), "实际：{err}");
    }

    /// 写错 kind 必须报错 —— 回退成 local 的表现是「扫到本地空目录，跑完 0 个还不响」。
    #[test]
    fn unknown_kind_is_error() {
        assert!(SourceKind::parse("smb").is_err());
        assert_eq!(SourceKind::parse("").unwrap(), SourceKind::Local);
        assert_eq!(SourceKind::parse("DAV").unwrap(), SourceKind::WebDav);
    }

    #[test]
    fn skip_list_covers_metadata_dirs() {
        assert!(skip_name(".git"));
        assert!(skip_name("@eaDir"));
        assert!(skip_name("#recycle"));
        assert!(!skip_name("115"));
    }
}
