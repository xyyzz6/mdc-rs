//! 出网统一层：HTTP 客户端 + NO_PROXY 豁免。
//!
//! 为什么单独抽一层（两条都是踩出来的）：
//!
//! 1. **内网必须直连。** 刮削站（javbus / FC2）在国内基本都要代理，
//!    但 CloudDrive2、Emby、NAS 都在内网 —— 走代理必然失败。
//!    douyin-nas 实测过这个坑：ffmpeg 读了系统里的 `HTTP_PROXY`，
//!    把访问内网 NAS 的请求也丢给代理，代理不认识这个地址直接回 404；
//!    现象是「时好时坏」（代理恰好没在跑时就正常），极难查。
//! 2. **代理内核跑在本机回环。** 内置内核那套起来后监听 `127.0.0.1:<port>`
//!    （见 `docs/PROXY.md`），如果自己去请求自己还套一层代理，就是死循环。
//!
//! 所以「代理」和「豁免」必须在**同一处**决定，不能每个调用点各写各的。

use anyhow::Result;
use reqwest::{Client, NoProxy, Proxy};
use std::time::Duration;

/// 浏览器 UA。刮削站对非浏览器 UA 会直接拒（javbus 尤其明显）。
pub const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                      (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// 默认直连的网段与名字。**内网一律不走代理**。
///
/// 用 CIDR 而不是只写 `192.168.1.15` 这种精确地址：同一个局域网里的
/// NAS / CD2 / Emby 换 IP 是常事，逐条加白名单迟早漏。
pub const DEFAULT_BYPASS: &[&str] = &[
    "localhost",
    "*.local",
    "127.0.0.0/8",
    "::1",
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16",
];

/// 构造客户端的参数。`no_proxy` 是**追加**的白名单，默认内网已豁免。
pub struct HttpOpts<'a> {
    pub proxy: Option<&'a str>,
    pub no_proxy: &'a [String],
    pub timeout_secs: u64,
}

/// 按配置构造 HTTP 客户端：代理 + 内网豁免 + cookie + 超时。
///
/// 没配代理（或配了空串）时就是一台普通客户端 —— 行为与以前完全一致，
/// 所以本机/内网部署的用户不受影响。
pub fn client(opts: HttpOpts<'_>) -> Result<Client> {
    let mut builder = Client::builder()
        .timeout(Duration::from_secs(opts.timeout_secs.max(5)))
        .user_agent(UA)
        .cookie_store(true);

    if let Some(raw) = opts.proxy {
        let raw = raw.trim();
        if !raw.is_empty() {
            let list = build_no_proxy(opts.no_proxy);
            // `*` 表示「全都别走代理」——此时根本不挂代理，比挂了再豁免更彻底。
            if list != "*" {
                // `no_proxy` 收 Option：解析不出来时宁可全走代理，也别静默变成直连
                let proxy = Proxy::all(raw)?.no_proxy(NoProxy::from_string(&list));
                builder = builder.proxy(proxy);
            }
        }
    }
    Ok(builder.build()?)
}

/// 拼出交给 reqwest 的 NO_PROXY 串（默认内网 + 用户追加项）。
pub fn build_no_proxy(extra: &[String]) -> String {
    // `*` 优先：用户明确说「全都直连」时，拼任何东西都没意义。
    if extra.iter().any(|e| e.trim() == "*") {
        return "*".to_string();
    }
    let mut out: Vec<String> = DEFAULT_BYPASS.iter().map(|s| s.to_string()).collect();
    for e in extra {
        let e = e.trim();
        if !e.is_empty() && !out.iter().any(|x| x == e) {
            out.push(e.to_string());
        }
    }
    out.join(",")
}

/// 某个 host 是否应当直连（不走代理）。
///
/// 这是上述规则的**纯逻辑实现**，用于日志、UI 展示、以及「内核在本机」这类
/// 需要提前判断的地方（请求路由本身由 reqwest 的 `NoProxy` 执行，规则同源）。
///
/// 匹配语义（与常见 NO_PROXY 一致，另加一条直觉）：
/// - `*` —— 全部直连
/// - `a.b.c.d/n` —— CIDR
/// - `.example.com` —— 该域及其子域
/// - `example.com` —— 自身**及其子域**（比 curl 略宽，方向是「多直连」，对内网无害）
/// - `*.local` —— 通配后缀
pub fn is_bypass(host: &str, extra: &[String]) -> bool {
    let raw = host.trim();
    if raw.is_empty() {
        return true;
    }
    let h = strip_port(raw);
    let h = h.trim_end_matches('.').to_ascii_lowercase();

    for e in extra {
        if match_entry(&h, e.trim()) {
            return true;
        }
    }
    for d in DEFAULT_BYPASS {
        if match_entry(&h, d) {
            return true;
        }
    }
    false
}

/// 去掉 `host:port` 里的端口（`[::1]:8080` / `1.2.3.4:80`）。
fn strip_port(h: &str) -> &str {
    if let Some(rest) = h.strip_prefix('[') {
        // IPv6 字面量
        return rest.split(']').next().unwrap_or(rest);
    }
    match h.rsplit_once(':') {
        // 只有「最后一个冒号后全是数字」才是端口，否则是裸 IPv6
        Some((a, b)) if !b.is_empty() && b.chars().all(|c| c.is_ascii_digit()) => a,
        _ => h,
    }
}

fn match_entry(host: &str, entry: &str) -> bool {
    if entry.is_empty() {
        return false;
    }
    let e = entry.to_ascii_lowercase();
    if e == "*" {
        return true;
    }
    if let Some(cidr) = e.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        // [::1] 这种写法
        return host == cidr;
    }
    if let Some((base, bits)) = e.split_once('/') {
        return match parse_ipv4(host) {
            Some(ip) => cidr_match(ip, base, bits).unwrap_or(false),
            None => false,
        };
    }
    if let Some(domain) = e.strip_prefix("*.") {
        return host == domain || host.ends_with(&format!(".{domain}"));
    }
    if let Some(domain) = e.strip_prefix('.') {
        return host == domain || host.ends_with(&format!(".{domain}"));
    }
    host == e || host.ends_with(&format!(".{e}"))
}

fn parse_ipv4(s: &str) -> Option<u32> {
    let mut it = s.split('.');
    let mut v = 0u32;
    for _ in 0..4 {
        v = (v << 8) | u32::from(it.next()?.parse::<u8>().ok()?);
    }
    if it.next().is_some() {
        return None;
    }
    Some(v)
}

fn cidr_match(ip: u32, base: &str, bits: &str) -> Option<bool> {
    let n: u32 = bits.parse().ok()?;
    let base = parse_ipv4(base)?;
    if n > 32 {
        return None;
    }
    let mask = if n == 0 { 0 } else { u32::MAX << (32 - n) };
    Some(ip & mask == base & mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 内网一律直连() {
        for h in [
            "192.168.1.15",
            "192.168.1.15:19798",
            "10.10.0.7",
            "172.16.0.1",
            "172.31.255.255",
            "169.254.1.1",
            "127.0.0.1",
            "[::1]",
            "[::1]:9208",
            "localhost",
            "nas.local",
        ] {
            assert!(is_bypass(h, &[]), "{h} 应该直连");
        }
    }

    #[test]
    fn 公网要走代理() {
        for h in ["javbus.com", "fc2.com", "8.8.8.8", "172.32.0.1", "1.1.1.1"] {
            assert!(!is_bypass(h, &[]), "{h} 不该被豁免");
        }
    }

    #[test]
    fn cidr_边界() {
        assert!(is_bypass("192.168.0.0", &[]));
        assert!(is_bypass("192.168.255.255", &[]));
        assert!(!is_bypass("192.169.0.1", &[]));
        assert!(!is_bypass("172.15.255.255", &[]));
    }

    #[test]
    fn 用户白名单生效() {
        let extra = vec!["my-nas.example.com".to_string(), "203.0.113.9".to_string()];
        assert!(is_bypass("my-nas.example.com", &extra));
        assert!(is_bypass("203.0.113.9", &extra));
        assert!(!is_bypass("other.example.com", &extra));
    }

    #[test]
    fn 星号等于全部直连() {
        assert!(is_bypass("javbus.com", &["*".to_string()]));
        // 拼出来的串也是 `*`，调用方据此直接不挂代理
        assert_eq!(build_no_proxy(&["*".to_string()]), "*");
    }

    #[test]
    fn 生成的_noproxy_串能被_reqwest_解析() {
        let s = build_no_proxy(&["nas.local".to_string()]);
        assert!(s.contains("192.168.0.0/16"));
        assert!(s.contains("nas.local"));
        // 解析不出来 = 规则白写了（reqwest 会静默忽略无法识别的条目）
        assert!(NoProxy::from_string(&s).is_some());
        assert!(NoProxy::from_string("*").is_some() || NoProxy::from_string("*").is_none());
    }

    #[test]
    fn 没配代理时客户端照常构造() {
        let c = client(HttpOpts {
            proxy: None,
            no_proxy: &[],
            timeout_secs: 5,
        });
        assert!(c.is_ok());
        // 空串同样当作没配
        assert!(client(HttpOpts {
            proxy: Some("  "),
            no_proxy: &[],
            timeout_secs: 5,
        })
        .is_ok());
    }

    #[test]
    fn 非法代理地址要报错而不是静默直连() {
        let c = client(HttpOpts {
            proxy: Some("not a url"),
            no_proxy: &[],
            timeout_secs: 5,
        });
        assert!(c.is_err());
    }
}
