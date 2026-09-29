//! CloudDrive2 直链构造。
//!
//! CD2 直链格式（19798 端口，实测有效）：
//! ```text
//! http://<host>:<port>/static/http/<internal>/False/<URL编码的网盘内绝对路径>
//! ```
//! 例：`http://192.168.1.15:19798/static/http/localhost:19798/False/%2F115%2F%E7%9C%8B%E5%89%A7%2Fsign.txt`
//!
//! 三个易错点（`cd2-scraper` 已踩过，这里照抄结论、并用对拍测试钉死）：
//! 1. `/d/` 是 **AList** 的格式，用在 CD2 上**必 404** —— 别抄错。
//! 2. 路径里的**斜杠也要编码**（`%2F`），所以 quote 的 safe 集合必须为空。
//! 3. 网盘内路径是**相对 CD2 挂载根**算的，不是本机绝对路径。
//!
//! ⚠️ 本模块的 `quote_path` 必须与 Python `urllib.parse.quote(p, safe="")`
//! **逐字节一致**（`tests::parity_with_python_quote` 就是干这个的）。

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

/// Python `quote` 的 always-safe 集合：字母 / 数字 / `_` `.` `-` `~`。
/// （Python 文档：`_.-~` 之外的可打印 ASCII 全部转义，`/` 默认 safe 但要显式清空。）
const UNRESERVED: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_.-~";

/// CD2 连接与直链模板配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Cd2Config {
    /// CD2 的 HTTP 服务地址（局域网 IP —— Emby/Jellyfin 也要能访问到，别填 127.0.0.1）
    pub host: String,
    pub port: u16,
    /// CD2 挂载根在**本机**的绝对路径。网盘内路径按它算相对路径。
    pub mount_root: String,
    /// 直链模板。占位符：`{host}` `{port}` `{internal}` `{path}` `{raw_path}` `{cloud_path}` `{filename}`
    pub url_template: String,
    /// 模板里 `{internal}` 的取值。CD2 自己复制的链接就是 `localhost:19798`。
    pub internal_addr: String,
    /// 是否对 `{path}` 做 URL 编码
    pub urlencode_path: bool,
    /// quote 的 safe 集合（留空 = 全编码，与 CD2 原生行为一致）
    pub encode_safe_chars: String,
    /// 拼 URL 前给网盘路径补的前缀（CD2 里挂了多层目录时用）
    pub path_prefix: String,
    /// 写完后顺手 HEAD 一次验证链接可达
    pub verify_after_write: bool,
    pub timeout_secs: u64,
}

impl Default for Cd2Config {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 19798,
            mount_root: String::new(),
            url_template: DEFAULT_CD2_TEMPLATE.to_string(),
            internal_addr: "localhost:{port}".to_string(),
            urlencode_path: true,
            encode_safe_chars: String::new(),
            path_prefix: String::new(),
            verify_after_write: false,
            timeout_secs: 10,
        }
    }
}

pub const DEFAULT_CD2_TEMPLATE: &str = "http://{host}:{port}/static/http/{internal}/False/{path}";

/// 与 Python `urllib.parse.quote(s, safe="")` 等价的百分号编码。
///
/// 手写而不是引 `percent-encoding`：要把「`/` 必须被编码」「`~` 不编码」
/// 这两条钉死，靠库的 `AsciiSet` 很容易漏一个字符，而漏一个就是 404。
pub fn quote_path(s: &str) -> String {
    quote_path_with_safe(s, "")
}

/// 同上，但额外保留 `safe` 里出现的字节（对应 cd2-scraper 的 `encode_safe_chars`）。
pub fn quote_path_with_safe(s: &str, safe: &str) -> String {
    let safe_bytes = safe.as_bytes();
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        if UNRESERVED.contains(b) || safe_bytes.contains(b) {
            out.push(*b as char);
        } else {
            out.push('%');
            out.push(char::from_digit((b >> 4) as u32, 16).unwrap().to_ascii_uppercase());
            out.push(char::from_digit((b & 0xf) as u32, 16).unwrap().to_ascii_uppercase());
        }
    }
    out
}

/// 把本机路径规范化成 `a/b/c` 形式（统一分隔符、去掉重复斜杠、去掉末尾斜杠）。
fn norm_slashes(p: &str) -> String {
    let s = p.trim().replace('\\', "/");
    let mut out = String::with_capacity(s.len());
    let mut prev_slash = false;
    for c in s.chars() {
        if c == '/' {
            if prev_slash {
                continue;
            }
            prev_slash = true;
        } else {
            prev_slash = false;
        }
        out.push(c);
    }
    if out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

/// 算「网盘内路径」（以 `/` 开头，形如 `/115/电影/x.mp4`）。
///
/// 返回 `Err` 说明文件不在 `mount_root` 下 —— 这时**必须报错而不是硬拼**，
/// 否则会生成一个看起来正常、点开 404 的 .strm。
pub fn cloud_path_of(abs_path: &str, cfg: &Cd2Config) -> Result<String> {
    let root = norm_slashes(&cfg.mount_root);
    if root.is_empty() {
        return Err(anyhow!("没有配置 CD2 挂载根（netdisk.mount_root），无法算出网盘内路径"));
    }
    let abs = norm_slashes(abs_path);
    let rest = if abs == root {
        ""
    } else if let Some(r) = abs.strip_prefix(&format!("{root}/")) {
        r
    } else {
        return Err(anyhow!(
            "文件不在 CD2 挂载根下，算不出网盘路径：\n  {abs}\n  挂载根：{root}"
        ));
    };

    let mut cp = if rest.is_empty() {
        "/".to_string()
    } else if rest.starts_with('/') {
        rest.to_string()
    } else {
        format!("/{rest}")
    };

    let prefix = cfg.path_prefix.trim().trim_matches('/');
    if !prefix.is_empty() {
        cp = format!("/{prefix}{cp}");
    }
    Ok(cp)
}

/// 按模板拼出 CD2 直链。
pub fn build_url(abs_path: &str, cfg: &Cd2Config) -> Result<String> {
    let cloud_path = cloud_path_of(abs_path, cfg)?;
    let internal = cfg
        .internal_addr
        .replace("{host}", &cfg.host)
        .replace("{port}", &cfg.port.to_string());

    let encoded = if cfg.urlencode_path {
        quote_path_with_safe(&cloud_path, &cfg.encode_safe_chars)
    } else {
        cloud_path.clone()
    };

    let template = cfg.url_template.trim();
    if template.is_empty() {
        return Err(anyhow!("没有配置 strm 地址模板（netdisk.url_template）"));
    }

    let filename = cloud_path.rsplit('/').next().unwrap_or("").to_string();
    Ok(template
        .replace("{host}", &cfg.host)
        .replace("{port}", &cfg.port.to_string())
        .replace("{internal}", &internal)
        .replace("{path}", &encoded)
        .replace("{raw_path}", &cloud_path)
        .replace("{cloud_path}", &cloud_path)
        .replace("{filename}", &filename))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with_root(root: &str) -> Cd2Config {
        Cd2Config {
            host: "192.168.1.15".into(),
            port: 19798,
            mount_root: root.into(),
            ..Default::default()
        }
    }

    /// 🔴 与 cd2-scraper（Python）逐字节对拍。
    /// 基准值由 `urllib.parse.quote(p, safe="")` 现场产出后固化在这里 ——
    /// 改了 `quote_path` 而没同步改 cd2-scraper，这条会红。
    #[test]
    fn parity_with_python_quote() {
        let cases: &[(&str, &str)] = &[
            ("/115/看剧/sign.txt", "%2F115%2F%E7%9C%8B%E5%89%A7%2Fsign.txt"),
            (
                "/115/电影/[FANZA] MIDV-567 1080p.mp4",
                "%2F115%2F%E7%94%B5%E5%BD%B1%2F%5BFANZA%5D%20MIDV-567%201080p.mp4",
            ),
            ("/115/A B/C+D/E&F.mkv", "%2F115%2FA%20B%2FC%2BD%2FE%26F.mkv"),
            (
                "/115/～テスト～/＃１.mp4",
                "%2F115%2F%EF%BD%9E%E3%83%86%E3%82%B9%E3%83%88%EF%BD%9E%2F%EF%BC%83%EF%BC%91.mp4",
            ),
            ("/_/a~b.c-d_e.mp4", "%2F_%2Fa~b.c-d_e.mp4"),
            (
                "/115/日本/あいう えお/テスト.mp4",
                "%2F115%2F%E6%97%A5%E6%9C%AC%2F%E3%81%82%E3%81%84%E3%81%86%20%E3%81%88%E3%81%8A%2F%E3%83%86%E3%82%B9%E3%83%88.mp4",
            ),
        ];
        for (raw, want) in cases {
            assert_eq!(&quote_path(raw), want, "编码不一致：{raw}");
        }
    }

    #[test]
    fn slash_must_be_encoded() {
        assert_eq!(quote_path("/"), "%2F");
        assert!(!quote_path("/a/b").contains('/'));
    }

    #[test]
    fn cloud_path_relative_to_mount_root() {
        let cfg = cfg_with_root("/mnt/clouddrive");
        assert_eq!(
            cloud_path_of("/mnt/clouddrive/115/电影/x.mp4", &cfg).unwrap(),
            "/115/电影/x.mp4"
        );
        // Windows 分隔符与末尾斜杠都要能吃
        let cfg_win = cfg_with_root("E:\\cd2\\");
        assert_eq!(
            cloud_path_of("E:/cd2/115/a.mp4", &cfg_win).unwrap(),
            "/115/a.mp4"
        );
    }

    #[test]
    fn outside_mount_root_is_error() {
        let cfg = cfg_with_root("/mnt/clouddrive");
        let err = cloud_path_of("/other/115/x.mp4", &cfg).unwrap_err().to_string();
        assert!(err.contains("不在 CD2 挂载根下"), "实际：{err}");
    }

    #[test]
    fn empty_mount_root_is_error() {
        let cfg = Cd2Config::default();
        assert!(cloud_path_of("/mnt/x/a.mp4", &cfg).is_err());
    }

    #[test]
    fn build_url_matches_cd2_format() {
        let cfg = cfg_with_root("/mnt/clouddrive");
        let url = build_url("/mnt/clouddrive/115/看剧/sign.txt", &cfg).unwrap();
        assert_eq!(
            url,
            "http://192.168.1.15:19798/static/http/localhost:19798/False/\
             %2F115%2F%E7%9C%8B%E5%89%A7%2Fsign.txt"
        );
        // 必须是 /static/http/，不能是 AList 的 /d/
        assert!(url.contains("/static/http/"));
        assert!(!url.contains("/d/"));
    }

    #[test]
    fn path_prefix_applied() {
        let mut cfg = cfg_with_root("/mnt/clouddrive");
        cfg.path_prefix = "/115open/".into();
        assert_eq!(
            cloud_path_of("/mnt/clouddrive/电影/x.mp4", &cfg).unwrap(),
            "/115open/电影/x.mp4"
        );
    }
}
