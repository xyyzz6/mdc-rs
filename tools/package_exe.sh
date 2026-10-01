#!/usr/bin/env bash
# 打包**单文件 Windows 版**（前端编译期内嵌进 exe，不需要 web-dist 目录）。
#
#   bash tools/package_exe.sh                 # 构建 + 校验 + 出 zip
#   bash tools/package_exe.sh --skip-build    # 只校验已有产物
#
# 产物：dist/picklight-<版本>-windows-x64.zip（内含 PickLight.exe + mihomo.exe）
#
# ⚠️ 校验必须是「字节级」的：只看着 exe 生成了不够，最典型的坑是
#    build.rs 拷的是**旧** web/dist（改完前端没重编 build.rs ⇒ 包里是老 UI，
#    从外面看不出来）。所以这里 grep 前端产物里的 JS/CSS 指纹串。
set -u
cd "$(dirname "$0")/.."
export PATH="/c/Users/25407/.cargo/bin:$PATH"

ROOT="$(pwd)"
OUT="$ROOT/dist"
# bash 里没有裸 python（本机只有托管的那个），显式挑一个
PYTHON="$(command -v python || command -v python3 || echo '/c/Users/25407/.workbuddy/binaries/python/versions/3.13.12/python.exe')"
VER="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"
ZIP="picklight-$VER-windows-x64.zip"
FAIL=0
ok()  { printf '  \033[32m✅\033[0m %s\n' "$1"; }
bad() { printf '  \033[31m❌\033[0m %s\n' "$1"; FAIL=1; }

echo "== 版本 $VER =="

if [ "${1:-}" != "--skip-build" ]; then
  if ! [ -f web/dist/index.html ]; then
    echo "⚠️ web/dist 不存在，先构建前端：cd web && npm install && npm run build"
    exit 1
  fi
  echo "== release 构建（embed-web：前端进二进制）=="
  cargo build --release -p mdc-server --features embed-web 2>&1 | tail -5 || exit 1
fi

EXE="$ROOT/target/release/mdc-server.exe"
[ -f "$EXE" ] || { echo "找不到 $EXE"; exit 1; }
# ⚠️ Windows python 认不了 /e/... 这种 MSYS 路径（FileNotFoundError），
#   传给 python 的一律转 Windows 路径
win() { cygpath -w "$1" 2>/dev/null || echo "$1"; }
EXE_W="$(win "$EXE")"

# 前端指纹：web/dist/assets 里的文件名是带 hash 的，取 js 那个
JS_BASENAME="$(python -c "
import os,glob
d='web/dist/assets'
print(os.path.basename(max(glob.glob(d+'/*.js'), key=os.path.getsize)) if glob.glob(d+'/*.js') else '')
")"
CSS_BASENAME="$(python -c "
import os,glob
d='web/dist/assets'
print(os.path.basename(max(glob.glob(d+'/*.css'), key=os.path.getsize)) if glob.glob(d+'/*.css') else '')
")"

mkdir -p "$OUT"
STAGE="$OUT/.stage-$VER"
mkdir -p "$STAGE"

# ⚠️ 这里刻意**不用 `rm -rf`**：IDE 的沙箱把 rm 换成 safe-bin/rm shim，
#    在脚本里 `rm -rf` 会被直接 SIGTERM（trace 停在 rm 那一行）。
#    cp -f 覆盖同名文件就够，不用先清目录。
cp -f "$EXE" "$STAGE/PickLight.exe"
# 内置代理内核：exe 同目录探测（kernel_path → MDC_PROXY_KERNEL → 同目录 → PATH）
if [ -f "$ROOT/dist/mdc-rs-0.1.0-windows-x64/mihomo.exe" ]; then
  cp -f "$ROOT/dist/mdc-rs-0.1.0-windows-x64/mihomo.exe" "$STAGE/mihomo.exe"
fi

echo "== 字节级校验 =="
SIZE="$(python -c "import os;print(os.path.getsize(r'''$EXE_W'''))")"
[ "$SIZE" -gt 10000000 ] && ok "PickLight.exe $SIZE 字节" || bad "PickLight.exe 只有 $SIZE 字节（太小的多半没编进去）"

for f in "$JS_BASENAME" "$CSS_BASENAME"; do
  [ -n "$f" ] || continue
  # 在 exe 里找指纹串；找不到 = 打进去的是旧前端
  if python - "$(win "$EXE")" "$f" <<'PY'
import sys
data = open(sys.argv[1], 'rb').read()
name = sys.argv[2].encode()
raise SystemExit(0 if name in data else 1)
PY
  then ok "内嵌前端含 $f"
  else bad "exe 里找不到 $f —— 打进去的是旧前端（touch crates/mdc-server/build.rs 后重编）"
  fi
done

# ⚠️ 别用 shutil.make_archive/ZIP_DEFLATED：两个 exe 合起来 100MB 级别，
#    本机 deflate 打包会被沙箱 SIGTERM 掉（实测 `shutil.make_archive` 直接
#    进程消失）。ZIP_STORED 稳过（体积约等于源文件之和，二进制本来就压不动）。
echo "== 打包 $ZIP =="
"$PYTHON" "$ROOT/tools/zip_stored.py" "$STAGE" "$ROOT/$ZIP" || bad "打 zip 失败"
if [ -f "$ROOT/$ZIP" ]; then mv -f "$ROOT/$ZIP" "$OUT/$ZIP"; ok "$OUT/$ZIP"; else bad "没产出 $OUT/$ZIP"; fi

exit $FAIL
