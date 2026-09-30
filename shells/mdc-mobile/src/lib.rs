//! Android 壳：完整引擎跑在手机上。
//!
//! Rust 核心作为 cdylib 编进 APK，在本进程内启动 axum 服务
//! （127.0.0.1:9208），WebView 加载内嵌前端（web/dist），API 打到
//! 本机 9208。数据目录（SQLite/配置/密钥）通过 `MDC_CONFIG_PATH`
//! 重定向到应用私有目录（app data dir）。
//!
//! 进程内还会拉起一个「伪装成 .so 的可执行文件」（Android 10 W^X 下
//! 只有 nativeLibraryDir 允许 execve，所以必须由 APK 以 lib*.so 携带、
//! useLegacyPackaging/extractNativeLibs 落地）：
//!
//! * `libclouddrive.so` —— CloudDrive2 官方安卓引擎（v1.0.5 提取），
//!   监听 127.0.0.1:19798，WebDAV 在 `/dav`。用户在它的管理页登录
//!   CD2 账号并挂载 115，之后 mdc-rs 的 WebDav 数据源直连
//!   `http://127.0.0.1:19798/dav` 就能扫到网盘视频。
//!
//! 另有 `libmihomo.so`（mihomo android-arm64）不在此自动启动：ProxyManager
//! 需要用户自己的订阅链接，UI 触发 start 时经 `MDC_PROXY_KERNEL`（这里指向
//! nativeLibraryDir/libmihomo.so）找到内核。
//!
//! 构建步骤（需要 Android SDK/NDK）：
//!   rustup target add aarch64-linux-android
//!   cargo tauri android init
//!   cargo tauri android build --apk --debug

use std::io::Read as _;
use std::io::Seek as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tauri::Manager;

/// CD2 引擎端口（官方默认；douyin-nas 同款，管理页与 WebDAV 同端口）
const CD2_PORT: u16 = 19798;
/// CD2 管理页静态文件（42 个文件，全部 ZIP_STORED 免压缩 —— 运行时按本地
/// 文件头直接切片落盘，不需要 inflate 依赖）。构建前用 python zipfile
/// `compression=ZIP_STORED` 重打包生成（cd2wwwroot_stored.zip）。
static CD2_WWWROOT: &[u8] = include_bytes!("../cd2wwwroot_stored.zip");

/// 内置 CD2 子进程句柄（App 退出时收尸，避免孤儿进程占着 19798）
static CD2_CHILD: Mutex<Option<std::process::Child>> = Mutex::new(None);

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // 数据目录必须指到应用私有目录：/data/data/<pkg>/ 下，
            // 再用相对路径 ./data 会落进不可写的安装目录。
            let data = app.path().app_data_dir()?;
            std::env::set_var("MDC_CONFIG_PATH", &data);
            std::env::set_var("MDC_BIND", "127.0.0.1:9208");
            // .strm 落点 = 应用私有目录（data_dir/strm）。自带媒体库 302 播放
            // 不需要别的 App 读到 strm，存 app 内最稳（外部存储曾经注入过，
            // 店主拍板撤掉；config.rs 的 MDC_STRM_ROOT 钩子保留给需要的人）。
            // 🔴 代理内核：APK 把 mihomo 以 libmihomo.so 携带，安装后落在
            //    nativeLibraryDir（Android 10 W^X 下唯一可 exec 的目录）。
            //    find_kernel 的第二优先级（MDC_PROXY_KERNEL 环境变量）接住它。
            if let Some(libdir) = native_library_dir() {
                let mihomo = libdir.join("libmihomo.so");
                if mihomo.exists() {
                    std::env::set_var("MDC_PROXY_KERNEL", &mihomo);
                }
                // CD2 引擎独立于 ProxyManager，setup 时直接后台拉起
                spawn_cd2(&libdir, &data);
            }
            std::thread::spawn(|| {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("tokio runtime");
                rt.block_on(async {
                    if let Err(e) = start_engine().await {
                        eprintln!("引擎启动失败: {e:#}");
                    }
                });
            });
            // tauri.conf.json 的 windows 为空 ⇒ 必须在代码里显式建窗口，
            // 否则安卓端 wry 不会创建 WebView（现象：白屏 + 控件树只有空 FrameLayout）。
            tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::default())
                .title("MDC-RS")
                .build()?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            // 收尸：App 退出时杀掉 CD2 子进程（否则占着 19798，下次启动
            // 会被「端口已有人听 → 复用」接住 —— 孤儿进程锁着旧数据库
            // 句柄，行为很难预期）。
            if let tauri::RunEvent::Exit = event {
                if let Ok(mut guard) = CD2_CHILD.lock() {
                    if let Some(child) = guard.as_mut() {
                        let _ = child.kill();
                    }
                    *guard = None;
                }
            }
        });
}

/// 进程内启动完整引擎：与桌面 exe 完全同套路由/状态（mdc-server 库入口）。
async fn start_engine() -> anyhow::Result<()> {
    let state = mdc_server::build_state().await?;
    mdc_server::serve(state).await
}

/// Android 10 W^X：只有 nativeLibraryDir 允许 execve。本进程的
/// libmdc_mobile.so 被 System.loadLibrary 加载过 ⇒ /proc/self/maps 里
/// 一定有它的落地路径，取其父目录即 nativeLibraryDir。
fn native_library_dir() -> Option<PathBuf> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    for line in maps.lines().rev() {
        if line.ends_with("/libmdc_mobile.so") {
            let path = line.split_whitespace().last()?;
            return Path::new(path).parent().map(|p| p.to_path_buf());
        }
    }
    None
}

/// 拉起内置 CD2 引擎（幂等：19798 已有人听就复用，不硬起第二个 ——
/// bind 失败会静默退出，表现为「内置网盘永远连不上」的半死状态）。
/// spawn 只发车，就绪探测丢后台线程（首次要建库，几秒）。
fn spawn_cd2(libdir: &Path, data_dir: &Path) {
    let home = data_dir.join("cd2");
    let _ = std::fs::create_dir_all(&home);

    let exe = libdir.join("libclouddrive.so");
    if !exe.exists() {
        eprintln!("APK 未内置 CD2 引擎（缺 {}）", exe.display());
        return;
    }

    // 端口已有人在听 = 上一个实例还活着，直接复用
    if std::net::TcpStream::connect(("127.0.0.1", CD2_PORT)).is_ok() {
        eprintln!("CD2 :{CD2_PORT} 已有实例在跑，复用");
        return;
    }

    // 预写 config.toml —— 只在不存在时写。引擎启动时会自己补全/重写，
    // 硬管反而和它的写入打架（douyin-nas 实测教训）。
    let cfg = home.join("config.toml");
    if !cfg.exists() {
        let _ = std::fs::write(
            &cfg,
            format!(
                "[webconfig]\n\
                 www_root = \"./wwwroot\"\n\
                 http_port = {CD2_PORT}\n\
                 https_port = {}\n\
                 enable_https = false\n\
                 webdav_root = \"/\"\n\
                 webdav_readonly = false\n\
                 webdav_enable_guest = false\n",
                CD2_PORT + 1
            ),
        );
    }

    // 解压管理页静态文件。🔴 没有它管理页 404 黑屏但 WebDAV 活着 ——
    // 引擎的 web 界面不在二进制里，官方 App 是解 assets 包到数据目录。
    ensure_cd2_wwwroot(&home);

    // 🔴 CLOUDDRIVE_HOME 必须指向可写目录，否则引擎去写死路径
    //    /Waytech/CloudDrive2/log 直接崩（Read-only file system）。
    let log = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.join("stdout.log"))
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("打开 cd2/stdout.log 失败：{e}");
            return;
        }
    };
    match std::process::Command::new(&exe)
        .env("CLOUDDRIVE_HOME", &home)
        .current_dir(&home)
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::inherit())
        .spawn()
    {
        Ok(child) => {
            eprintln!("内置 CD2 引擎已拉起：{}", exe.display());
            if let Ok(mut guard) = CD2_CHILD.lock() {
                *guard = Some(child);
            }
        }
        Err(e) => {
            eprintln!("启动内置 CD2 引擎失败（{}）：{e}", exe.display());
            return;
        }
    }

    // 就绪探测：首次启动要建 sqlite 库，30s 内轮询端口（不阻塞主流程）
    std::thread::spawn(move || {
        for _ in 0..75 {
            if std::net::TcpStream::connect(("127.0.0.1", CD2_PORT)).is_ok() {
                eprintln!("内置 CD2 引擎已就绪，监听 :{CD2_PORT}");
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(400));
        }
        eprintln!("内置 CD2 引擎 30s 内未就绪（日志：cd2/stdout.log）");
    });
}

/// 解压嵌入的 wwwroot（全 stored zip）。`wwwroot/index.html` 已存在则跳过。
/// 只按本地文件头（0x04034b50）顺序切片 —— 包是我们自己 ZIP_STORED 打的，
/// 结构可控；路径做 zip-slip 防护。
fn ensure_cd2_wwwroot(home: &Path) {
    if home.join("wwwroot/index.html").exists() {
        return;
    }
    let mut r = std::io::Cursor::new(CD2_WWWROOT);
    let mut count = 0u32;
    loop {
        let mut sig = [0u8; 4];
        if r.read_exact(&mut sig).is_err() {
            break;
        }
        if sig != [0x50, 0x4b, 0x03, 0x04] {
            break; // 到中央目录了，顺序写的包不会再有本地头
        }
        let mut hdr = [0u8; 26];
        if r.read_exact(&mut hdr).is_err() {
            break;
        }
        // hdr 是去掉 4 字节签名后的本地头：method@4、comp_size@14、
        // name_len@22、extra_len@24
        let name_len = u16::from_le_bytes([hdr[22], hdr[23]]) as usize;
        let extra_len = u16::from_le_bytes([hdr[24], hdr[25]]) as usize;
        let comp_size = u32::from_le_bytes([hdr[14], hdr[15], hdr[16], hdr[17]]) as u64;
        let mut name = vec![0u8; name_len];
        if r.read_exact(&mut name).is_err() {
            break;
        }
        if r.seek(std::io::SeekFrom::Current(extra_len as i64)).is_err() {
            break;
        }
        let name = String::from_utf8_lossy(&name).to_string();
        if name.ends_with('/') {
            continue; // 目录项
        }
        let mut data = vec![0u8; comp_size as usize];
        if r.read_exact(&mut data).is_err() {
            break;
        }
        // zip-slip 防护：只保留普通组件
        let mut out_path = home.to_path_buf();
        for part in Path::new(&name).components() {
            if let std::path::Component::Normal(p) = part {
                out_path.push(p);
            }
        }
        if !out_path.starts_with(home) {
            continue;
        }
        if let Some(parent) = out_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::write(&out_path, &data).is_ok() {
            count += 1;
        }
    }
    eprintln!("已解压 CD2 管理页静态文件 {count} 个");
}
