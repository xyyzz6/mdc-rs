//! 刮削引擎：Provider trait + 注册表 + 自定义 YAML 源。
//!
//! 借鉴 mdc-ng 的两层设计：
//! - 内置源（Rust 实现，如 javbus）；
//! - 自定义源（`<数据目录>/providers/*.yaml`，CSS 选择器声明式配置），
//!   让用户不写代码就能适配新站点，也规避源站小改版要等更新的问题。

pub mod custom;
pub mod fc2;
pub mod javbus;

use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::config::AppConfig;
use crate::model::VideoMeta;

/// 刮削上下文：共享的 HTTP 客户端（带代理/超时/UA）。
#[derive(Clone)]
pub struct ScrapeCtx {
    pub http: Client,
    pub flaresolverr: Option<String>,
}

impl ScrapeCtx {
    /// `proxy`：**调用方给的有效代理**（server 传内置内核的 effective_proxy
    /// —— 内核 Running 时是 `http://127.0.0.1:17890`）。不传才回落
    /// `cfg.common.proxy`（手填的外部代理）。🔴 只认 common.proxy 而不看
    /// 内核的话，内核跑起来了刮削还在直连 —— 真机截图实锤「全部 failed」。
    pub fn from_config(cfg: &AppConfig, proxy: Option<&str>) -> Result<Self> {
        let p = proxy
            .map(|s| s.to_string())
            .or_else(|| cfg.common.proxy.clone());
        // 出网统一走 net.rs：代理与「内网直连」豁免必须同一处决定
        // （代理不认识内网地址会直接回 404，douyin-nas 踩过）。
        let http = crate::net::client(crate::net::HttpOpts {
            proxy: p.as_deref(),
            no_proxy: &cfg.common.no_proxy,
            timeout_secs: cfg.common.timeout_secs,
        })?;
        Ok(Self {
            http,
            flaresolverr: cfg.scrape.flaresolverr.clone(),
        })
    }
}

#[async_trait]
pub trait Provider: Send + Sync {
    /// 稳定 id，用于配置优先级（如 "javbus"、"custom:mysite"）
    fn id(&self) -> &str;
    /// 是否需要登录/付费，UI 展示用
    fn label(&self) -> &str;
    /// 这个源能不能处理该番号。
    ///
    /// 默认 `true`。源实现里按番号形态收窄（如 FC2 源只认 `FC2-PPV-*`）——
    /// 引擎会跳过不支持的源，省掉一次**注定失败的网络请求**和一条误导性的错误日志。
    fn supports(&self, _number: &str) -> bool {
        true
    }
    async fn search(&self, ctx: &ScrapeCtx, number: &str) -> Result<Vec<VideoMeta>>;
}

/// 一个源对某个番号的返回状态（给「多源人工精选」用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateStatus {
    /// 命中
    Hit,
    /// 该源不处理这种番号（`supports()` 为 false）—— **没发请求**
    Skipped,
    /// 发过请求，但没命中或出错
    Failed,
}

/// 单个源的结果。和 `VideoMeta::merge` 出来的合并结果不同：
/// 这是**原样的、没合并的**，UI 才能让用户逐个比较后自己挑。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub provider: String,
    pub label: String,
    pub status: CandidateStatus,
    pub error: Option<String>,
    pub meta: Option<VideoMeta>,
}

/// 引擎：按配置的优先级依次调用各源并合并结果。
pub struct Engine {
    pub providers: Vec<Box<dyn Provider>>,
}

impl Engine {
    /// 内置源 + 自定义 YAML 源（加载失败的单个源跳过并记日志）。
    pub fn load(cfg: &AppConfig) -> Self {
        let mut providers: Vec<Box<dyn Provider>> = vec![
            // 有码主力站
            Box::new(javbus::JavBus::new()),
            // 补 javbus 不索引的 FC2（官方站，权威且无需登录）
            Box::new(fc2::Fc2::new()),
        ];
        match custom::load_custom_providers() {
            Ok(list) => {
                for p in list {
                    providers.push(Box::new(p));
                }
            }
            Err(e) => tracing::warn!(error = %e, "加载自定义源失败（忽略）"),
        }
        // 按配置排序：priorities 里靠前的源排前面
        let prio = |id: &str| -> usize {
            cfg.scrape
                .priorities
                .iter()
                .position(|p| p == id)
                .unwrap_or(usize::MAX)
        };
        providers.sort_by_key(|p| prio(p.id()));
        // 停用清单最后过滤（排序前过滤也行，这里只是让 id 解析失败更早暴露）
        if !cfg.scrape.disabled.is_empty() {
            let before = providers.len();
            providers.retain(|p| !cfg.scrape.disabled.iter().any(|d| d == p.id()));
            tracing::info!(
                disabled = ?cfg.scrape.disabled,
                removed = before - providers.len(),
                "按配置停用刮削源"
            );
        }
        Self { providers }
    }

    /// 按优先级逐个尝试，把每个源命中的第一条结果合并进总元数据。
    ///
    /// 不支持该番号的源（`supports()` 为 false）直接跳过，不发请求。
    /// **每个源各自的原始结果**（不合并），给「多源人工精选」用。
    ///
    /// 这是引擎的底层原语 —— `scrape()` 就是「把里面的 Hit 按顺序合并」。
    /// 不支持该番号的源（`supports()` 为 false）会被标成 `Skipped` 且**不发请求**。
    pub async fn candidates(&self, ctx: &ScrapeCtx, number: &str) -> Vec<Candidate> {
        let mut out = Vec::with_capacity(self.providers.len());
        for p in &self.providers {
            let base = Candidate {
                provider: p.id().to_string(),
                label: p.label().to_string(),
                status: CandidateStatus::Skipped,
                error: None,
                meta: None,
            };
            if !p.supports(number) {
                out.push(base);
                continue;
            }
            match p.search(ctx, number).await {
                Ok(list) => match list.into_iter().next() {
                    Some(meta) => out.push(Candidate {
                        status: CandidateStatus::Hit,
                        meta: Some(meta),
                        ..base
                    }),
                    None => out.push(Candidate {
                        status: CandidateStatus::Failed,
                        error: Some("源没返回结果".into()),
                        ..base
                    }),
                },
                Err(e) => out.push(Candidate {
                    status: CandidateStatus::Failed,
                    error: Some(e.to_string()),
                    ..base
                }),
            }
        }
        out
    }

    /// 按优先级逐个尝试，把每个源命中的第一条结果合并进总元数据。
    ///
    /// 就是「`candidates()` 里的 Hit 按顺序合并」—— 合并规则见 `VideoMeta::merge`。
    pub async fn scrape(&self, ctx: &ScrapeCtx, number: &str) -> Vec<VideoMeta> {
        let mut merged: Option<VideoMeta> = None;
        for c in self.candidates(ctx, number).await {
            match c.status {
                CandidateStatus::Hit => {
                    if let Some(meta) = c.meta {
                        tracing::info!(provider = %c.provider, number, "命中");
                        match &mut merged {
                            Some(m) => m.merge(meta),
                            None => merged = Some(meta),
                        }
                    }
                }
                CandidateStatus::Failed => {
                    tracing::warn!(
                        provider = %c.provider,
                        number,
                        error = c.error.as_deref().unwrap_or(""),
                        "未命中或出错"
                    );
                }
                CandidateStatus::Skipped => {
                    tracing::debug!(provider = %c.provider, number, "该源不处理这种番号，跳过");
                }
            }
        }
        merged.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::VideoMeta;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// 假源：返回预置元数据，并记录被调用了几次。
    struct Fake {
        id: &'static str,
        /// 只处理含这些子串的番号；空 = 全都处理
        handles: Vec<&'static str>,
        meta: VideoMeta,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Provider for Fake {
        fn id(&self) -> &str {
            self.id
        }
        fn label(&self) -> &str {
            self.id
        }
        fn supports(&self, number: &str) -> bool {
            self.handles.is_empty() || self.handles.iter().any(|h| number.contains(h))
        }
        async fn search(&self, _ctx: &ScrapeCtx, _number: &str) -> Result<Vec<VideoMeta>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![self.meta.clone()])
        }
    }

    fn test_ctx() -> ScrapeCtx {
        ScrapeCtx {
            http: reqwest::Client::new(),
            flaresolverr: None,
        }
    }

    fn fake(
        id: &'static str,
        handles: Vec<&'static str>,
        meta: VideoMeta,
    ) -> (Box<dyn Provider>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Box::new(Fake { id, handles, meta, calls: calls.clone() }),
            calls,
        )
    }

    fn empty_meta(number: &str) -> VideoMeta {
        VideoMeta { number: number.into(), ..Default::default() }
    }

    /// `supports()` 为 false 的源**不该被调用** —— 否则就是白跑一次注定失败的请求，
    /// 还附赠一条误导性的错误日志。
    #[tokio::test]
    async fn unsupported_provider_is_not_called() {
        let (fc2_src, fc2_calls) =
            fake("fc2", vec!["FC2"], empty_meta("FC2-PPV-4680562"));
        let (bus_src, bus_calls) = fake("javbus", vec!["MIDV"], empty_meta("MIDV-567"));
        let engine = Engine { providers: vec![fc2_src, bus_src] };

        let out = engine.scrape(&test_ctx(), "MIDV-567").await;
        assert_eq!(out.len(), 1);
        assert_eq!(bus_calls.load(Ordering::SeqCst), 1, "支持的源应被调用");
        assert_eq!(fc2_calls.load(Ordering::SeqCst), 0, "不支持的源不该被调用");

        // 反向也要成立
        let (fc2_src, fc2_calls) =
            fake("fc2", vec!["FC2"], empty_meta("FC2-PPV-4680562"));
        let (bus_src, bus_calls) = fake("javbus", vec!["MIDV"], empty_meta("MIDV-567"));
        let engine = Engine { providers: vec![fc2_src, bus_src] };
        let out = engine.scrape(&test_ctx(), "FC2-PPV-4680562").await;
        assert_eq!(out[0].number, "FC2-PPV-4680562");
        assert_eq!(fc2_calls.load(Ordering::SeqCst), 1);
        assert_eq!(bus_calls.load(Ordering::SeqCst), 0);
    }

    /// 🔴 多源合并：**首个非空字段优先**（先命中的源不被后命中的覆盖），
    /// 而演员/标签这类列表取**并集且去重**。
    ///
    /// 这条路径以前从没被跑过 —— 只有一个源时 `merge` 永远不会被调用。
    #[tokio::test]
    async fn multi_source_merge_fills_gaps_and_unions_lists() {
        let a = VideoMeta {
            number: "MIDV-567".into(),
            title: Some("A 的标题".into()),
            actors: vec!["演员甲".into()],
            tags: vec!["标签1".into()],
            ..Default::default()
        };
        let b = VideoMeta {
            number: "MIDV-567".into(),
            title: Some("B 的标题".into()),
            cover_url: Some("https://x/c.jpg".into()),
            release_date: chrono::NaiveDate::from_ymd_opt(2024, 1, 2),
            actors: vec!["演员乙".into(), "演员甲".into()],
            tags: vec!["标签2".into()],
            ..Default::default()
        };
        let (sa, _) = fake("a", vec![], a);
        let (sb, _) = fake("b", vec![], b);
        let engine = Engine { providers: vec![sa, sb] };

        let out = engine.scrape(&test_ctx(), "MIDV-567").await;
        assert_eq!(out.len(), 1);
        let m = &out[0];
        assert_eq!(
            m.title.as_deref(),
            Some("A 的标题"),
            "首个非空字段优先，B 不该覆盖"
        );
        assert_eq!(
            m.cover_url.as_deref(),
            Some("https://x/c.jpg"),
            "A 缺的字段应由 B 补上"
        );
        assert!(m.release_date.is_some(), "A 缺的字段应由 B 补上");
        assert_eq!(m.actors, vec!["演员甲", "演员乙"], "演员取并集且去重");
        assert_eq!(m.tags, vec!["标签1", "标签2"], "标签取并集且去重");
    }

    /// `candidates()` 要把「每个源各自的结果」原样给出：命中 / 跳过 / 未命中三态分明，
    /// 而且跳过的源**不发请求**。这是「多源人工精选」UI 的数据来源。
    #[tokio::test]
    async fn candidates_reports_each_provider_separately() {
        struct Boom;
        #[async_trait]
        impl Provider for Boom {
            fn id(&self) -> &str {
                "boom"
            }
            fn label(&self) -> &str {
                "会炸的源"
            }
            async fn search(&self, _c: &ScrapeCtx, _n: &str) -> Result<Vec<VideoMeta>> {
                anyhow::bail!("源挂了")
            }
        }
        let (hit_src, hit_calls) = fake("good", vec![], empty_meta("MIDV-567"));
        let (skip_src, skip_calls) = fake("fc2only", vec!["FC2"], empty_meta("FC2-PPV-1"));
        let engine = Engine { providers: vec![hit_src, skip_src, Box::new(Boom)] };

        let cands = engine.candidates(&test_ctx(), "MIDV-567").await;
        assert_eq!(cands.len(), 3, "每个源都要有一条，UI 才能逐个比较");
        assert_eq!(cands[0].provider, "good");
        assert_eq!(cands[0].status, CandidateStatus::Hit);
        assert_eq!(cands[0].meta.as_ref().map(|m| m.number.as_str()), Some("MIDV-567"));
        assert_eq!(cands[1].status, CandidateStatus::Skipped);
        assert!(cands[1].meta.is_none());
        assert_eq!(cands[2].status, CandidateStatus::Failed);
        assert!(
            cands[2].error.as_deref().unwrap_or("").contains("源挂了"),
            "失败原因要带给 UI，实际：{:?}",
            cands[2].error
        );
        assert_eq!(hit_calls.load(Ordering::SeqCst), 1);
        assert_eq!(skip_calls.load(Ordering::SeqCst), 0, "跳过的源不该发请求");
    }

    /// 全部源都失败时返回空（而不是 panic / 半成品）。
    #[tokio::test]
    async fn all_sources_failing_yields_empty() {
        struct Boom;
        #[async_trait]
        impl Provider for Boom {
            fn id(&self) -> &str {
                "boom"
            }
            fn label(&self) -> &str {
                "boom"
            }
            async fn search(&self, _c: &ScrapeCtx, _n: &str) -> Result<Vec<VideoMeta>> {
                anyhow::bail!("炸了")
            }
        }
        let engine = Engine { providers: vec![Box::new(Boom)] };
        assert!(engine.scrape(&test_ctx(), "MIDV-567").await.is_empty());
    }

    /// 内置源必须都注册上，而且各自只认自己的番号形态。
    #[test]
    fn builtin_providers_are_registered_and_scoped() {
        let engine = Engine::load(&AppConfig::default());
        let ids: Vec<&str> = engine.providers.iter().map(|p| p.id()).collect();
        assert!(ids.contains(&"javbus"), "实际：{ids:?}");
        assert!(ids.contains(&"fc2"), "实际：{ids:?}");

        let fc2 = engine.providers.iter().find(|p| p.id() == "fc2").unwrap();
        assert!(fc2.supports("FC2-PPV-4680562"));
        assert!(!fc2.supports("MIDV-567"));

        // javbus 明确声明不处理 FC2（让引擎跳过，别每次 FC2 都报一条误导性 WARN）
        let bus = engine.providers.iter().find(|p| p.id() == "javbus").unwrap();
        assert!(bus.supports("MIDV-567"));
        assert!(
            !bus.supports("FC2-PPV-4680562"),
            "javbus 不索引 FC2，应声明不支持"
        );

        // 两个源对番号形态的分工：FC2 只归 FC2 源，其余只归 javbus
        assert_eq!(
            engine
                .providers
                .iter()
                .filter(|p| p.supports("FC2-PPV-4680562"))
                .map(|p| p.id())
                .collect::<Vec<_>>(),
            vec!["fc2"]
        );
        assert_eq!(
            engine
                .providers
                .iter()
                .filter(|p| p.supports("MIDV-567"))
                .map(|p| p.id())
                .collect::<Vec<_>>(),
            vec!["javbus"]
        );
    }
}
