//! `.strm` 指针文件生成 + 增量 manifest。
//!
//! `.strm` 是几十字节的文本，内容就是播放地址。Emby/Jellyfin 扫到它只读文本、
//! 不碰视频本身 —— 扫库从几小时变几分钟，且**不产生任何上传流量**（网盘场景的关键）。
//!
//! 三条从既有项目继承来的纪律：
//! 1. **落点固定**、监控清单与片源解耦（douyin-nas §60）；
//! 2. **`.part` 写完再 rename** —— 否则网盘/SMB 抖动会留下半份坏文件（douyin-nas §55）；
//! 3. **文件名按字节限长 200** —— 单文件名上限是 **255 字节**，中日文 3 字节/字，
//!    中文长标题很容易超（douyin-nas §66 栽过）。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `.strm` 输出的相关配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StrmConfig {
    /// `.strm` 落点根目录（**本地**固定目录，Emby/Jellyfin 扫这里）。
    /// 留空则回退到 `<数据目录>/strm`。
    pub root: String,
    /// 要监控的网盘目录（**CD2 挂载根下的本机绝对路径**，可多个；与片源解耦）
    pub jobs: Vec<String>,
    /// 是否递归子目录
    pub recursive: bool,
    /// 递归深度上限，0 = 不限
    pub max_depth: u32,
    /// 写入时带 UTF-8 BOM（Emby/Jellyfin 与多数播放器都认，不认也无害）
    pub bom: bool,
    /// manifest 路径，留空 = `<数据目录>/strm_manifest.json`
    pub manifest_path: Option<String>,
    /// 自动运行间隔（小时）。**0 = 只手动触发**。
    pub interval_hours: u32,
}

impl Default for StrmConfig {
    fn default() -> Self {
        Self {
            root: String::new(),
            jobs: Vec::new(),
            recursive: true,
            max_depth: 0,
            bom: true,
            manifest_path: None,
            interval_hours: 0,
        }
    }
}

/// 当前 unix 秒。
pub fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 定时调度 + 单飞（single-flight）运行态。
///
/// 两件事必须分开持久化：
/// - `running` **只在内存**（进程重启后就该是 false，落盘毫无意义还容易卡死）；
/// - `last_run` 落 `<数据目录>/strm_last_run`（纯 unix 秒文本）。
///   ⚠️ 别写进 `config.toml` —— 那样用户每改一次配置都会连带把时间戳改掉，
///   定时节奏被配置操作带偏。
#[derive(Debug)]
pub struct StrmRunner {
    running: std::sync::atomic::AtomicBool,
    last_run: std::sync::Mutex<Option<i64>>,
    state_path: PathBuf,
}

impl StrmRunner {
    /// `state_path` 是 last_run 的落盘位置（一般 `<数据目录>/strm_last_run`）。
    pub fn new(state_path: PathBuf) -> Self {
        let last_run = std::fs::read_to_string(&state_path)
            .ok()
            .and_then(|s| s.trim().parse::<i64>().ok());
        Self {
            running: std::sync::atomic::AtomicBool::new(false),
            last_run: std::sync::Mutex::new(last_run),
            state_path,
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn last_run(&self) -> Option<i64> {
        *self.last_run.lock().unwrap()
    }

    /// 抢坑。拿不到说明已有一轮在跑。
    ///
    /// 🔴 释放**只靠 `Drop`**，不写手动的收尾代码 —— 漏一行就是
    /// 「第一轮看着全部正常、之后一切触发都返回 already-running」的静默失效
    /// （douyin-nas 栽过：CAS 抢坑后忘了在 finally 里置回 false）。
    pub fn acquire(&self) -> anyhow::Result<StrmRunGuard<'_>> {
        use std::sync::atomic::Ordering;
        if self
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(anyhow::anyhow!("已有一轮网盘刮削在跑，本次跳过"));
        }
        Ok(StrmRunGuard { runner: self })
    }

    /// 跑成功后记时间戳并落盘。**失败不记** —— 否则一次配错要等满一个周期才重试。
    pub fn mark_done(&self) {
        let now = now_unix();
        *self.last_run.lock().unwrap() = Some(now);
        if let Some(parent) = self.state_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&self.state_path, now.to_string()) {
            tracing::warn!(error = %e, "写 last_run 失败（只影响下次调度时机）");
        }
    }

    /// 下一次该跑的时刻；`interval_hours = 0` 表示不定时。
    ///
    /// 从没跑过 → 立刻该跑（冷启动补偿：进程不常驻，等满一个周期会永远赶不上）。
    pub fn next_run_at(&self, interval_hours: u32) -> Option<i64> {
        if interval_hours == 0 {
            return None;
        }
        let interval = interval_hours as i64 * 3600;
        Some(match self.last_run() {
            Some(t) => t + interval,
            None => now_unix(),
        })
    }
}

/// 单飞令牌。**丢掉它就释放**（含 panic 展开路径）。
pub struct StrmRunGuard<'a> {
    runner: &'a StrmRunner,
}

impl Drop for StrmRunGuard<'_> {
    fn drop(&mut self) {
        self.runner
            .running
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// 文件名按**字节**限长后的安全名。
///
/// 超长时：按字节截断 → 退到合法 UTF-8 边界 → 追加 8 位哈希防重名。
/// 没超长的名字**原样返回**（不做任何「优化」，避免改坏用户看得懂的名字）。
pub fn safe_filename_bytes(name: &str, max_bytes: usize) -> String {
    let s = name.trim();
    if s.is_empty() {
        return "untitled".to_string();
    }
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let digest = format!("{:08x}", fnv1a(s.as_bytes()));
    let budget = max_bytes.saturating_sub(digest.len() + 1).max(8);
    let mut cut = budget.min(s.len());
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    let head = s[..cut].trim_end_matches(['.', ' ']);
    format!("{head}-{digest}")
}

/// 稳定的 8 位哈希（不引 md5，避免为一个防重名拉依赖）。
fn fnv1a(data: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in data {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// 写 `.strm`：`[BOM] + url + \n`。
///
/// 走 `.part` → `rename`，任何一步失败都不在媒体库里留半份文件。
pub fn write_strm(path: &Path, url: &str, bom: bool) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建 strm 目录失败：{}", parent.display()))?;
    }
    let body = format!("{}\n", url.trim());
    let mut bytes = Vec::with_capacity(body.len() + 3);
    if bom {
        bytes.extend_from_slice("\u{FEFF}".as_bytes());
    }
    bytes.extend_from_slice(body.as_bytes());

    let tmp = path.with_extension("strm.part");
    std::fs::write(&tmp, &bytes)
        .with_context(|| format!("写入临时文件失败：{}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| {
        let _ = std::fs::remove_file(&tmp);
        format!("重命名失败：{} -> {}", tmp.display(), path.display())
    })?;
    Ok(())
}

/// 读 `.strm`，顺带清掉 BOM 与首尾空白。
pub fn read_strm(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .trim_start_matches('\u{FEFF}')
        .trim()
        .to_string()
}

/// 增量 manifest：`源文件绝对路径 -> (strm 落点, 签名)`。
///
/// 签名就是 strm 内容（URL）。**不删旧条目** —— 网盘抖动/暂时挂载不上时，
/// 恢复后仍能命中跳过；代价是缓慢增长，可接受（douyin-nas §55 的结论）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub entries: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub out: String,
    pub sig: String,
}

impl Manifest {
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "manifest 解析失败，按空清单处理（会全量重写一轮，幂等无害）");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let raw = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.part");
        std::fs::write(&tmp, raw)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// 命中「内容没变 + 文件还在」才返回 true。
    pub fn is_fresh(&self, src: &str, sig: &str) -> bool {
        match self.entries.get(src) {
            Some(e) => e.sig == sig && Path::new(&e.out).exists(),
            None => false,
        }
    }

    pub fn put(&mut self, src: &str, out: &str, sig: &str) {
        self.entries.insert(
            src.to_string(),
            Entry {
                out: out.to_string(),
                sig: sig.to_string(),
            },
        );
    }
}

/// 扫描目录下的视频文件。跳过隐藏目录（`.` 开头）与 `@eaDir` 之类的元数据目录。
///
/// `max_depth = 0` 表示不限层数；`recursive = false` 时只看第一层。
pub fn scan_source_dir(dir: &Path, recursive: bool, max_depth: u32) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !dir.is_dir() {
        return out;
    }
    walk(dir, 0, recursive, max_depth, &mut out);
    out.sort();
    out
}

fn walk(dir: &Path, depth: u32, recursive: bool, max_depth: u32, out: &mut Vec<PathBuf>) {
    if max_depth > 0 && depth > max_depth {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // 隐藏目录 / 群晖缩略图目录：扫进去纯属浪费请求，还可能触发风控
        // （名单只有一份，在 `source::skip_name`，两个目录源共用）
        if crate::source::skip_name(&name) {
            continue;
        }
        if p.is_dir() {
            if recursive {
                walk(&p, depth + 1, recursive, max_depth, out);
            }
            continue;
        }
        if crate::parser::is_video_file(&p) {
            out.push(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mdc_strm_test_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn bom_and_newline_are_byte_exact() {
        let d = tmpdir("bom");
        let p = d.join("a.strm");
        write_strm(&p, "http://h/x.mp4", true).unwrap();
        let raw = std::fs::read(&p).unwrap();
        assert_eq!(raw, "\u{FEFF}http://h/x.mp4\n".as_bytes());
        // 不带 BOM
        write_strm(&p, "http://h/x.mp4", false).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"http://h/x.mp4\n");
    }

    #[test]
    fn write_is_atomic_no_part_left() {
        let d = tmpdir("atomic");
        let p = d.join("sub/b.strm");
        write_strm(&p, "http://h/y.mp4", true).unwrap();
        assert!(p.exists());
        assert!(!d.join("sub/b.strm.part").exists(), ".part 残留了");
        assert_eq!(read_strm(&p), "http://h/y.mp4");
    }

    #[test]
    fn byte_limit_keeps_short_names_untouched() {
        assert_eq!(safe_filename_bytes("MIDV-567 1080p", 200), "MIDV-567 1080p");
    }

    #[test]
    fn byte_limit_cuts_chinese_on_char_boundary() {
        let long = "あ".repeat(200); // 600 字节
        let got = safe_filename_bytes(&long, 200);
        assert!(got.len() <= 200, "超过 200 字节：{}", got.len());
        // 必须是合法 UTF-8（否则 to_string 会 panic），且带防重名后缀
        assert!(got.is_char_boundary(got.len()));
        assert_eq!(got.rsplit('-').next().unwrap().len(), 8);
    }

    #[test]
    fn manifest_skips_only_when_file_still_exists() {
        let d = tmpdir("manifest");
        let mf = d.join("strm_manifest.json");
        let out = d.join("out/x.strm");
        write_strm(&out, "http://h/x.mp4", true).unwrap();

        let mut m = Manifest::load(&mf);
        m.put("/src/x.mp4", &out.to_string_lossy(), "http://h/x.mp4");
        m.save(&mf).unwrap();

        let m2 = Manifest::load(&mf);
        assert!(m2.is_fresh("/src/x.mp4", "http://h/x.mp4"), "内容没变应命中跳过");
        assert!(!m2.is_fresh("/src/x.mp4", "http://h/CHANGED.mp4"), "URL 变了不该跳过");
        assert!(!m2.is_fresh("/src/other.mp4", "http://h/x.mp4"), "没记录过不该跳过");

        std::fs::remove_file(&out).unwrap();
        assert!(!m2.is_fresh("/src/x.mp4", "http://h/x.mp4"), "落点被删了不该跳过");
    }

    #[test]
    fn scan_filters_and_recurses() {
        let d = tmpdir("scan");
        std::fs::create_dir_all(d.join("sub/deep")).unwrap();
        std::fs::create_dir_all(d.join(".hidden")).unwrap();
        std::fs::write(d.join("a.mp4"), b"x").unwrap();
        std::fs::write(d.join("b.txt"), b"x").unwrap();
        std::fs::write(d.join("sub/c.mkv"), b"x").unwrap();
        std::fs::write(d.join("sub/deep/d.mp4"), b"x").unwrap();
        std::fs::write(d.join(".hidden/e.mp4"), b"x").unwrap();

        let r = scan_source_dir(&d, true, 0);
        let names: Vec<String> = r
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["a.mp4", "c.mkv", "d.mp4"], "实际：{names:?}");

        // 不递归 = 只有第一层
        let r2 = scan_source_dir(&d, false, 0);
        assert_eq!(r2.len(), 1);
        // 限深 1 = 只到 sub/
        let r3 = scan_source_dir(&d, true, 1);
        assert_eq!(r3.len(), 2);
    }

    /// 🔴 单飞：第二轮必须被拒，而且**令牌 drop 后必须能再抢到**。
    /// 后半句才是关键 —— douyin-nas 栽的就是「第一轮正常、之后永远 already-running」。
    #[test]
    fn single_flight_blocks_then_releases() {
        let d = tmpdir("runner");
        let r = StrmRunner::new(d.join("strm_last_run"));

        {
            let g1 = r.acquire().expect("第一次应该抢得到");
            assert!(r.is_running());
            assert!(r.acquire().is_err(), "第二轮必须被拒");
            drop(g1);
        }
        assert!(!r.is_running(), "令牌 drop 后必须释放");
        assert!(r.acquire().is_ok(), "释放后必须能再抢到");
    }

    /// panic 展开路径也要释放（RAII 的意义就在这）。
    #[test]
    fn guard_releases_on_panic() {
        let d = tmpdir("runner_panic");
        let r = StrmRunner::new(d.join("strm_last_run"));
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = r.acquire().unwrap();
            panic!("boom");
        }));
        assert!(res.is_err());
        assert!(!r.is_running(), "panic 之后没释放 → 定时任务会永久静默失效");
    }

    #[test]
    fn last_run_persists_and_survives_restart() {
        let d = tmpdir("runner_persist");
        let p = d.join("strm_last_run");
        let r = StrmRunner::new(p.clone());
        assert_eq!(r.last_run(), None, "从没跑过应为 None");
        r.mark_done();
        let t = r.last_run().unwrap();

        // 模拟进程重启
        let r2 = StrmRunner::new(p);
        assert_eq!(r2.last_run(), Some(t), "last_run 必须跨重启保留");
    }

    /// 冷启动补偿：从没跑过 → 立刻该跑；跑过 → last_run + interval。
    #[test]
    fn next_run_at_does_cold_start_catchup() {
        let d = tmpdir("runner_next");
        let r = StrmRunner::new(d.join("strm_last_run"));

        assert_eq!(r.next_run_at(0), None, "0 小时 = 不定时");

        let before = now_unix();
        let next = r.next_run_at(6).unwrap();
        assert!(next <= before + 1, "从没跑过应立刻该跑，实际 {next} vs {before}");

        r.mark_done();
        let t = r.last_run().unwrap();
        assert_eq!(r.next_run_at(6).unwrap(), t + 6 * 3600);
    }

    /// 失败**不该**记时间戳，否则一次配错要干等一个周期。
    #[test]
    fn failed_run_does_not_mark_done() {
        let d = tmpdir("runner_fail");
        let r = StrmRunner::new(d.join("strm_last_run"));
        {
            let _g = r.acquire().unwrap();
            // 故意不调 mark_done —— 模拟跑失败
        }
        assert_eq!(r.last_run(), None);
        assert!(!r.is_running());
    }
}
