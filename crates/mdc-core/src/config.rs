use anyhow::{Context, Result};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::cd2::Cd2Config;
use crate::strm::StrmConfig;

/// 全局配置。TOML 持久化到 `<数据目录>/config.toml`。
///
/// 数据目录解析顺序：
/// 1. 环境变量 `MDC_CONFIG_PATH`
/// 2. 当前工作目录下的 `./data`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub common: CommonConfig,
    pub scrape: ScrapeConfig,
    pub naming: NamingConfig,
    pub translate: TranslateConfig,
    pub auth: AuthConfig,
    /// 网盘接入（当前是 CloudDrive2 直链）
    pub netdisk: Cd2Config,
    /// `.strm` 输出
    pub strm: StrmConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CommonConfig {
    /// HTTP 代理，如 http://127.0.0.1:7890 或 socks5://...
    pub proxy: Option<String>,
    /// 刮削并发线程数
    pub scrape_thread_count: u32,
    /// 单请求超时（秒）
    pub timeout_secs: u64,
    /// 媒体库根目录（移动端/桌面端可为空，走手动选择）
    pub media_dirs: Vec<String>,
}

impl Default for CommonConfig {
    fn default() -> Self {
        Self {
            proxy: None,
            scrape_thread_count: 4,
            timeout_secs: 30,
            media_dirs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScrapeConfig {
    /// 刮削源优先级，从高到低；未列出的按注册顺序兜底
    pub priorities: Vec<String>,
    pub retry_times: u32,
    /// FlareSolverr 地址，用于过 Cloudflare，如 http://127.0.0.1:8191
    pub flaresolverr: Option<String>,
    /// 要**停用**的源 id（如 `javbus`）。内置源默认全开，这里列出的会被跳过。
    pub disabled: Vec<String>,
}

impl Default for ScrapeConfig {
    fn default() -> Self {
        Self {
            priorities: Vec::new(),
            retry_times: 3,
            flaresolverr: None,
            disabled: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NamingConfig {
    /// 目标目录模板（相对媒体库根），可用变量：number/title/actor/studio/year
    pub folder_template: String,
    /// 文件名模板（不含扩展名）
    pub file_template: String,
}

impl Default for NamingConfig {
    fn default() -> Self {
        Self {
            folder_template: "{actor}/{number} {title}".to_string(),
            file_template: "{number} {title}".to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TranslateConfig {
    /// none | openai | deepl | deeplx | google
    pub engine: String,
    pub openai_key: Option<String>,
    pub openai_base: Option<String>,
    pub openai_model: Option<String>,
    pub deepl_endpoint: Option<String>,
    pub deepl_api_key: Option<String>,
    /// 网络字幕/标题附加中文字幕文件
    pub auto_translate_title: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthConfig {
    /// 两者都配置才启用登录鉴权
    pub username: Option<String>,
    pub password: Option<String>,
}

impl AppConfig {
    pub fn data_dir() -> PathBuf {
        if let Ok(p) = std::env::var("MDC_CONFIG_PATH") {
            if !p.trim().is_empty() {
                return PathBuf::from(p);
            }
        }
        PathBuf::from("./data")
    }

    pub fn load() -> Result<Self> {
        let dir = Self::data_dir();
        let path = dir.join("config.toml");
        if !path.exists() {
            let cfg = Self::default();
            cfg.save()?;
            return Ok(cfg);
        }
        let raw = fs::read_to_string(&path).with_context(|| format!("读取 {}", path.display()))?;
        let cfg = toml::from_str(&raw).context("解析 config.toml")?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let dir = Self::data_dir();
        fs::create_dir_all(&dir)?;
        let raw = toml::to_string_pretty(self)?;
        fs::write(dir.join("config.toml"), raw)?;
        Ok(())
    }

    pub fn auth_enabled(&self) -> bool {
        self.auth.username.is_some() && self.auth.password.is_some()
    }

    /// JWT 签名密钥：首次调用时随机生成并写入数据目录。
    /// （mdc-ng 把密钥硬编码在程序里导致任何实例的 token 都可伪造，这里必须避免。）
    pub fn jwt_secret() -> Result<String> {
        let dir = Self::data_dir();
        fs::create_dir_all(&dir)?;
        let path = dir.join("jwt_secret");
        if path.exists() {
            return Ok(fs::read_to_string(&path)?.trim().to_string());
        }
        let mut buf = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut buf);
        let secret: String = buf.iter().map(|b| format!("{b:02x}")).collect();
        fs::write(&path, &secret)?;
        Ok(secret)
    }

    /// 自定义刮削源目录：`<数据目录>/providers/*.yaml`
    pub fn providers_dir() -> PathBuf {
        Self::data_dir().join("providers")
    }

    /// `.strm` 落点根目录。配置为空时回退到 `<数据目录>/strm` ——
    /// 这样「什么都不填」也能跑，且不会往用户媒体库里乱写。
    pub fn strm_root(&self) -> PathBuf {
        let r = self.strm.root.trim();
        if r.is_empty() {
            Self::data_dir().join("strm")
        } else {
            PathBuf::from(r)
        }
    }

    /// 增量 manifest 路径。配置为空时 `<数据目录>/strm_manifest.json`。
    pub fn strm_manifest_path(&self) -> PathBuf {
        match self.strm.manifest_path.as_ref().map(|s| s.trim()) {
            Some(p) if !p.is_empty() => PathBuf::from(p),
            _ => Self::data_dir().join("strm_manifest.json"),
        }
    }
}

pub fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            _ => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

pub fn ensure_dir(p: &Path) -> Result<()> {
    if !p.exists() {
        fs::create_dir_all(p)?;
    }
    Ok(())
}
