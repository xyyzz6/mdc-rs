//! mdc-server：单进程 HTTP 服务（API + 静态前端同端口）。
//!
//! 与 mdc-ng 的双进程架构不同，这里前后端同端口，部署更简单；
//! 鉴权用 per-install 随机 JWT 密钥（避免 mdc-ng 的硬编码密钥漏洞），
//! 且不存在免鉴权的文件读取接口。
//!
//! 本 crate 同时提供库入口（`build_state`/`build_router`/`serve`），
//! 供安卓壳（shells/mdc-mobile）在进程内直接启动引擎——桌面/Docker
//! 走 `main()`（可执行文件），APK 走库调用，同一套路由与状态。

use anyhow::Result;
use axum::{
    extract::{Path, Query, Request, State},
    http::{header, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use mdc_core::{
    config::{AppConfig, ProxyConfig},
    db,
    model::OrganizeMode,
    pipeline, proxy, scrape,
    VERSION,
};

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<RwLock<AppConfig>>,
    pub pool: sqlx::SqlitePool,
    pub engine: Arc<scrape::Engine>,
    pub jwt_key: Arc<String>,
    /// 网盘刮削的运行态（单飞令牌 + last_run）。定时器与手动触发共用它。
    pub strm_runner: Arc<mdc_core::strm::StrmRunner>,
    /// 内置代理内核。里面持有子进程（`Child` 非 Sync）⇒ 必须 Mutex；
    /// 启动要花几秒到十几秒，**不要在请求里同步等**，一律 spawn。
    pub proxy: Arc<tokio::sync::Mutex<proxy::ProxyManager>>,
}

/// 构建服务运行态：加载配置、初始化数据库与引擎。
///
/// 环境变量与桌面 `main()` 完全一致（`MDC_CONFIG_PATH` 定数据目录，
/// `MDC_USERNAME`/`MDC_PASSWORD` 临时开鉴权），安卓壳在启动前设好前者即可。
pub async fn build_state() -> Result<AppState> {
    let mut cfg = AppConfig::load()?;
    // docker-compose 里一直声明着 MDC_USERNAME/MDC_PASSWORD，但代码从没读过 ——
    // 用户填了 admin/admin 就以为开了鉴权，其实完全没开（安全相关，必须接上）。
    // 只在内存里覆盖，不落盘：密钥不该被写进 config.toml。
    if let (Ok(u), Ok(p)) = (std::env::var("MDC_USERNAME"), std::env::var("MDC_PASSWORD")) {
        if !u.trim().is_empty() && !p.is_empty() {
            tracing::info!(user = %u, "启用环境变量提供的登录鉴权");
            cfg.auth.username = Some(u);
            cfg.auth.password = Some(p);
        }
    }
    mdc_core::scrape::custom::write_example_provider()?;

    let pool = db::init_pool(&AppConfig::data_dir().join("mdc.db")).await?;
    let engine = Arc::new(scrape::Engine::load(&cfg));
    let jwt_key = Arc::new(AppConfig::jwt_secret()?);
    // last_run 单独落一个文件，**不进 config.toml** —— 否则用户每改一次配置
    // 都会连带把时间戳改掉，定时节奏被配置操作带偏。
    let strm_runner = Arc::new(mdc_core::strm::StrmRunner::new(
        AppConfig::data_dir().join("strm_last_run"),
    ));

    // cfg 会被 move 进 Arc，先把代理配置抄一份出来
    let proxy_cfg = cfg.proxy.clone();
    Ok(AppState {
        cfg: Arc::new(RwLock::new(cfg)),
        pool,
        engine,
        jwt_key,
        strm_runner,
        proxy: Arc::new(tokio::sync::Mutex::new(proxy::ProxyManager::new(
            AppConfig::data_dir().join("proxy"),
            proxy_cfg,
        ))),
    })
}

/// WebView 内嵌前端（APK）访问本机 9208 是**跨源**（tauri:// 或
/// http://tauri.localhost → http://127.0.0.1:9208），必须放 CORS；
/// 桌面同源请求不受影响。只放行本机/壳来源，别开成任意站点都能打。
fn cors_layer() -> CorsLayer {
    let allow = |origin: &HeaderValue, _: &_| {
        let o = origin.to_str().unwrap_or_default();
        [
            "tauri://localhost",
            "http://tauri.localhost",
            "https://tauri.localhost",
            "http://localhost",
            "https://localhost",
            "http://127.0.0.1",
        ]
        .iter()
        .any(|a| o == *a || o.strip_prefix(a).is_some_and(|r| r.starts_with(':')))
    };
    CorsLayer::new()
        .allow_origin(tower_http::cors::AllowOrigin::predicate(allow))
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::DELETE,
        ])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
}

/// 组装完整路由（API + 静态前端回退）。桌面与安卓共用同一份。
pub fn build_router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/api/config", get(get_config).put(put_config))
        .route("/api/parse", post(parse_file))
        .route("/api/scrape/test", post(scrape_test))
        .route("/api/tasks", get(list_tasks).post(create_tasks))
        .route("/api/tasks/run", post(run_tasks))
        .route("/api/tasks/{id}", axum::routing::delete(delete_task))
        // 网盘 / .strm：全部在鉴权保护区内（读目录、写文件都不许裸奔）
        .route("/api/netdisk", get(netdisk_state).put(put_netdisk))
        .route("/api/source/probe", post(source_probe))
        .route("/api/strm/scan", post(strm_scan))
        .route("/api/strm/run", post(strm_run))
        .route("/api/strm/status", get(strm_status))
        .route("/api/strm/list", get(strm_list))
        // 内置代理内核（状态/配置/重载/日志，都在鉴权区内）
        .route("/api/proxy/status", get(proxy_status))
        .route("/api/proxy/config", put(put_proxy))
        .route("/api/proxy/reload", post(proxy_reload))
        .route("/api/proxy/log", get(proxy_log))
        // 多源人工精选
        .route("/api/scrape/candidates", post(scrape_candidates))
        .route("/api/videos/manual", get(list_manual_meta))
        .route(
            "/api/videos/{number}/meta",
            put(save_manual_meta).delete(clear_manual_meta),
        )
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware));

    // login/health 公开；其余 API 过鉴权
    let api = Router::new()
        .route("/api/auth/login", post(login))
        .route("/api/health", get(health))
        .merge(protected)
        .layer(cors_layer());

    let dist = web_dist_dir();
    if dist.join("index.html").exists() {
        let index = ServeFile::new(dist.join("index.html"));
        let static_svc = ServeDir::new(&dist)
            .append_index_html_on_directories(true)
            .not_found_service(index);
        Router::new()
            .merge(api)
            .fallback_service(static_svc)
            .layer(TraceLayer::new_for_http())
            .with_state(state)
    } else {
        tracing::warn!("web/dist 不存在，仅提供 API。构建前端: cd web && npm run build");
        Router::new()
            .merge(api)
            .fallback(spa_fallback)
            .layer(TraceLayer::new_for_http())
            .with_state(state)
    }
}

/// 监听并常驻。绑定地址走 `MDC_BIND`（缺省 127.0.0.1:9208），
/// 定时调度与内置代理内核在这里拉起——安卓壳调它 = 进程内完整引擎。
pub async fn serve(state: AppState) -> Result<()> {
    tokio::spawn(strm_scheduler(state.clone()));
    // 上次是启用状态的话，启动即拉起内核（后台，不阻塞服务就绪）
    sync_proxy(state.clone()).await;

    let bind: SocketAddr = std::env::var("MDC_BIND")
        .unwrap_or_else(|_| "127.0.0.1:9208".into())
        .parse()?;
    tracing::info!("mdc-server v{VERSION} listening on http://{bind}");
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, build_router(state)).await?;
    Ok(())
}

// ---------- 鉴权 ----------

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String,
    exp: usize,
}

fn issue_token(key: &str, username: &str) -> Result<String> {
    let claims = Claims {
        sub: username.to_string(),
        exp: (chrono::Utc::now() + chrono::Duration::hours(72)).timestamp() as usize,
    };
    Ok(encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(key.as_bytes()),
    )?)
}

async fn auth_middleware(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    // 未配置账号密码 = 不启用鉴权（本地单机模式）
    let enabled = state.cfg.read().await.auth_enabled();
    if enabled {
        let token = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(|s| s.to_string());
        let ok = match token {
            Some(t) => decode::<Claims>(
                &t,
                &DecodingKey::from_secret(state.jwt_key.as_bytes()),
                &Validation::new(Algorithm::HS256),
            )
            .is_ok(),
            None => false,
        };
        if !ok {
            return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
        }
    }
    next.run(request).await
}

// ---------- 请求/响应 ----------

#[derive(Deserialize)]
struct LoginReq {
    username: String,
    password: String,
}

#[derive(Deserialize)]
struct ScanReq {
    dir: String,
}

#[derive(Deserialize)]
struct RunReq {
    #[serde(default = "default_mode")]
    mode: String,
    library_root: Option<String>,
}

fn default_mode() -> String {
    "hard_link".into()
}

#[derive(Deserialize)]
struct ParseReq {
    filename: String,
}

#[derive(Deserialize)]
struct ScrapeTestReq {
    number: String,
}

fn parse_mode(s: &str) -> anyhow::Result<OrganizeMode> {
    // ⚠️ 未知取值必须报错，不能兜底成 HardLink：
    // 前端发 "strm" 而这里悄悄变成 hard_link，就会对网盘挂载做硬链接 —— 后果不可预料。
    Ok(match s {
        "hard_link" => OrganizeMode::HardLink,
        "copy" => OrganizeMode::Copy,
        "move" => OrganizeMode::Move,
        "symlink" => OrganizeMode::Symlink,
        "in_place" => OrganizeMode::InPlace,
        "strm" => OrganizeMode::Strm,
        other => anyhow::bail!("未知的整理方式：{other}"),
    })
}

// ---------- handlers ----------

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true, "version": VERSION }))
}

async fn login(State(state): State<AppState>, Json(req): Json<LoginReq>) -> Response {
    let cfg = state.cfg.read().await;
    let ok = cfg.auth.username.as_deref() == Some(req.username.as_str())
        && cfg.auth.password.as_deref() == Some(req.password.as_str());
    if !ok {
        return (StatusCode::UNAUTHORIZED, "用户名或密码错误").into_response();
    }
    match issue_token(&state.jwt_key, &req.username) {
        Ok(t) => Json(serde_json::json!({ "token": t })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_config(State(state): State<AppState>) -> Json<AppConfig> {
    Json(state.cfg.read().await.clone())
}

async fn put_config(
    State(state): State<AppState>,
    Json(cfg): Json<AppConfig>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    cfg.save().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    *state.cfg.write().await = cfg;
    // 整体配置里也含 [proxy]，改了就得让内核跟着变，否则 UI 上开关与真实状态不一致
    sync_proxy(state).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn parse_file(Json(req): Json<ParseReq>) -> Json<mdc_core::parser::ParsedFilename> {
    Json(mdc_core::parser::parse_filename(&req.filename))
}

// ---------- 网盘（CD2 挂载）与 .strm ----------

#[derive(Deserialize)]
struct NetdiskPutReq {
    netdisk: mdc_core::cd2::Cd2Config,
    strm: mdc_core::strm::StrmConfig,
    /// 目录源。**可选**：老前端不带这个字段时保持原样，不把配置清空。
    #[serde(default)]
    source: Option<mdc_core::source::SourceConfig>,
}

#[derive(Deserialize)]
struct SourceProbeReq {
    /// 要探的目录；留空 = 源根（local 是挂载根，webdav 是基址）
    #[serde(default)]
    dir: Option<String>,
}

#[derive(Deserialize)]
struct StrmRunReq {
    /// 忽略增量清单，全量重写一轮
    #[serde(default)]
    force: bool,
}

/// 真写一个探针文件来判断可写性。
///
/// ⚠️ 只看 `[ -d ]` 或 `is_dir()` **不算数** —— 目录存在但只读、或落在
/// 不可写的挂载上，判断会假绿，等到真正写 `.strm` 时才炸（douyin-nas 的 fpk 就栽在这）。
fn probe_writable(dir: &std::path::Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(format!(".mdc_probe_{}", std::process::id()));
    match std::fs::write(&probe, b"ok") {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

// ---------- 内置代理内核 ----------

#[derive(Deserialize)]
struct ProxyPutReq {
    proxy: ProxyConfig,
}

async fn proxy_status(State(state): State<AppState>) -> Json<serde_json::Value> {
    let cfg = state.cfg.read().await.proxy.clone();
    let mut m = state.proxy.lock().await;
    // 内核可能自己崩了：每次问状态都顺手检查一次，别让 UI 一直显示「运行中」
    m.refresh();
    Json(serde_json::json!({
        "enabled": cfg.enabled,
        "kernel": cfg.kernel,
        "port": cfg.port,
        // 把完整配置回给前端：UI 只调这一个接口就能渲染表单并原样提交
        "proxy": cfg,
        "phase": m.phase().clone(),
        // 实际生效的出网代理：内核 / 外部代理 / 直连(None)
        "effective_proxy": m.effective_proxy(),
        // 未启用时也报，UI 才能提示「先放一个 mihomo 到这里」
        "kernel_found": proxy::find_kernel(&cfg).is_ok(),
    }))
}

async fn put_proxy(
    State(state): State<AppState>,
    Json(req): Json<ProxyPutReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    {
        let mut cfg = state.cfg.write().await;
        cfg.proxy = req.proxy;
        cfg.save()
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }
    sync_proxy(state).await;
    Ok(Json(serde_json::json!({"ok": true})))
}

async fn proxy_reload(State(state): State<AppState>) -> Json<serde_json::Value> {
    sync_proxy(state).await;
    Json(serde_json::json!({"ok": true}))
}

async fn proxy_log(
    State(state): State<AppState>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let lines = q
        .get("lines")
        .and_then(|s| s.parse().ok())
        .unwrap_or(50usize)
        .min(500);
    Json(serde_json::json!({ "log": state.proxy.lock().await.log_tail(lines) }))
}

/// 按当前配置拉起/停掉内核。**启动放到后台**：拉订阅 + 等端口最多十几秒，
/// 在请求里同步等会把 HTTP 请求拖死。
async fn sync_proxy(state: AppState) {
    let cfg = state.cfg.read().await.proxy.clone();
    if !cfg.enabled {
        // 走 disable 而不是 stop：要一并同步配置并清掉状态
        state.proxy.lock().await.disable(cfg);
        return;
    }
    tokio::spawn(async move {
        let mut m = state.proxy.lock().await;
        // 传**最新**配置：manager 内部那份可能是启动时抄的旧副本
        if let Err(e) = m.start(cfg).await {
            // 起不来不等于不能干活：降级链会自动回落到外部代理/直连，
            // 但必须让用户在 UI 上看得到原因
            tracing::warn!(error = %e, "内置代理内核启动失败，已回落");
        }
    });
}

async fn netdisk_state(State(state): State<AppState>) -> Json<serde_json::Value> {
    let cfg = state.cfg.read().await.clone();
    let out_root = cfg.strm_root();
    let mount_root = cfg.netdisk.mount_root.trim();
    // 🔴 目录存不存在要**问目录源**：webdav 源（安卓）下根本没有本机路径可以 `is_dir()`，
    // 一律用 std::path 判断会让 APK 端永远显示「挂载未就绪」。
    let src = cfg.dir_source().ok();
    let mut mount_ok = false;
    let mut job_states: Vec<serde_json::Value> = Vec::new();
    if let Some(s) = &src {
        mount_ok = match s.kind() {
            mdc_core::source::SourceKind::Local => {
                !mount_root.is_empty() && std::path::Path::new(mount_root).is_dir()
            }
            mdc_core::source::SourceKind::WebDav => matches!(s.exists_dir("/").await, Ok(true)),
        };
        for j in &cfg.strm.jobs {
            let dir = j.trim();
            let exists = !dir.is_empty() && matches!(s.exists_dir(dir).await, Ok(true));
            job_states.push(serde_json::json!({ "dir": j, "exists": exists }));
        }
    }
    Json(serde_json::json!({
        "netdisk": cfg.netdisk,
        "strm": cfg.strm,
        "source": cfg.source,
        "source_kind": src.as_ref().map(|s| s.kind().to_string()),
        "source_root": src.as_ref().map(|s| s.root()),
        "source_ok": src.is_some(),
        "strm_root": out_root.to_string_lossy(),
        "manifest_path": cfg.strm_manifest_path().to_string_lossy(),
        "mount_ok": mount_ok,
        "out_writable": probe_writable(&out_root),
        "jobs": job_states,
    }))
}

/// 试连目录源：列出指定目录（默认源根）里的前若干条，并给出**直链样例**。
///
/// 目的很实际：用户填完 WebDAV 基址/挂载根后，得有个地方能立刻看到
/// 「连上了没、网盘路径算得对不对」—— 否则只能等真跑一轮失败才知道。
async fn source_probe(
    State(state): State<AppState>,
    Json(req): Json<SourceProbeReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let cfg = state.cfg.read().await.clone();
    let src = cfg
        .dir_source()
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    // 默认探**第一个监控目录**（没有才退回源根）：用户想验证的是「我配的那个目录通不通」，
    // 探源根只能看到一层网盘名，看不出直链对不对。
    let fallback = cfg
        .strm
        .jobs
        .iter()
        .map(|s| s.trim())
        .find(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| src.root());
    let dir = match req.dir.as_deref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(d) => d.to_string(),
        None => fallback,
    };
    // 只探一层：探针要快，别一口气把整个网盘翻一遍
    let entries = src
        .list(&dir, false, 1)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("列目录失败：{e}")))?;
    let sample: Vec<serde_json::Value> = entries
        .iter()
        .take(10)
        .map(|e| {
            let url = src
                .cloud_path(&e.path)
                .ok()
                .and_then(|cp| mdc_core::cd2::build_url_from_cloud(&cp, &cfg.netdisk).ok());
            serde_json::json!({
                "path": e.path,
                "name": e.name,
                "size": e.size,
                "cloud_path": src.cloud_path(&e.path).ok(),
                "url": url,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({
        "kind": src.kind().to_string(),
        "root": src.root(),
        "dir": dir,
        "total": entries.len(),
        "sample": sample,
    })))
}

async fn put_netdisk(
    State(state): State<AppState>,
    Json(req): Json<NetdiskPutReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut cfg = state.cfg.read().await.clone();
    cfg.netdisk = req.netdisk;
    cfg.strm = req.strm;
    if let Some(src) = req.source {
        cfg.source = src;
    }
    cfg.save()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    *state.cfg.write().await = cfg;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// 只扫不写：预览这一轮会处理哪些文件、直链长什么样。
async fn strm_scan(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let cfg = state.cfg.read().await.clone();
    let src = cfg
        .dir_source()
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let mut items = Vec::new();
    let mut unparseable = 0usize;
    for job in &cfg.strm.jobs {
        let job = job.trim();
        if job.is_empty() {
            continue;
        }
        if !matches!(src.exists_dir(job).await, Ok(true)) {
            continue;
        }
        let Ok(entries) = src.list(job, cfg.strm.recursive, cfg.strm.max_depth).await else {
            continue;
        };
        for e in entries {
            let s = e.path.clone();
            let parsed = mdc_core::parser::parse_filename(&s);
            if parsed.number.is_none() {
                unparseable += 1;
                continue;
            }
            // 网盘路径由目录源给，直链再从网盘路径拼（安卓端没有本机路径可反推）
            let url = src
                .cloud_path(&s)
                .ok()
                .and_then(|cp| mdc_core::cd2::build_url_from_cloud(&cp, &cfg.netdisk).ok())
                .unwrap_or_default();
            items.push(serde_json::json!({
                "source": s,
                "number": parsed.number,
                "url": url,
                "url_ok": !url.is_empty(),
            }));
        }
    }
    let total = items.len();
    items.truncate(50);
    Ok(Json(serde_json::json!({
        "total": total,
        "unparseable": unparseable,
        "preview": items,
        "out_root": cfg.strm_root().to_string_lossy(),
    })))
}

/// 真正跑一轮：扫描 → 刮削 → 写 .strm + NFO + 海报。
async fn strm_run(
    State(state): State<AppState>,
    Json(req): Json<StrmRunReq>,
) -> Result<Json<pipeline::StrmRunStats>, (StatusCode, String)> {
    let cfg = state.cfg.read().await.clone();
    let ctx = scrape::ScrapeCtx::from_config(&cfg)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("构建 HTTP 客户端失败: {e}")))?;
    let engine = state.engine.clone();
    let pool = state.pool.clone();
    let stats =
        pipeline::run_strm_jobs(&state.strm_runner, &pool, &engine, &ctx, &cfg, req.force)
            .await
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(stats))
}

/// 网盘刮削的运行状态（定时器开关、上次/下次运行、已生成数量）。
async fn strm_status(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let cfg = state.cfg.read().await.clone();
    let manifest = mdc_core::strm::Manifest::load(&cfg.strm_manifest_path());
    let generated = manifest
        .entries
        .values()
        .filter(|e| std::path::Path::new(&e.out).exists())
        .count();
    Ok(Json(serde_json::json!({
        "running": state.strm_runner.is_running(),
        "last_run": state.strm_runner.last_run(),
        "next_run": state.strm_runner.next_run_at(cfg.strm.interval_hours),
        "interval_hours": cfg.strm.interval_hours,
        "manifest_entries": manifest.entries.len(),
        "generated": generated,
        "out_root": cfg.strm_root().to_string_lossy(),
    })))
}

/// 定时调度循环。
///
/// 每轮重新读配置（用户改了间隔/目录要立刻生效），间隔用 `sleep` 而不是固定
/// `interval` tick —— 一轮跑几十分钟时，固定 tick 会背靠背连跑。
async fn strm_scheduler(state: AppState) {
    const ERR_RETRY: u64 = 600; // 失败重试 10 分钟（而不是干等一个完整周期）
    loop {
        let interval_hours = { state.cfg.read().await.strm.interval_hours };

        if interval_hours == 0 {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            continue;
        }

        // 冷启动补偿：进程不常驻，last_run 超一个周期（或从没跑过）就立刻补跑，
        // 否则 24h 档永远赶不上。
        let wait = match state.strm_runner.next_run_at(interval_hours) {
            Some(next) => (next - mdc_core::strm::now_unix()).max(0) as u64,
            None => 60,
        };
        if wait > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(wait.min(3600))).await;
            continue;
        }

        if state.strm_runner.is_running() {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            continue;
        }

        let cfg = state.cfg.read().await.clone();
        match scrape::ScrapeCtx::from_config(&cfg) {
            Ok(ctx) => {
                tracing::info!(interval_hours, "定时轮：开始网盘刮削");
                match pipeline::run_strm_jobs(
                    &state.strm_runner,
                    &state.pool,
                    &state.engine,
                    &ctx,
                    &cfg,
                    false,
                )
                .await
                {
                    Ok(s) => tracing::info!(
                        total = s.total,
                        added = s.added,
                        skipped = s.skipped,
                        failed = s.failed,
                        "定时轮完成"
                    ),
                    Err(e) => {
                        tracing::warn!(error = %e, "定时轮失败，{ERR_RETRY}s 后重试");
                        tokio::time::sleep(std::time::Duration::from_secs(ERR_RETRY)).await;
                    }
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "定时轮构建 HTTP 客户端失败");
                tokio::time::sleep(std::time::Duration::from_secs(ERR_RETRY)).await;
            }
        }
    }
}

/// 已生成的 `.strm` 清单（只读媒体库输出目录，不碰网盘）。
async fn strm_list(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let cfg = state.cfg.read().await.clone();
    let root = cfg.strm_root();
    let mut files = Vec::new();
    collect_strm(&root, &root, &mut files, 0);
    files.sort();
    let total = files.len();
    let head: Vec<String> = files.into_iter().take(500).collect();
    Ok(Json(serde_json::json!({
        "root": root.to_string_lossy(),
        "total": total,
        "files": head,
    })))
}

fn collect_strm(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<String>, depth: u32) {
    if depth > 12 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_strm(root, &p, out, depth + 1);
        } else if p.extension().and_then(|e| e.to_str()) == Some("strm") {
            if let Ok(rel) = p.strip_prefix(root) {
                out.push(rel.to_string_lossy().to_string());
            }
        }
    }
}

async fn scrape_test(
    State(state): State<AppState>,
    Json(req): Json<ScrapeTestReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let cfg = state.cfg.read().await.clone();
    let ctx = scrape::ScrapeCtx::from_config(&cfg)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("构建 HTTP 客户端失败: {e}")))?;
    let metas = state.engine.scrape(&ctx, &req.number).await;
    Ok(Json(serde_json::json!({ "results": metas })))
}

// ---------- 多源人工精选 ----------

#[derive(Deserialize)]
struct CandidatesReq {
    number: String,
}

#[derive(Deserialize)]
struct SaveMetaReq {
    /// 用户挑中的那一条元数据（前端直接把候选里的 meta 回传）
    meta: mdc_core::model::VideoMeta,
}

/// 拉取**每个源各自的原始结果**（不合并），供人工比较挑选。
async fn scrape_candidates(
    State(state): State<AppState>,
    Json(req): Json<CandidatesReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let cfg = state.cfg.read().await.clone();
    let ctx = scrape::ScrapeCtx::from_config(&cfg)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("构建 HTTP 客户端失败: {e}")))?;
    let list = state.engine.candidates(&ctx, &req.number).await;
    Ok(Json(serde_json::json!({
        "number": req.number,
        "candidates": list,
    })))
}

/// 保存人工精选结果。之后这个番号的处理会**直接用它、不再刮削**。
async fn save_manual_meta(
    State(state): State<AppState>,
    Path(number): Path<String>,
    Json(req): Json<SaveMetaReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // 番号必须与 URL 一致，否则会出现「存到 A、内容写着 B」这种脏数据
    if !req.meta.number.is_empty() && req.meta.number != number {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("番号不一致：URL 是 {number}，元数据里是 {}", req.meta.number),
        ));
    }
    let json = serde_json::to_string(&req.meta)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    db::set_manual_meta(&state.pool, &number, req.meta.title.as_deref(), &json)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true, "number": number })))
}

/// 取消人工精选（普通记录保留，之后会恢复自动刮削）。
async fn clear_manual_meta(
    State(state): State<AppState>,
    Path(number): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let n = db::clear_manual_meta(&state.pool, &number)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true, "cleared": n })))
}

async fn list_manual_meta(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let items = db::list_manual(&state.pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let items: Vec<serde_json::Value> = items
        .into_iter()
        .map(|(number, title)| serde_json::json!({ "number": number, "title": title }))
        .collect();
    Ok(Json(serde_json::json!({ "items": items })))
}

async fn create_tasks(
    State(state): State<AppState>,
    Json(req): Json<ScanReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let n = pipeline::create_tasks_for_dir(&state.pool, std::path::Path::new(&req.dir))
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(serde_json::json!({ "created": n })))
}

async fn list_tasks(
    State(state): State<AppState>,
) -> Result<Json<Vec<db::TaskRow>>, (StatusCode, String)> {
    let rows = db::list_tasks(&state.pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(rows))
}

async fn run_tasks(
    State(state): State<AppState>,
    Json(req): Json<RunReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mode = parse_mode(&req.mode).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let cfg = state.cfg.read().await.clone();
    // strm 模式的落点根固定走配置，别让前端漏传就写到网盘挂载里去
    let library_root = match (mode, req.library_root) {
        (OrganizeMode::Strm, _) => Some(cfg.strm_root()),
        (_, Some(r)) => Some(PathBuf::from(r)),
        (_, None) => None,
    };
    let ctx = scrape::ScrapeCtx::from_config(&cfg)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    // 同步跑（规模大了再改队列）；现在先串行执行并返回处理数量
    let engine = state.engine.clone();
    let pool = state.pool.clone();
    let n = pipeline::process_pending(&pool, &engine, &ctx, &cfg, mode, library_root.as_deref())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "processed": n })))
}

async fn delete_task(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    sqlx::query("DELETE FROM tasks WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn spa_fallback(_uri: axum::http::Uri) -> Response {
    let dist = web_dist_dir();
    let index = dist.join("index.html");
    if index.exists() {
        match tokio::fs::read(&index).await {
            Ok(bytes) => {
                return (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    bytes,
                )
                    .into_response();
            }
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    }
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::json!({
            "hint": "前端未构建。运行: cd web && npm install && npm run build",
            "version": VERSION
        })
        .to_string(),
    )
        .into_response()
}

fn web_dist_dir() -> PathBuf {
    std::env::var("MDC_WEB_DIST")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./web/dist"))
}
