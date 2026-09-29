use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// 一部影片的完整元数据（刮削结果的统一模型）。
///
/// `#[serde(default)]`：**反序列化时字段可缺省**。
/// 少了它，`PUT /api/videos/{number}/meta` 这种接口就要求客户端回传全部字段
/// （`actors`/`tags`/`preview_urls` 一个都不能少），只传 `{number,title}` 会 422 ——
/// 实测踩过。序列化行为不受影响。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoMeta {
    /// 番号，唯一键，如 ABP-123 / FC2-PPV-3141592
    pub number: String,
    pub title: Option<String>,
    pub original_title: Option<String>,
    pub actors: Vec<String>,
    pub release_date: Option<NaiveDate>,
    pub studio: Option<String>,
    pub series: Option<String>,
    pub director: Option<String>,
    pub runtime_min: Option<u32>,
    pub tags: Vec<String>,
    pub cover_url: Option<String>,
    pub poster_url: Option<String>,
    pub preview_urls: Vec<String>,
    /// 详情页地址（多来源人工核对时用）
    pub website: Option<String>,
    /// 命中来源的 provider id
    pub source: Option<String>,
    pub uncensored: Option<bool>,
}

impl VideoMeta {
    pub fn merge(&mut self, other: VideoMeta) {
        if self.title.is_none() {
            self.title = other.title;
        }
        if self.original_title.is_none() {
            self.original_title = other.original_title;
        }
        for a in other.actors {
            if !self.actors.contains(&a) {
                self.actors.push(a);
            }
        }
        if self.release_date.is_none() {
            self.release_date = other.release_date;
        }
        if self.studio.is_none() {
            self.studio = other.studio;
        }
        if self.series.is_none() {
            self.series = other.series;
        }
        if self.director.is_none() {
            self.director = other.director;
        }
        if self.runtime_min.is_none() {
            self.runtime_min = other.runtime_min;
        }
        for t in other.tags {
            if !self.tags.contains(&t) {
                self.tags.push(t);
            }
        }
        if self.cover_url.is_none() {
            self.cover_url = other.cover_url;
        }
        if self.poster_url.is_none() {
            self.poster_url = other.poster_url;
        }
        for u in other.preview_urls {
            if !self.preview_urls.contains(&u) {
                self.preview_urls.push(u);
            }
        }
        if self.website.is_none() {
            self.website = other.website;
        }
        if self.uncensored.is_none() {
            self.uncensored = other.uncensored;
        }
        if self.source.is_none() {
            self.source = other.source;
        }
    }
}

/// 文件整理方式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrganizeMode {
    /// 硬链接（推荐本地场景，省空间）
    HardLink,
    Copy,
    Move,
    Symlink,
    /// 原地整理：只生成 NFO/图片元数据，不动视频文件
    InPlace,
    /// 网盘（CD2 挂载）专用：视频一个字节都不搬，写 `.strm` 指针 + NFO + 海报到本地目录。
    ///
    /// 对网盘，其余四种模式都不可取：跨盘 hardlink 必失败，copy/move 等于
    /// 真下载再上传 —— 慢，而且必然触发网盘风控。
    Strm,
}

impl OrganizeMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            OrganizeMode::HardLink => "hard_link",
            OrganizeMode::Copy => "copy",
            OrganizeMode::Move => "move",
            OrganizeMode::Symlink => "symlink",
            OrganizeMode::InPlace => "in_place",
            OrganizeMode::Strm => "strm",
        }
    }
}

/// 任务状态机：pending → running → done / failed
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Running,
    Done,
    Failed,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::Running => "running",
            TaskStatus::Done => "done",
            TaskStatus::Failed => "failed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 🔴 缺省字段要能反序列化。
    ///
    /// 少了 `#[serde(default)]`，`PUT /api/videos/{number}/meta` 会要求客户端回传
    /// **全部**字段（`actors`/`tags`/`preview_urls` 一个都不能少），
    /// 只传 `{number,title}` 直接 422 —— 实测踩过。
    #[test]
    fn partial_json_deserializes() {
        let m: VideoMeta =
            serde_json::from_str(r#"{"number":"MIDV-567","title":"标题"}"#).unwrap();
        assert_eq!(m.number, "MIDV-567");
        assert_eq!(m.title.as_deref(), Some("标题"));
        assert!(m.actors.is_empty());
        assert!(m.tags.is_empty());
        assert!(m.preview_urls.is_empty());
        assert!(m.release_date.is_none());
    }

    /// 空对象也要能反序列化（对应「只改一个字段」这类最小请求）。
    #[test]
    fn empty_json_deserializes() {
        let m: VideoMeta = serde_json::from_str("{}").unwrap();
        assert!(m.number.is_empty());
    }

    /// 序列化行为不受 `#[serde(default)]` 影响：字段该出还是出。
    #[test]
    fn serialization_still_emits_all_fields() {
        let m = VideoMeta { number: "MIDV-567".into(), ..Default::default() };
        let s = serde_json::to_string(&m).unwrap();
        for k in ["number", "actors", "tags", "preview_urls", "cover_url"] {
            assert!(s.contains(k), "序列化结果缺少字段 {k}：{s}");
        }
    }
}
