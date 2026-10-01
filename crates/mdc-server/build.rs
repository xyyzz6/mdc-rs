//! `embed-web` 前置步骤：把 `web/dist` 拷进 `OUT_DIR/web`，并生成
//! `$OUT_DIR/web_mod.rs`（一个静态资产表），让单文件 exe 在编译期把前端
//! 整个打进二进制。
//!
//! 为什么不用 `include_dir` 宏：它只接受**单个字符串字面量**（传 `concat!`
//! 会 panic：「This macro only accepts a single, non-empty string argument」），
//! 拿不到 `OUT_DIR` 这种编译期才知道的绝对路径。自己生成一张表更可控，
//! 顺带把 MIME 表也写在生成侧。
//!
//! 前端产物是构建生成的（`web/dist` 被 .gitignore），没它时不能让
//! `embed-web` 直接编译失败——否则 clones 仓库的人一编就挂。
//! 这里退化成一个占位页面，跑 `cd web && npm run build` 重编即可拿到真前端。

use std::path::{Path, PathBuf};
use std::{fs, io};

const PLACEHOLDER: &str = r#"<!doctype html><meta charset="utf-8">
<title>拾光 · PickLight</title>
<body style="font:16px system-ui;padding:40px;color:#333">
<h1>拾光 · PickLight</h1>
<p>前端尚未内嵌进可执行文件。请构建前端后重新编译：</p>
<pre>cd web &amp;&amp; npm install &amp;&amp; npm run build
cargo build --release -p mdc-server --features embed-web</pre>
"#;

/// 只列前端真正会用的类型（含 sha256 指纹命名的 assets，按扩展名判 MIME）。
fn mime_of(name: &str) -> &'static str {
    const T: &[(&str, &str)] = &[
        (".html", "text/html; charset=utf-8"),
        (".htm", "text/html; charset=utf-8"),
        (".js", "application/javascript; charset=utf-8"),
        (".mjs", "application/javascript; charset=utf-8"),
        (".css", "text/css; charset=utf-8"),
        (".json", "application/json; charset=utf-8"),
        (".svg", "image/svg+xml"),
        (".png", "image/png"),
        (".jpg", "image/jpeg"),
        (".jpeg", "image/jpeg"),
        (".webp", "image/webp"),
        (".ico", "image/x-icon"),
        (".woff", "font/woff"),
        (".woff2", "font/woff2"),
        (".ttf", "font/ttf"),
        (".map", "application/json; charset=utf-8"),
        (".txt", "text/plain; charset=utf-8"),
    ];
    T.iter()
        .find(|(ext, _)| name.ends_with(ext))
        .map(|(_, m)| *m)
        .unwrap_or("application/octet-stream")
}

fn main() {
    // ⚠️ 必须用 env::var：OUT_DIR 只在 build script 运行期有，
    // 写成 env!("OUT_DIR") 会在编译本脚本时就炸。
    // 路径一律基于 CARGO_MANIFEST_DIR，别信 cwd（rc 里 cwd 未必是包根）。
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out: PathBuf = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("web");
    // manifest = <workspace>/crates/mdc-server ⇒ 前端在 <workspace>/web/dist
    let dist = manifest.join("../../web/dist");

    // 每次都从干净目录开始，避免上一次的旧文件（已删除的资产）留在包里
    let _ = fs::remove_dir_all(&out);
    fs::create_dir_all(&out).expect("创建 OUT_DIR/web");

    if dist.join("index.html").exists() {
        copy_dir(&dist, &out).expect("拷贝 web/dist");
        println!("cargo:rerun-if-changed=../web/dist");
    } else {
        println!("cargo:warning=web/dist 不存在，embed-web 只打占位页面");
        fs::write(out.join("index.html"), PLACEHOLDER).expect("写占位页面");
    }

    let _ = gen_mod(&out);
}

/// 生成资产表：每个文件一条 `(&str 路径, &str MIME, &[u8] 内容)`。
fn gen_mod(out: &Path) -> io::Result<()> {
    let mut rows: Vec<String> = Vec::new();
    walk(out, out, &mut rows, "")?;

    // 注意：生成文件是插进 `mod embedded { ... }` 里的，只能用 `//` 注释——
    // `//!` 是**项内部**文档注释，出现在 static 上会报 E0753。
    let mut buf = String::from(
        "// 由 build.rs 生成：web/dist 的内嵌资产表（单文件 exe 用）。\n\
         // 别手改，改 crates/mdc-server/build.rs。\n\
         pub static FILES: &[(&str, &str, &[u8])] = &[\n",
    );
    for r in rows {
        buf.push_str(&r);
    }
    buf.push_str("];\n");
    fs::write(out.join("web_mod.rs"), buf)?;
    Ok(())
}

fn walk(base: &Path, dir: &Path, rows: &mut Vec<String>, prefix: &str) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            walk(base, &entry.path(), rows, &format!("{prefix}{name}/"))?;
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let path = format!("{prefix}{name}");
        let mime = mime_of(&name);
        // include_bytes! 的路径相对**本生成文件**（同为 OUT_DIR）解析
        rows.push(format!(
            "    (\"{path}\", \"{mime}\", include_bytes!(\"{}\")),\n",
            dir.join(&name).to_string_lossy().replace('\\', "/")
        ));
        let _ = base;
    }
    Ok(())
}

fn copy_dir(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
