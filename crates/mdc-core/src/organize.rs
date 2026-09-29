//! 文件整理：五种模式与 mdc-ng 对齐。

use crate::config::sanitize_filename;
use crate::model::OrganizeMode;
use anyhow::{anyhow, Result};
use std::path::Path;

/// 按 mode 把 source 整理为 dest（dest 是完整目标文件路径）。
pub fn apply(mode: OrganizeMode, source: &Path, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match mode {
        OrganizeMode::HardLink => {
            if dest.exists() {
                tracing::warn!(dest = %dest.display(), "目标已存在，跳过硬链");
                return Ok(());
            }
            std::fs::hard_link(source, dest).map_err(|e| {
                anyhow!(
                    "硬链失败（{}），跨盘/跨文件系统时请改用 copy/move 模式",
                    e
                )
            })?;
        }
        OrganizeMode::Copy => {
            if dest.exists() {
                tracing::warn!(dest = %dest.display(), "目标已存在，跳过复制");
                return Ok(());
            }
            std::fs::copy(source, dest)?;
        }
        OrganizeMode::Move => {
            if dest.exists() {
                return Err(anyhow!("目标已存在: {}", dest.display()));
            }
            if std::fs::rename(source, dest).is_err() {
                // 跨盘 rename 会失败，退化为 copy+delete
                std::fs::copy(source, dest)?;
                std::fs::remove_file(source)?;
            }
        }
        OrganizeMode::Symlink => {
            if dest.exists() {
                return Err(anyhow!("目标已存在: {}", dest.display()));
            }
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(source, dest)?;
            }
            #[cfg(windows)]
            {
                // Windows 需要开发者模式或管理员权限才能建符号链接
                std::os::windows::fs::symlink_file(source, dest).map_err(|e| {
                    anyhow!("符号链接失败（{}）。Windows 需开启开发者模式，或改用 hardlink/copy", e)
                })?;
            }
        }
        // 原地整理：视频不动，元数据由 pipeline 直接写在 source 旁边
        OrganizeMode::InPlace => {}
        // strm 模式：视频也不动，`.strm` 指针由 pipeline 用 CD2 直链写出。
        // 这里刻意留空而不是报错 —— 目标路径的渲染（含 .strm 后缀）也在 pipeline 里，
        // 两边分开实现会不一致。
        OrganizeMode::Strm => {}
    }
    Ok(())
}

/// 渲染命名模板。占位符 `{var}` 风格，可用变量：number/title/actor/studio/series/year。
/// （刻意不用模板引擎：单括号对用户更直观，未知占位符原样保留。）
///
/// 模板里可以用 `/` **分层**（默认的 `{actor}/{number} {title}` 就是），
/// 所以清洗必须**逐段**做 —— 对整个字符串跑 `sanitize_filename` 会把 `/` 当非法字符
/// 抹掉，模板写得再对也只能生成一层目录。
///
/// ⚠️ 每段按**字节**限长 200（单文件名上限 255 字节，中日文 3 字节/字）。
pub fn render_name(
    template: &str,
    vars: &std::collections::HashMap<&str, String>,
) -> Result<String> {
    let mut out = template.to_string();
    for (k, v) in vars {
        // ⚠️ 清洗的是**变量值**，不是整串：值里的 `/` 是「数据」（标题里出现斜杠很常见，
        // 应该变成空格），模板自己写的 `/` 是「结构」（`{actor}/{number}` 要分层）。
        // 两者混在一起处理，必然错一个。
        out = out.replace(&format!("{{{k}}}"), &sanitize_filename(v));
    }
    let mut parts: Vec<String> = Vec::new();
    for seg in out.split(['/', '\\']) {
        let seg = sanitize_filename(seg);
        let seg = seg.trim();
        // 空段直接丢掉（比如 {actor} 没刮到）—— 留个 "untitled" 目录层毫无意义
        if seg.is_empty() {
            continue;
        }
        // 🔴 防目录穿越：纯点段（`.` / `..`）一律丢掉。
        // 标题/演员名是**从刮削站点抓来的外部数据**，不能假定它不含 `..`；
        // 而上面的逐段清洗把 `/` 变成了分层符，`../../x` 会真的逃出输出根目录。
        if seg.chars().all(|c| c == '.') {
            continue;
        }
        parts.push(crate::strm::safe_filename_bytes(seg, MAX_NAME_BYTES));
    }
    if parts.is_empty() {
        return Ok("untitled".to_string());
    }
    Ok(parts.join("/"))
}

/// 文件名字节上限：留出 `.strm` / `-poster.jpg` 这些后缀的余量。
pub const MAX_NAME_BYTES: usize = 200;

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> std::collections::HashMap<&'static str, String> {
        let mut m = std::collections::HashMap::new();
        for (k, v) in pairs {
            // 模板变量名是 &'static str，这里为了测试方便做一次泄漏（测试进程内无所谓）
            m.insert(Box::leak(k.to_string().into_boxed_str()) as &'static str, v.to_string());
        }
        m
    }

    #[test]
    fn template_render() {
        let v = vars(&[("number", "ABP-123"), ("title", "Great/Title?")]);
        let name = render_name("{number} {title}", &v).unwrap();
        assert_eq!(name, "ABP-123 Great Title");
    }

    /// 默认模板 `{actor}/{number} {title}` 必须真的分层 ——
    /// 整串跑 sanitize 会把 `/` 抹掉，只能生成一层目录（e2e 测试抓到的真 bug）。
    #[test]
    fn template_supports_nested_dirs() {
        let v = vars(&[("actor", "三上悠亜"), ("number", "MIDV-567"), ("title", "テスト")]);
        assert_eq!(
            render_name("{actor}/{number} {title}", &v).unwrap(),
            "三上悠亜/MIDV-567 テスト"
        );
    }

    /// 没刮到的字段要**丢掉那一层**，不能留一个叫 "untitled" 的目录。
    #[test]
    fn empty_segments_are_dropped_not_filled_with_untitled() {
        let v = vars(&[("actor", ""), ("number", "MIDV-567"), ("title", "")]);
        assert_eq!(render_name("{actor}/{number} {title}", &v).unwrap(), "MIDV-567");
        // 全空时给个兜底，不能返回空串（否则 dest 会退化成父目录）
        let v2 = vars(&[("actor", ""), ("number", ""), ("title", "")]);
        assert_eq!(render_name("{actor}/{number}", &v2).unwrap(), "untitled");
    }

    /// 模板里写 `..` 不能逃出输出根目录。
    #[test]
    fn template_cannot_escape_out_root() {
        let v = vars(&[("title", ".."), ("number", "..")]);
        let got = render_name("{title}/{number}", &v).unwrap();
        assert!(!got.contains(".."), "实际：{got}");
    }
}
