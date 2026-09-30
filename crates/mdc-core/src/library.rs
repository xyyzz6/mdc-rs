//! 自带影视库（P4a）：扫 `.strm` 输出目录，聚合出「番号 → 可播条目」。
//!
//! 设计（docs/LIBRARY.md）：
//! - **不加任何外部组件**：数据就是 strm_root 里已落盘的三件套
//!   `<名>.strm + <名>.nfo + <名>.jpg / <名>-poster.jpg`，外加文件名反推的番号；
//! - 一个番号可能有多个文件（多 part / 多分辨率）→ 聚合成一条，files 全列出；
//! - 元数据以 **NFO 为准**（里面是刮削/人工精选的最终结果），NFO 缺失才退回文件名解析；
//! - 每次请求现扫（几百个小文件的读在几十毫秒级），**不做缓存不落库** ——
//!   保持「strm 目录 = 唯一事实」的一致性，P4b 引入进度记录时才需要 DB。

use crate::parser::parse_filename;
use crate::strm::read_strm;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// 单个可播文件（一个 `.strm`）。
#[derive(Debug, Clone, Serialize)]
pub struct LibFile {
    /// 展示名：去掉 `.strm` 后缀的文件名（同番号多文件时靠它区分 part/分辨率）
    pub name: String,
    /// 播放直链（strm 文件内容，通常指向 CD2 `/static/http/...`）
    pub url: String,
    /// .strm 文件自身路径（「移出库」删除用）
    pub dest: String,
}

/// 一条影视库条目 = 一个番号。
#[derive(Debug, Clone, Serialize)]
pub struct LibEntry {
    pub number: String,
    pub title: String,
    pub year: String,
    pub premiered: String,
    pub actors: Vec<String>,
    pub tags: Vec<String>,
    pub studio: Option<String>,
    pub runtime_min: Option<u32>,
    /// 海报文件（`-poster.jpg` 优先，其次同名 `.jpg`）；None = 前端画占位块
    pub poster: Option<PathBuf>,
    pub files: Vec<LibFile>,
}

/// 从 NFO XML 里抽第一个 `<tag>…</tag>` 的文本（文件是自己写的，简单解析够用）。
fn nfo_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = start + xml[start..].find(&close)?;
    let s = xml[start..end].trim();
    if s.is_empty() {
        None
    } else {
        // 写入时做了 XML 转义，读回来只还原最常见两个（&amp; &lt; &gt; &quot;）
        Some(
            s.replace("&amp;", "&")
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\""),
        )
    }
}

/// 抽所有 `<actor><name>…</name></actor>`。
fn nfo_actors(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(p) = rest.find("<actor>") {
        let seg = &rest[p + "<actor>".len()..];
        let Some(end) = seg.find("</actor>") else { break };
        if let Some(name) = nfo_text(&seg[..end], "name") {
            out.push(name);
        }
        rest = &seg[end + "</actor>".len()..];
    }
    out
}

/// 海报优先级：`<stem>-poster.jpg`（Jellyfin 命名约定）→ `<stem>.jpg` → `<stem>.png`。
///
/// ⚠️ 用 file_name 拼：`with_extension` 对无扩展名 stem 会拼出
/// `MIDV-567.poster.jpg` 这种没人写过的名字。
fn find_poster(strm_path: &Path) -> Option<PathBuf> {
    let parent = strm_path.parent()?;
    let stem = strm_path.file_stem()?.to_string_lossy().to_string();
    for name in [
        format!("{stem}-poster.jpg"),
        format!("{stem}.jpg"),
        format!("{stem}.png"),
    ] {
        let cand = parent.join(name);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

/// 番号来源优先级：NFO `uniqueid` → 文件名解析 → 目录名解析。
fn entry_number(nfo: Option<&str>, strm_path: &Path) -> String {
    if let Some(xml) = nfo {
        if let Some(n) = nfo_text(xml, "uniqueid") {
            if !n.trim().is_empty() {
                return n.trim().to_string();
            }
        }
    }
    let name = strm_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    if let Some(n) = parse_filename(&name).number {
        return n;
    }
    strm_path
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| name)
}

/// 扫一个 `.strm` 文件，产出 (番号, 文件, NFO文本)。
fn scan_one(strm_path: &Path) -> Option<(String, LibFile, Option<String>)> {
    let url = read_strm(strm_path);
    if url.is_empty() {
        return None;
    }
    let nfo_path = strm_path.with_extension("nfo");
    let nfo = std::fs::read_to_string(&nfo_path).ok();
    let name = strm_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let number = entry_number(nfo.as_deref(), strm_path);
    if number.trim().is_empty() {
        return None;
    }
    Some((
        number,
        LibFile {
            name,
            url,
            dest: strm_path.to_string_lossy().to_string(),
        },
        nfo,
    ))
}

/// 扫整个 strm_root，聚合番号 → 条目。
///
/// 目录不存在/为空 → 空列表（前端显示「库是空的」而不是报错）。
pub fn scan_library(strm_root: &Path) -> Vec<LibEntry> {
    let mut strm_files: Vec<PathBuf> = Vec::new();
    collect_strm(strm_root, 0, &mut strm_files);
    strm_files.sort();

    // 番号 -> (元数据只认第一份 NFO, files 追加)
    let mut order: Vec<String> = Vec::new();
    let mut map: std::collections::BTreeMap<String, LibEntry> = std::collections::BTreeMap::new();

    for path in strm_files {
        let Some((number, file, nfo)) = scan_one(&path) else {
            continue;
        };
        let e = map.entry(number.clone()).or_insert_with(|| {
            order.push(number.clone());
            LibEntry {
                number: number.clone(),
                title: number.clone(),
                year: String::new(),
                premiered: String::new(),
                actors: Vec::new(),
                tags: Vec::new(),
                studio: None,
                runtime_min: None,
                poster: find_poster(&path),
                files: Vec::new(),
            }
        });
        if e.poster.is_none() {
            e.poster = find_poster(&path);
        }
        if let Some(xml) = nfo {
            // 一个番号多个文件时 NFO 内容一样，只在首见时填充一次
            if e.files.is_empty() {
                if let Some(t) = nfo_text(&xml, "title") {
                    e.title = t;
                }
                e.year = nfo_text(&xml, "year").unwrap_or_default();
                e.premiered = nfo_text(&xml, "premiered").unwrap_or_default();
                e.studio = nfo_text(&xml, "studio");
                e.runtime_min = nfo_text(&xml, "runtime").and_then(|s| s.parse().ok());
                let actors = nfo_actors(&xml);
                if !actors.is_empty() {
                    e.actors = actors;
                }
                e.tags = xml
                    .match_indices("<genre>")
                    .filter_map(|(i, _)| {
                        let seg = &xml[i..];
                        // 切片必须包含闭合标签，nfo_text 才找得到配对
                        let end = seg.find("</genre>")? + "</genre>".len();
                        nfo_text(&seg[..end], "genre")
                    })
                    .collect();
            }
        }
        e.files.push(file);
    }

    // 排序：有发行日期的按日期新→旧，没日期的按番号
    let mut out: Vec<LibEntry> = order
        .into_iter()
        .filter_map(|k| map.remove(&k))
        .collect();
    out.sort_by(|a, b| {
        b.premiered
            .cmp(&a.premiered)
            .then_with(|| b.number.cmp(&a.number))
    });
    out
}

/// 条目过滤：关键词命中番号/标题/演员（大小写不敏感），标签精确命中。
pub fn filter_entries(items: Vec<LibEntry>, query: &str, tag: &str) -> Vec<LibEntry> {
    let q = query.trim().to_lowercase();
    let tag = tag.trim();
    items
        .into_iter()
        .filter(|e| {
            if !tag.is_empty() && !e.tags.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
                return false;
            }
            if q.is_empty() {
                return true;
            }
            e.number.to_lowercase().contains(&q)
                || e.title.to_lowercase().contains(&q)
                || e.actors.iter().any(|a| a.to_lowercase().contains(&q))
        })
        .collect()
}

fn collect_strm(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    if depth > 12 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_strm(&p, depth + 1, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("strm") {
            out.push(p);
        }
    }
}

/// 「移出库」：删掉该番号的所有 `.strm` 及同 stem 的 NFO/海报，并从增量
/// 清单里移除对应条目（否则下一轮会因清单还在而跳过重建，或者反过来
/// 残留条目指向已删文件）。返回删除的 `.strm` 数量。
///
/// `重新匹配` 也走这里：采用人工精选后先移出，下一轮按新元数据重建。
pub fn remove_entry(strm_root: &Path, manifest_path: &Path, number: &str) -> anyhow::Result<u32> {
    let want = number.trim().to_ascii_lowercase();
    let lib = scan_library(strm_root);
    let Some(entry) = lib.iter().find(|e| e.number.eq_ignore_ascii_case(&want)) else {
        anyhow::bail!("媒体库里没有 {number}");
    };

    let mut removed = 0u32;
    let mut manifest = crate::strm::Manifest::load(manifest_path);
    for f in &entry.files {
        let dest = PathBuf::from(&f.dest);
        // 同 stem 的附属文件一起删（NFO / 海报 / 降级生成时的封面拷贝）
        let stem_exts = ["nfo", "jpg", "png"];
        for ext in stem_exts {
            let p = dest.with_extension(ext);
            if p.exists() {
                let _ = std::fs::remove_file(&p);
            }
        }
        // `<stem>-poster.jpg` 变体（with_extension 只会替换最后一个扩展名）
        if let Some(stem) = dest.file_stem().and_then(|s| s.to_str()) {
            let poster = dest.with_file_name(format!("{stem}-poster.jpg"));
            if poster.exists() {
                let _ = std::fs::remove_file(&poster);
            }
        }
        if dest.exists() {
            let _ = std::fs::remove_file(&dest);
            removed += 1;
        }
        // 清单按网盘路径（src）为键：值里 out 指向我们刚删的文件
        let victims: Vec<String> = manifest
            .entries
            .iter()
            .filter(|(_, e)| e.out == f.dest)
            .map(|(k, _)| k.clone())
            .collect();
        for k in victims {
            manifest.entries.remove(&k);
        }
    }
    manifest.save(manifest_path)?;
    if removed == 0 {
        anyhow::bail!("没有可移出的文件（{number}）");
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mdc_lib_test_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn nfo_xml(number: &str, title: &str, year: &str, actors: &[&str]) -> String {
        let mut s = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<movie>\n  <title>{title}</title>\n  <year>{year}</year>\n  <premiered>{year}-07-02</premiered>\n  <runtime>120</runtime>\n  <uniqueid type=\"number\" default=\"true\">{number}</uniqueid>\n"
        );
        for a in actors {
            s.push_str(&format!("  <actor>\n    <name>{a}</name>\n  </actor>\n"));
        }
        s.push_str("  <genre>TagA</genre>\n  <genre>TagB</genre>\n</movie>\n");
        s
    }

    #[test]
    fn groups_by_number_and_reads_nfo() {
        let d = tmpdir("groups");
        let dir = d.join("MIDV-567");
        std::fs::create_dir_all(&dir).unwrap();
        crate::strm::write_strm(&dir.join("MIDV-567.strm"), "http://cd2/a", true).unwrap();
        std::fs::write(dir.join("MIDV-567.nfo"), nfo_xml("MIDV-567", "标题&amp;特", "2024", &["演员甲"])).unwrap();
        std::fs::write(dir.join("MIDV-567-poster.jpg"), b"jpg").unwrap();

        let lib = scan_library(&d);
        assert_eq!(lib.len(), 1);
        let e = &lib[0];
        assert_eq!(e.number, "MIDV-567");
        assert_eq!(e.title, "标题&特", "XML 实体要还原");
        assert_eq!(e.year, "2024");
        assert_eq!(e.premiered, "2024-07-02");
        assert_eq!(e.actors, vec!["演员甲"]);
        assert_eq!(e.tags, vec!["TagA", "TagB"]);
        assert_eq!(e.runtime_min, Some(120));
        assert_eq!(e.files.len(), 1);
        assert_eq!(e.files[0].url, "http://cd2/a");
        assert!(e.poster.is_some(), "-poster.jpg 优先命中");
    }

    #[test]
    fn multi_part_same_number_is_one_entry() {
        let d = tmpdir("multip");
        let dir = d.join("ABP-123");
        std::fs::create_dir_all(&dir).unwrap();
        crate::strm::write_strm(&dir.join("ABP-123-cd1.strm"), "http://cd2/1", false).unwrap();
        crate::strm::write_strm(&dir.join("ABP-123-cd2.strm"), "http://cd2/2", false).unwrap();
        std::fs::write(dir.join("ABP-123-cd1.nfo"), nfo_xml("ABP-123", "T", "2020", &[])).unwrap();
        std::fs::write(dir.join("ABP-123-cd2.nfo"), nfo_xml("ABP-123", "T", "2020", &[])).unwrap();

        let lib = scan_library(&d);
        assert_eq!(lib.len(), 1, "同番号必须聚合成一条");
        assert_eq!(lib[0].files.len(), 2);
        assert_eq!(lib[0].files[0].name, "ABP-123-cd1");
    }

    #[test]
    fn missing_nfo_falls_back_to_filename() {
        let d = tmpdir("nonfo");
        std::fs::create_dir_all(&d).unwrap();
        crate::strm::write_strm(&d.join("SSIS-984 [1080p].strm"), "http://cd2/x", false).unwrap();
        std::fs::write(d.join("SSIS-984 [1080p].jpg"), b"jpg").unwrap();

        let lib = scan_library(&d);
        assert_eq!(lib.len(), 1);
        assert_eq!(lib[0].number, "SSIS-984", "NFO 缺失时从文件名解析番号");
        assert_eq!(lib[0].title, "SSIS-984", "没标题就先用番号");
        assert!(lib[0].poster.is_some());
    }

    #[test]
    fn empty_or_unreadable_url_is_skipped() {
        let d = tmpdir("empty");
        std::fs::create_dir_all(&d).unwrap();
        crate::strm::write_strm(&d.join("A-1.strm"), "   ", false).unwrap();
        assert!(scan_library(&d).is_empty(), "空 URL 的 strm 不进库");
    }

    #[test]
    fn filter_matches_number_title_actor() {
        let mk = |number: &str, title: &str, actors: &[&str]| LibEntry {
            number: number.into(),
            title: title.into(),
            year: String::new(),
            premiered: String::new(),
            actors: actors.iter().map(|s| s.to_string()).collect(),
            tags: vec!["TagA".into()],
            studio: None,
            runtime_min: None,
            poster: None,
            files: Vec::new(),
        };
        let items = vec![
            mk("MIDV-567", "某片", &["佐藤"]),
            mk("FC2-PPV-1", "Another", &["田中"]),
        ];
        assert_eq!(filter_entries(items.clone(), "midv", "").len(), 1);
        assert_eq!(filter_entries(items.clone(), "another", "").len(), 1, "标题命中");
        assert_eq!(filter_entries(items.clone(), "田中", "").len(), 1, "演员命中");
        assert_eq!(filter_entries(items.clone(), "", "TagA").len(), 2);
        assert_eq!(filter_entries(items, "", "TagX").len(), 0);
    }

    #[test]
    fn sort_is_premiered_desc() {
        let d = tmpdir("sort");
        for (n, y) in [("AAA-1", "2020"), ("BBB-2", "2024")] {
            let dir = d.join(n);
            std::fs::create_dir_all(&dir).unwrap();
            crate::strm::write_strm(&dir.join(format!("{n}.strm")), "http://cd2/x", false).unwrap();
            std::fs::write(dir.join(format!("{n}.nfo")), nfo_xml(n, n, y, &[])).unwrap();
        }
        let lib = scan_library(&d);
        assert_eq!(lib[0].number, "BBB-2", "新片在前");
    }
}
