//! NFO 生成（Kodi/Emby/Jellyfin 兼容的 movie XML）。

use crate::model::VideoMeta;
use anyhow::Result;
use chrono::NaiveDate;

const NFO_TEMPLATE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<movie>
  <title>{{ title }}</title>
  <originaltitle>{{ original_title }}</originaltitle>
  <plot>{{ plot }}</plot>
  <outline>{{ plot }}</outline>
  <studio>{{ studio }}</studio>
  <director>{{ director }}</director>
  <set>{{ series }}</set>
  <premiered>{{ premiered }}</premiered>
  <year>{{ year }}</year>
  <runtime>{{ runtime }}</runtime>
  <mpaa>XXX</mpaa>
  <uniqueid type="number" default="true">{{ number }}</uniqueid>
  {% for actor in actors %}
  <actor>
    <name>{{ actor }}</name>
  </actor>
  {% endfor %}
  {% for tag in tags %}
  <genre>{{ tag }}</genre>
  {% endfor %}
</movie>
"#;

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 渲染 NFO XML 文本。
pub fn render_nfo(meta: &VideoMeta) -> Result<String> {
    let title = meta
        .title
        .clone()
        .unwrap_or_else(|| meta.number.clone());
    let premiered = meta
        .release_date
        .map(|d: NaiveDate| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default();
    let year = meta
        .release_date
        .map(|d| d.format("%Y").to_string())
        .unwrap_or_default();

    let mut env = minijinja::Environment::new();
    env.add_template("nfo", NFO_TEMPLATE)?;
    let ctx = minijinja::context! {
        title => xml_escape(&title),
        original_title => xml_escape(meta.original_title.as_deref().unwrap_or("")),
        plot => xml_escape(meta.title.as_deref().unwrap_or("")),
        studio => xml_escape(meta.studio.as_deref().unwrap_or("")),
        director => xml_escape(meta.director.as_deref().unwrap_or("")),
        series => xml_escape(meta.series.as_deref().unwrap_or("")),
        premiered,
        year,
        runtime => meta.runtime_min.unwrap_or(0).to_string(),
        number => xml_escape(&meta.number),
        actors => meta.actors.iter().map(|a| xml_escape(a)).collect::<Vec<_>>(),
        tags => meta.tags.iter().map(|t| xml_escape(t)).collect::<Vec<_>>(),
    };
    Ok(env.get_template("nfo")?.render(ctx)?)
}

/// 把 NFO 写到视频文件旁边（同名 .nfo）。
pub fn write_nfo_alongside(meta: &VideoMeta, video_path: &std::path::Path) -> Result<std::path::PathBuf> {
    let nfo_path = video_path.with_extension("nfo");
    std::fs::write(&nfo_path, render_nfo(meta)?)?;
    Ok(nfo_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_xml() {
        let meta = VideoMeta {
            number: "ABP-123".into(),
            title: Some("Test & <Movie>".into()),
            actors: vec!["Actor A".into()],
            release_date: NaiveDate::from_ymd_opt(2024, 7, 2),
            ..Default::default()
        };
        let xml = render_nfo(&meta).unwrap();
        assert!(xml.contains("<title>Test &amp; &lt;Movie&gt;</title>"));
        assert!(xml.contains("<year>2024</year>"));
        assert!(xml.contains("<name>Actor A</name>"));
    }
}
