use anyhow::{Context, Result};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::cd2::Cd2Config;
use crate::source::SourceConfig;
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
    /// 内置代理内核（详见 docs/PROXY.md）
    pub proxy: ProxyConfig,
    /// 目录源：网盘是挂成本地目录（local）还是走 WebDAV（webdav，安卓唯一可行）
    pub source: SourceConfig,
}

/// 内置代理内核。
///
/// 🔴 内核二进制**不进仓库**（体积 + 各自授权），找不到就是 `KernelMissing`，
/// 此时功能禁用并**如实报错**，绝不静默退化成直连。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxyConfig {
    /// 是否启用内置内核
    pub enabled: bool,
    /// `mihomo` | `sing-box`（当前只实现了 mihomo）
    pub kernel: String,
    /// 订阅链接。**只存用户自己填的**，不预置任何节点/订阅。
    pub subscribe_url: Option<String>,
    /// 内核二进制路径；留空时按 kernel_path → 环境变量 → exe 同目录 → PATH 顺序探测
    pub kernel_path: Option<String>,
    /// 内核监听端口（默认 17890，避开常见的 7890，因为本机可能已有 Clash）
    pub port: u16,
    /// 是否把监听放开到局域网（给 Emby / CD2 共用）；默认只本机
    pub expose_lan: bool,
    /// 兜底代理：内核没起来时用它，也是**第一次拉订阅**的引导通道
    pub external_proxy: Option<String>,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            kernel: "mihomo".to_string(),
            subscribe_url: None,
            kernel_path: None,
            port: crate::proxy::DEFAULT_PORT,
            expose_lan: false,
            external_proxy: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CommonConfig {
    /// HTTP 代理，如 http://127.0.0.1:7890 或 socks5://...，也可是内置内核的地址
    pub proxy: Option<String>,
    /// **追加**的直连白名单（域名 / IP / CIDR）。
    /// 内网（192.168.*、10.*、172.16-31.*、*.local、回环）**默认已豁免**，这里只填额外的。
    /// 理由见 `net.rs`：代理不认识内网地址会直接回 404，且现象是「时好时坏」。
    pub no_proxy: Vec<String>,
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
            no_proxy: Vec::new(),
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

    /// 造目录源（扫网盘目录用）。
    ///
    /// 代理参数一并传进去：WebDAV 地址一般是局域网，`net.rs` 的默认规则会判成直连；
    /// 万一用户把 CD2 放在远端，这里也能走代理 —— 但**绝不能**反过来让内网请求被代理吃掉。
    pub fn dir_source(&self) -> Result<std::sync::Arc<dyn crate::source::DirSource>> {
        crate::source::build(
            &self.source,
            &self.netdisk,
            self.common.proxy.as_deref(),
            &self.common.no_proxy,
        )
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
