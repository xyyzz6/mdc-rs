//! 从文件名解析番号、分段、分辨率等线索。
//!
//! 对齐 mdc-ng 的能力子集：先支持最常见的命名形态，跑通闭环后再逐步扩展。

use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ParsedFilename {
    pub number: Option<String>,
    /// 多分段（CD1/CD2、A/B）
    pub part: Option<String>,
    /// 720p / 1080p / 2160p / 4k / 8k（统一小写）
    pub resolution: Option<String>,
    /// 清理后的标题提示（去扩展名、去分段/分辨率标记）
    pub title_hint: Option<String>,
    pub is_fc2: bool,
}

/// 分辨率 token。用 `token_re` 包起来（见下），不用 `\b`。
const RESOLUTION_BODY: &str = r"(?i:(2160p|1080p|720p|480p|4k|8k|2k))";

/// 构造「独立 token」正则：token 两侧不能紧挨字母/数字。
///
/// 🔴 **不能用 `\b` 做边界。** `_` 在正则里是**单词字符**，所以 `\b` 在
/// `MIDV-567_1080p` 的 `7` 与 `_` 之间、`1pondo_080918_002` 的 `_` 与 `0` 之间
/// **都不成立** —— 实测 `MIDV-567_1080p`、`xxx_MIDV-567`、`SSIS00424_1080p`
/// 这些**极常见**的命名全都解析不出来。而 `regex` crate 又不支持 look-around，
/// 只能用 `(?:^|[^0-9A-Za-z])` / `(?:$|[^0-9A-Za-z])` 这种非捕获守卫。
fn token_re(body: &str) -> Regex {
    Regex::new(&format!(r"(?:^|[^0-9A-Za-z]){body}(?:$|[^0-9A-Za-z])")).unwrap()
}

/// 主入口：解析一个文件名（带或不带扩展名均可）。
pub fn parse_filename(input: &str) -> ParsedFilename {
    let mut out = ParsedFilename::default();
    let name = std::path::Path::new(input)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(input)
        .to_string();

    // 分辨率
    if let Some(m) = token_re(RESOLUTION_BODY).captures(&name) {
        out.resolution = Some(m[1].to_lowercase());
    }

    // 分段：CD1 / Disc B / Part 2
    let part_re = token_re(r"(?i:(?:cd|disc|part)[\s._-]?([0-9]|[a-d]))");
    if let Some(m) = part_re.captures(&name) {
        out.part = Some(m[1].to_uppercase());
    }

    // FC2 系列：FC2-PPV-3141592 / fc2ppv 3141592
    let fc2_re = token_re(r"(?i:fc2[-_ ]?(?:ppv)?[-_ ]?(\d{5,8}))");
    if let Some(m) = fc2_re.captures(&name) {
        out.is_fc2 = true;
        out.number = Some(format!("FC2-PPV-{}", &m[1]));
    }

    // 含数字的厂牌前缀：T28620 / T28-620（T28 这个厂牌码自带数字，
    // 通用连字符形态的 `[A-Z]{2,7}-` 和紧凑形态的 `[A-Z]{2,5}\d` 都认不出它）
    if out.number.is_none() {
        if let Some(n) = parse_numeric_prefix(&name) {
            out.number = Some(n);
        }
    }

    // 带连字符标准番号：ABP-123 / MIDV-567（要求字母 2-7 + 数字 2-5）
    // 排除分辨率token误命中（4K 等），因为要求数字部分 ≥2 位且前面是纯字母
    if out.number.is_none() {
        if let Some(m) = token_re(r"(?i:([A-Z]{2,7})-(\d{2,5}))").captures(&name) {
            out.number = Some(format!("{}-{}", m[1].to_uppercase(), &m[2]));
        }
    }

    // 无连字符紧凑形态：SSIS00424 / MIDV567（字母 2-5 + 数字 3-6，前导零去除）
    // ⚠️ 刻意不加 `(?i)`：小写会让 `video1080` 这类词被误当番号
    if out.number.is_none() {
        if let Some(m) = token_re(r"([A-Z]{2,5})(\d{3,6})").captures(&name) {
            let letters = m[1].to_uppercase();
            let digits = m[2].trim_start_matches('0');
            out.number = Some(format!("{letters}-{digits}"));
        }
    }

    // 無碼「日期式」番号：MMDDYY[-_]NNN（1pondo / Caribbeancom / 10musume / pacopacomama 等）。
    // javbus 的 slug 就是这个形态（`080918_002`、`091626-001`）。
    // ⚠️ 必须校验月/日是否合法，否则 `123456_001` 这种随机数字串会被误当番号。
    if out.number.is_none() {
        if let Some(n) = parse_date_style(&name) {
            out.number = Some(n);
        }
    }

    // Tokyo-Hot：`n1234`（字母 n + 4~5 位数字）。放最后，优先级最低。
    if out.number.is_none() {
        if let Some(m) = token_re(r"(?i:n(\d{4,5}))").captures(&name) {
            out.number = Some(format!("n{}", &m[1]));
        }
    }

    // 标题提示：去掉分段/分辨率 token 后的剩余内容（信息量足够时才保留）
    let mut hint = name.clone();
    if let Some(num) = &out.number {
        let loose = Regex::new(&format!(
            r"(?i){}\s*",
            regex::escape(num.replace('-', "").as_str())
        ))
        .unwrap();
        hint = loose.replace_all(&hint, "").to_string();
        let dashed = Regex::new(&format!(r"(?i){}\s*", regex::escape(num))).unwrap();
        hint = dashed.replace_all(&hint, "").to_string();
    }
    // ⚠️ 这两个都用 `token_re`，匹配会**吃掉两侧各一个分隔符**，
    // 所以替换成空格而不是空串，否则相邻词会被粘在一起
    hint = part_re.replace_all(&hint, " ").to_string();
    hint = token_re(RESOLUTION_BODY).replace_all(&hint, " ").to_string();
    let brackets = Regex::new(r"(?i)(?:fc2|ppv)[-_\s]*\d{5,8}").unwrap();
    hint = brackets.replace_all(&hint, "").to_string();
    let tidy = Regex::new(r"[\[\](){}]+").unwrap();
    let hint = tidy.replace_all(hint.trim(), " ").trim().to_string();
    if hint.chars().count() >= 2 {
        out.title_hint = Some(hint);
    }

    out
}

/// 含数字的厂牌前缀表：`T28620` / `T28-620` → `T28-620`。
///
/// 🔴 **只放实测过的前缀。** 猜出来的前缀会把别的片子解析成**错的番号**，
/// 而错番号会静默贴上别人的元数据 —— 比解析失败严重得多。
/// 加一条之前先在 javbus 上搜 `<前缀>-<数字>`，确认 slug 就是这个形态。
///
/// 已知：`T28`（实测 javbus slug 就是 `T28-620`）。
const NUMERIC_PREFIXES: &[&str] = &["T28"];

fn parse_numeric_prefix(name: &str) -> Option<String> {
    for p in NUMERIC_PREFIXES {
        let body = format!(r"(?i:{}[-_ ]?(\d{{3,5}}))", regex::escape(p));
        if let Some(m) = token_re(&body).captures(name) {
            return Some(format!("{}-{}", p.to_uppercase(), &m[1]));
        }
    }
    None
}

/// 無碼「日期式」番号：`MMDDYY[-_]NNN`，**保留原分隔符**
/// （javbus 的 slug 就是这个形态：`080918_002`、`091626-001`）。
///
/// ⚠️ 必须校验月/日合法，否则 `123456_001` 这种随机数字串会被误当番号。
/// 这条校验也顺带挡住了「长番号 + 下划线 + 数字」的误命中：
/// `ABP-12345_78` 里 mm=12、dd=34 不合法 → 不会被当成日期式。
fn parse_date_style(name: &str) -> Option<String> {
    let m = token_re(r"(\d{6})([-_])(\d{2,3})").captures(name)?;
    let digits = m[1].to_string();
    let mm: u32 = digits.get(0..2)?.parse().ok()?;
    let dd: u32 = digits.get(2..4)?.parse().ok()?;
    if !(1..=12).contains(&mm) || !(1..=31).contains(&dd) {
        return None;
    }
    Some(format!("{}{}{}", digits, &m[2], &m[3]))
}

/// 视频文件扩展名
pub fn is_video_file(path: &std::path::Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()).as_deref(),
        Some("mp4" | "mkv" | "avi" | "wmv" | "mov" | "ts" | "m2ts" | "flv" | "webm" | "iso")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashed_number() {
        let p = parse_filename("[FANZA] MIDV-567 1080p.mp4");
        assert_eq!(p.number.as_deref(), Some("MIDV-567"));
        assert_eq!(p.resolution.as_deref(), Some("1080p"));
    }

    #[test]
    fn fc2_number() {
        let p = parse_filename("FC2-PPV-3141592 4K.mp4");
        assert_eq!(p.number.as_deref(), Some("FC2-PPV-3141592"));
        assert!(p.is_fc2);
        assert_eq!(p.resolution.as_deref(), Some("4k"));
    }

    #[test]
    fn compact_number() {
        let p = parse_filename("SSIS00424.avi");
        assert_eq!(p.number.as_deref(), Some("SSIS-424"));
    }

    #[test]
    fn multi_part() {
        let p = parse_filename("ABP-123 CD2.mp4");
        assert_eq!(p.number.as_deref(), Some("ABP-123"));
        assert_eq!(p.part.as_deref(), Some("2"));
    }

    #[test]
    fn no_number() {
        let p = parse_filename("family_holiday_video.mp4");
        assert!(p.number.is_none());
    }

    /// 含数字的厂牌前缀（T28 自带数字，通用形态都认不出）。
    #[test]
    fn numeric_studio_prefix() {
        assert_eq!(
            parse_filename("T28620.mp4").number.as_deref(),
            Some("T28-620")
        );
        assert_eq!(
            parse_filename("[T28] T28-620 1080p.mp4").number.as_deref(),
            Some("T28-620"),
            "已经是连字符形态也要认（T28 含数字，通用连字符形态认不出）"
        );
    }

    /// 無碼日期式番号：保留原分隔符，与 javbus 的 slug 一致。
    #[test]
    fn date_style_uncensored() {
        assert_eq!(
            parse_filename("080918_002.mp4").number.as_deref(),
            Some("080918_002")
        );
        assert_eq!(
            parse_filename("091626-001.mp4").number.as_deref(),
            Some("091626-001")
        );
        // 厂牌前缀在文件名里时，要能从中抽出日期式番号
        assert_eq!(
            parse_filename("Caribbeancom-091626-001.mp4").number.as_deref(),
            Some("091626-001")
        );
        assert_eq!(
            parse_filename("1pondo_080918_002.mp4").number.as_deref(),
            Some("080918_002")
        );
        assert_eq!(
            parse_filename("10musume-092426_01.mp4").number.as_deref(),
            Some("092426_01")
        );
    }

    /// 🔴 日期式的月/日校验：随机数字串不能被误当番号。
    #[test]
    fn date_style_rejects_impossible_dates() {
        // 月 20 不存在
        assert!(parse_filename("201312_15.mp4").number.is_none());
        // 日 34 不存在
        assert!(parse_filename("123456_78.mp4").number.is_none());
        // 6 位数字超出通用连字符形态（最多 5 位）的范围，日期式校验（月 12/日 34）也不合法
        // → 认不出是**对的**，宁可失败也别猜
        assert!(parse_filename("ABP-123456_78.mp4").number.is_none());
    }

    /// 🔴 `_` 在正则里是**单词字符**，所以 `\b` 在它旁边不成立 ——
    /// 这组命名以前**全都解析不出来**（`番号_分辨率` 是极常见的命名）。
    #[test]
    fn underscore_adjacent_tokens_parse() {
        for (f, want) in [
            ("MIDV-567_1080p.mp4", "MIDV-567"),
            ("xxx_MIDV-567.mp4", "MIDV-567"),
            ("SSIS00424_1080p.mp4", "SSIS-424"),
            ("xxx_SSIS00424.mp4", "SSIS-424"),
            ("FC2-PPV-3141592_1080p.mp4", "FC2-PPV-3141592"),
            ("T28-620_1080p.mp4", "T28-620"),
            ("1pondo_080918_002.mp4", "080918_002"),
        ] {
            assert_eq!(parse_filename(f).number.as_deref(), Some(want), "{f}");
        }

        // 分辨率（同一个 `\b` 坑）
        assert_eq!(
            parse_filename("MIDV-567_1080p.mp4").resolution.as_deref(),
            Some("1080p")
        );
        // 分段（也是同一个坑）
        let q = parse_filename("ABP-123_CD2.mp4");
        assert_eq!(q.part.as_deref(), Some("2"));
        assert_eq!(q.number.as_deref(), Some("ABP-123"));
    }

    /// Tokyo-Hot 的 `nNNNN`（放最后、优先级最低）。
    #[test]
    fn tokyo_hot_style() {
        assert_eq!(parse_filename("n1234.mp4").number.as_deref(), Some("n1234"));
        assert_eq!(
            parse_filename("Tokyo-Hot n12345 1080p.mp4").number.as_deref(),
            Some("n12345")
        );
        // 不能从长单词中间咬出来（`Chin1234` 里的 n 前面是字母，没有词边界）
        assert!(parse_filename("Chin1234.mp4").number.is_none());
        // 位数不够不算
        assert!(parse_filename("n123.mp4").number.is_none());
    }

    /// 新形态不能抢走已有形态的匹配。
    #[test]
    fn new_patterns_do_not_steal_existing_ones() {
        assert_eq!(
            parse_filename("[FANZA] MIDV-567 1080p.mp4").number.as_deref(),
            Some("MIDV-567")
        );
        assert_eq!(
            parse_filename("SSIS00424.avi").number.as_deref(),
            Some("SSIS-424")
        );
        assert_eq!(
            parse_filename("FC2-PPV-3141592 4K.mp4").number.as_deref(),
            Some("FC2-PPV-3141592")
        );
        // 只有分辨率的文件名不该产出番号
        assert!(parse_filename("1080p.mp4").number.is_none());
        assert!(parse_filename("movie_2023-12-15.mp4").number.is_none());
    }
}
