//! 内置代理内核托管（设计见 `docs/PROXY.md`）。
//!
//! 这个模块只做一件事：**把内核的生命周期管起来**，
//! 并且保证内核没起来时**有明确的降级路径**。
//!
//! 三条硬规则：
//! 1. 内核二进制不进仓库（体积 + 授权），找不到就 `KernelMissing` 并**如实报错**，
//!    绝不静默退化成直连 —— 静默退化是 mdc-ng 那类工具最难查的失效模式。
//! 2. **自己别套自己**：内核监听 127.0.0.1，所以 `net.rs` 的默认内网豁免必须包含回环，
//!    否则 mdc 请求内核时又被自己代理一次。两处必须一起看。
//! 3. 拉订阅可能**本身就需要代理**（鸡生蛋）——所以留有 `external_proxy` 作为引导通道。

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::config::ProxyConfig;

/// 默认监听端口。刻意避开 7890 —— 本机很可能已经跑着一个 Clash，撞端口最难查。
pub const DEFAULT_PORT: u16 = 17890;
/// 等内核就绪的上限
pub const READY_TIMEOUT: Duration = Duration::from_secs(15);
/// 就绪探测间隔
pub const READY_POLL: Duration = Duration::from_millis(200);

/// 内核状态。UI 直接展示，`Failed` 必须带**给用户看的原因**。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// 未启用
    Disabled,
    /// 正在拉订阅 / 起进程 / 等端口
    Starting,
    Running { port: u16 },
    Failed { reason: String },
}

pub struct ProxyManager {
    cfg: ProxyConfig,
    /// `<数据目录>/proxy`：config.yaml、home/、kernel.log
    dir: PathBuf,
    child: Option<Child>,
    phase: Phase,
}

impl ProxyManager {
    pub fn new(dir: impl Into<PathBuf>, cfg: ProxyConfig) -> Self {
        Self {
            cfg,
            dir: dir.into(),
            child: None,
            phase: Phase::Disabled,
        }
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// **降级链**：内核在跑就用内核，否则退回用户填的外部代理，都没有就是直连（None）。
    ///
    /// 刮削器拿代理地址只走这一个函数 —— 不要在各调用点自己拼。
    pub fn effective_proxy(&self) -> Option<String> {
        match &self.phase {
            Phase::Running { port } => Some(format!("http://127.0.0.1:{port}")),
            _ => self.cfg.external_proxy.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(|s| s.to_string()),
        }
    }

    /// 启动内核。任何一步失败都落到 `Failed { reason }` 并把这个错误抛给调用方。
    ///
    /// 🔴 配置**必须由调用方传入**：manager 自己那份是启动时的副本，
    /// 用户在 UI 里改完配置后它不会自动更新（实测：改完 enabled 仍是旧值 ⇒
    /// 内核永远停在「未启用」，回落也拿不到新的 external_proxy）。
    pub async fn start(&mut self, cfg: ProxyConfig) -> Result<()> {
        self.cfg = cfg;
        self.stop();
        if !self.cfg.enabled {
            self.phase = Phase::Disabled;
            return Ok(());
        }
        self.phase = Phase::Starting;
        // 配置不全也要落到 Failed —— 否则 UI 上永远是「未启用」，看不出原因
        let url = match self
            .cfg
            .subscribe_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(u) => u.to_string(),
            None => {
                let e = anyhow!("未配置订阅链接：内置内核需要你自己的订阅");
                self.phase = Phase::Failed {
                    reason: e.to_string(),
                };
                return Err(e);
            }
        };

        // 失败一律落到 Failed，调用方（UI）就能直接展示原因
        if let Err(e) = self.boot(&url).await {
            self.phase = Phase::Failed {
                reason: e.to_string(),
            };
            return Err(e);
        }
        Ok(())
    }

    async fn boot(&mut self, url: &str) -> Result<()> {
        if self.cfg.kernel != "mihomo" {
            bail!("暂未实现内核 `{}`（当前只支持 mihomo）", self.cfg.kernel);
        }
        let bin = find_kernel(&self.cfg)?;
        let sub = fetch_subscribe(url, self.cfg.external_proxy.as_deref()).await?;
        let port = self.cfg.port;
        let text = build_kernel_config(&sub, port, self.cfg.expose_lan)?;

        std::fs::create_dir_all(&self.dir)?;
        let home = self.dir.join("home");
        std::fs::create_dir_all(&home)?;
        let cfg_path = self.dir.join("config.yaml");
        std::fs::write(&cfg_path, text)?;

        // 内核输出落盘：UI 的「内核日志」读这个文件，也便于事后排查
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("kernel.log"))
            .context("打不开 kernel.log")?;
        let err = log.try_clone()?;
        let child = Command::new(&bin)
            .arg("-f")
            .arg(&cfg_path)
            .arg("-d")
            .arg(&home)
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err))
            .spawn()
            .with_context(|| format!("起不来内核：{}", bin.display()))?;
        self.child = Some(child);

        if !wait_ready(port, READY_TIMEOUT).await {
            // 起了进程但端口始终没起来 —— 记一笔日志再失败，别只说「超时」
            let tail = read_log_tail(&self.dir.join("kernel.log"), 20);
            self.stop();
            bail!("内核端口 {port} 在 {}s 内没就绪。内核日志尾部：\n{tail}", READY_TIMEOUT.as_secs());
        }
        self.phase = Phase::Running { port };
        Ok(())
    }

    /// 停用：同步配置 + 停进程 + 清状态。
    ///
    /// 不能只调 `stop()` —— 那样 manager 里还是旧配置（回落地址不更新），
    /// 而且上次的 `Failed{reason}` 会一直留在 UI 上显得还在报错。
    pub fn disable(&mut self, cfg: ProxyConfig) {
        self.cfg = cfg;
        self.stop();
        self.phase = Phase::Disabled;
    }

    /// 停掉内核（幂等）。
    pub fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        if matches!(self.phase, Phase::Running { .. } | Phase::Starting) {
            self.phase = Phase::Disabled;
        }
    }

    /// 检查内核是否还活着（进程自己崩了要立刻反映到状态里）。
    pub fn refresh(&mut self) {
        if let Some(c) = self.child.as_mut() {
            match c.try_wait() {
                Ok(Some(st)) => {
                    self.child = None;
                    self.phase = Phase::Failed {
                        reason: format!("内核进程已退出（{}）", st),
                    };
                }
                Ok(None) => {}
                Err(e) => self.phase = Phase::Failed {
                    reason: format!("检查内核进程失败：{e}"),
                },
            }
        }
    }

    pub fn log_tail(&self, lines: usize) -> String {
        read_log_tail(&self.dir.join("kernel.log"), lines)
    }
}

/// 按 顺序找内核二进制：配置 → 环境变量 → exe 同目录 → PATH。
pub fn find_kernel(cfg: &ProxyConfig) -> Result<PathBuf> {
    let name = kernel_binary_name(&cfg.kernel)?;

    if let Some(p) = cfg.kernel_path.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let p = PathBuf::from(p);
        if p.exists() {
            return Ok(p);
        }
        bail!("配置的 kernel_path 不存在：{}", p.display());
    }
    if let Ok(p) = std::env::var("MDC_PROXY_KERNEL") {
        if !p.trim().is_empty() {
            let p = PathBuf::from(p.trim());
            if p.exists() {
                return Ok(p);
            }
            bail!("环境变量 MDC_PROXY_KERNEL 指向的文件不存在：{}", p.display());
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let c = dir.join(&name);
            if c.exists() {
                return Ok(c);
            }
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&paths) {
            let c = d.join(&name);
            if c.exists() {
                return Ok(c);
            }
        }
    }
    bail!(
        "找不到内核二进制 `{name}`：内置内核的二进制不随源码分发（体积与授权），\
         请把 {name} 放到 <数据目录>/../ 或程序同目录，或在配置里指定 proxy.kernel_path。\
         没有内核时请把 proxy.enabled 设为 false，并改用 proxy.external_proxy。"
    )
}

fn kernel_binary_name(kernel: &str) -> Result<String> {
    let base = match kernel {
        "mihomo" => "mihomo",
        "sing-box" | "singbox" => "sing-box",
        other => bail!("未知内核 `{other}`"),
    };
    Ok(if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    })
}

/// 拉订阅。**这一步可能需要代理**（订阅站本身就在墙外），所以允许传引导代理。
pub async fn fetch_subscribe(url: &str, bootstrap: Option<&str>) -> Result<String> {
    let client = crate::net::client(crate::net::HttpOpts {
        proxy: bootstrap,
        no_proxy: &[],
        timeout_secs: 30,
    })?;
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("拉订阅失败：{url}"))?
        .error_for_status()
        .with_context(|| format!("订阅站返回错误状态：{url}"))?;
    let body = resp.text().await?;
    if body.trim().is_empty() {
        bail!("订阅内容为空：{url}");
    }
    Ok(body)
}

/// 订阅内容 → 内核配置。
///
/// 只做两件事：**覆盖**我们关心的字段，其余原样保留（订阅里的节点/分组/规则不动）。
pub fn build_kernel_config(sub: &str, port: u16, expose_lan: bool) -> Result<String> {
    let text = parse_subscribe(sub)?;
    let mut v: Value = serde_yaml::from_str(&text).context("订阅不是合法 YAML")?;
    let m = v
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("订阅的 YAML 顶层不是映射"))?;

    // 没有分组的话内核起不来 —— 早点说清楚，别等端口超时
    if !m.contains_key(Value::String("proxy-groups".into())) {
        bail!("订阅里没有 proxy-groups：内核无法选节点，请换一个完整的订阅");
    }

    set(m, "mixed-port", Value::Number(port.into()));
    set(m, "mode", Value::String("rule".into()));
    set(m, "log-level", Value::String("warning".into()));
    if expose_lan {
        set(m, "allow-lan", Value::Bool(true));
        set(m, "bind-address", Value::String("*".into()));
    } else {
        // 默认只本机：最小暴露面，与既有安全底线一致
        set(m, "allow-lan", Value::Bool(false));
        set(m, "bind-address", Value::String("127.0.0.1".into()));
    }
    Ok(serde_yaml::to_string(&v)?)
}

fn set(m: &mut serde_yaml::Mapping, k: &str, v: Value) {
    m.insert(Value::String(k.to_string()), v);
}

/// 订阅有纯 YAML 和 base64(YAML) 两种形态，都要认。
///
/// ⚠️ 判据必须是「解析出来是**映射**」，不能只看「能不能解析」：
/// YAML 把任意文本都当字符串标量解析成功（`<!DOCTYPE html>…` 也能过），
/// 那样订阅站返回 403 页面时会被当成合法订阅，一路错到起内核才失败。
fn parse_subscribe(raw: &str) -> Result<String> {
    let t = raw.trim();
    if is_mapping(t) {
        return Ok(t.to_string());
    }
    if let Some(dec) = try_base64(t) {
        if is_mapping(&dec) {
            return Ok(dec);
        }
    }
    bail!("订阅内容既不是 YAML 映射，也不是 base64(YAML 映射)")
}

fn is_mapping(s: &str) -> bool {
    matches!(serde_yaml::from_str::<Value>(s), Ok(Value::Mapping(_)))
}

/// 最小 base64 解码（标准 + URL-safe）—— 只为省一个依赖，够用即可。
fn try_base64(s: &str) -> Option<String> {
    let t: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if t.is_empty() || t.len() % 4 != 0 {
        return None;
    }
    if !t
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_'))
    {
        return None;
    }
    let b = t.as_bytes();
    let mut out = Vec::with_capacity(b.len() / 4 * 3);
    for chunk in b.chunks(4) {
        let mut acc = 0u32;
        let mut bits = 0u32;
        for &c in chunk {
            if c == b'=' {
                break;
            }
            let v = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' | b'-' => 62,
                b'/' | b'_' => 63,
                _ => return None,
            } as u32;
            acc = (acc << 6) | v;
            bits += 6;
        }
        // acc 是**左对齐**的 bits 位数据：第 i 个字节要右移 `bits - 8*(i+1)`。
        // 只在 24 位满块（bits=24）时才恰好等于 >>16 / >>8 / >>0 ——
        // 末尾带 padding 的块（12/18 位）用固定位移会解出 \0，订阅直接变乱码。
        let n = (bits / 8) as usize;
        for i in 0..n {
            let shift = bits - 8 * (i as u32 + 1);
            out.push((acc >> shift) as u8);
        }
    }
    String::from_utf8(out).ok()
}

/// 等端口能连上。内核冷启动要时间，固定 sleep 不够稳，只能探。
pub async fn wait_ready(port: u16, limit: Duration) -> bool {
    let addr = format!("127.0.0.1:{port}");
    let deadline = std::time::Instant::now() + limit;
    loop {
        if let Ok(Ok(_)) = timeout(Duration::from_millis(500), TcpStream::connect(&addr)).await {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(READY_POLL).await;
    }
}

fn read_log_tail(path: &Path, lines: usize) -> String {
    let Ok(s) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let v: Vec<&str> = s.lines().collect();
    let from = v.len().saturating_sub(lines);
    v[from..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUB: &str = "\
proxies:
  - name: node-1
    type: socks5
    server: 1.2.3.4
    port: 1080
proxy-groups:
  - name: PROXY
    type: select
    proxies: [node-1]
rules:
  - MATCH,PROXY
";

    #[test]
    fn 改写订阅时保留节点并覆盖端口() {
        let out = build_kernel_config(SUB, 17890, false).unwrap();
        assert!(out.contains("node-1"), "节点必须原样保留");
        assert!(out.contains("mixed-port: 17890"));
        assert!(out.contains("allow-lan: false"));
        assert!(out.contains("bind-address: 127.0.0.1"));
        // 能再解析回来 = 生成的是合法 YAML
        let v: Value = serde_yaml::from_str(&out).unwrap();
        assert_eq!(
            v.get("mixed-port").and_then(|x| x.as_u64()),
            Some(17890),
            "端口没写进去"
        );
    }

    #[test]
    fn 放开局域网时监听地址变通配() {
        let out = build_kernel_config(SUB, 17890, true).unwrap();
        assert!(out.contains("allow-lan: true"));
        assert!(out.contains("bind-address: '*'"));
    }

    #[test]
    fn 订阅里的原值会被我们的端口覆盖() {
        let with_port = format!("mixed-port: 7890\n{SUB}");
        let out = build_kernel_config(&with_port, 17890, false).unwrap();
        let v: Value = serde_yaml::from_str(&out).unwrap();
        // 必须是我们的 17890，不是订阅里的 7890 —— 否则端口冲突
        assert_eq!(v.get("mixed-port").and_then(|x| x.as_u64()), Some(17890));
    }

    #[test]
    fn base64_订阅也要能认() {
        let enc = "cHJveGllczoKICAtIG5hbWU6IG5vZGUtMQogICAgdHlwZTogc29ja3M1CiAgICBzZXJ2ZXI6IDEuMi4zLjQKICAgIHBvcnQ6IDEwODAKcHJveHktZ3JvdXBzOgogIC0gbmFtZTogUFJPWFkKICAgIHR5cGU6IHNlbGVjdAogICAgcHJveGllczogW25vZGUtMV0KcnVsZXM6CiAgLSBNQVRDSCxQUk9YWQo=";
        let dec = try_base64(enc).expect("应当能解出 base64");
        assert!(dec.contains("proxy-groups"));
        let out = build_kernel_config(enc, 17890, false).unwrap();
        assert!(out.contains("mixed-port: 17890"));
    }

    #[test]
    fn 没有分组的订阅要早报错() {
        let bad = "proxies:\n  - name: x\n    type: socks5\n";
        let e = build_kernel_config(bad, 17890, false).unwrap_err();
        assert!(e.to_string().contains("proxy-groups"));
    }

    #[test]
    fn 非_yaml_非_base64_要报错() {
        let e = build_kernel_config("<!DOCTYPE html><html>403</html>", 17890, false).unwrap_err();
        assert!(e.to_string().contains("既不是 YAML"));
    }

    #[test]
    fn 内核二进制找不到要给出可操作的错误() {
        let cfg = ProxyConfig {
            kernel_path: Some("C:/绝对不存在/mihomo.exe".to_string()),
            ..Default::default()
        };
        let e = find_kernel(&cfg).unwrap_err();
        assert!(e.to_string().contains("不存在"));

        // 什么都不配时（PATH 里一般也没有 mihomo）应提示「不随源码分发」
        let cfg2 = ProxyConfig::default();
        let e2 = find_kernel(&cfg2).unwrap_err();
        assert!(e2.to_string().contains("不随源码分发"));
    }

    #[test]
    fn 未知内核名要报错() {
        assert!(kernel_binary_name("mihomo").is_ok());
        assert!(kernel_binary_name("sing-box").is_ok());
        assert!(kernel_binary_name("xray").is_err());
    }

    #[test]
    fn 降级链_内核没起来就用外部代理() {
        let cfg = ProxyConfig {
            enabled: true,
            external_proxy: Some("http://127.0.0.1:7890".to_string()),
            ..Default::default()
        };
        let mut m = ProxyManager::new(std::env::temp_dir().join("mdc-proxy-test"), cfg);
        // 没启动 = 退回外部代理
        assert_eq!(
            m.effective_proxy().as_deref(),
            Some("http://127.0.0.1:7890")
        );
        // 起来了就用内核
        m.phase = Phase::Running { port: 17890 };
        assert_eq!(
            m.effective_proxy().as_deref(),
            Some("http://127.0.0.1:17890")
        );
        // 崩了又退回外部
        m.phase = Phase::Failed {
            reason: "x".into(),
        };
        assert_eq!(
            m.effective_proxy().as_deref(),
            Some("http://127.0.0.1:7890")
        );
    }

    #[test]
    fn 没有外部代理时就是直连() {
        let m = ProxyManager::new(
            std::env::temp_dir().join("mdc-proxy-test2"),
            ProxyConfig::default(),
        );
        assert_eq!(m.effective_proxy(), None);
    }

    #[tokio::test]
    async fn 端口没监听时要超时返回_false() {
        // 端口 1 一般没人监听；给 600ms 上限，别让测试变慢
        assert!(!wait_ready(1, Duration::from_millis(600)).await);
    }

    #[tokio::test]
    async fn 未启用时_start_不报错且状态为_disabled() {
        let mut m = ProxyManager::new(
            std::env::temp_dir().join("mdc-proxy-test3"),
            ProxyConfig::default(),
        );
        m.start(ProxyConfig::default()).await.unwrap();
        assert_eq!(m.phase(), &Phase::Disabled);
    }

    #[tokio::test]
    async fn 没填订阅要报_user_能看懂的错() {
        let cfg = ProxyConfig {
            enabled: true,
            ..Default::default()
        };
        let mut m = ProxyManager::new(std::env::temp_dir().join("mdc-proxy-test4"), cfg.clone());
        let e = m.start(cfg).await.unwrap_err();
        assert!(e.to_string().contains("订阅"));
        assert!(matches!(m.phase(), Phase::Failed { .. }));
    }

    #[test]
    fn 停用后要清掉上次的失败原因并同步配置() {
        let mut m = ProxyManager::new(
            std::env::temp_dir().join("mdc-proxy-test6"),
            ProxyConfig {
                external_proxy: Some("http://127.0.0.1:7890".to_string()),
                ..Default::default()
            },
        );
        m.phase = Phase::Failed {
            reason: "找不到内核".into(),
        };
        // 停用 + 外部代理清空
        m.disable(ProxyConfig::default());
        assert_eq!(m.phase(), &Phase::Disabled, "停用后不该还显示失败");
        assert_eq!(m.effective_proxy(), None, "回落地址要按新配置来");
    }

    #[tokio::test]
    async fn 配置热更新后_start_要用新配置() {
        // 构造时是「未启用」，start 时传入「启用 + 外部代理」——
        // 必须按**传入的**这份走，否则 UI 上改了开关永远不生效
        let mut m = ProxyManager::new(
            std::env::temp_dir().join("mdc-proxy-test5"),
            ProxyConfig::default(),
        );
        let fresh = ProxyConfig {
            enabled: true,
            external_proxy: Some("http://127.0.0.1:7890".to_string()),
            ..Default::default()
        };
        let e = m.start(fresh).await.unwrap_err();
        assert!(e.to_string().contains("订阅"), "应当走到「缺订阅」而不是停在未启用");
        assert!(matches!(m.phase(), Phase::Failed { .. }));
        // 失败也要能回落到**新配置里**的外部代理
        assert_eq!(
            m.effective_proxy().as_deref(),
            Some("http://127.0.0.1:7890")
        );
    }
}
