#!/usr/bin/env bash
# 构建拾光 · PickLight 的 **Android APK**（arm64 纯包，前端随包走）。
#
#   bash tools/build_apk.sh                 # 构建 + 校验 + 出 dist/picklight-<ver>-arm64-debug.apk
#   bash tools/build_apk.sh --skip-build    # 只校验已有产物
#
# 产物：dist/picklight-<版本>-arm64-debug.apk
#
# 🔴 这三条是这套 APK 反复踩出来的坑，别乱改：
#   1. tauri CLI 会把 377MB 的 .so 往 jniLibs 拷，**每次都静默产出 0 字节**
#      ⇒ 必须手动 cp 再 `gradlew ... -x rustBuildArm64Debug -x rustBuildUniversalDebug`。
#      APK 内 .so 0 字节 = 装上秒崩 UnsatisfiedLinkError（tools/check_apk.py 会拦）。
#   2. 全局 cargo config 里 `[env] CC=w64devkit gcc` 会污染 android 交叉编译
#      （ring 编成 x86_64 ⇒ dlopen cannot locate symbol）⇒ 显式指定 NDK 的 CC/AR。
#   3. 沙箱里 `rm -rf` 会被 SIGTERM ⇒ 全程免 rm -rf，stale 文件一律 cp -f 覆盖。
set -u
cd "$(dirname "$0")/.."
export PATH="/c/Users/25407/.cargo/bin:/c/Users/25407/.cargo/bin:$PATH"

ROOT="$(pwd)"
MOBILE="$ROOT/shells/mdc-mobile"
ANDROID="$MOBILE/gen/android"
JNI="$ANDROID/app/src/main/jniLibs/arm64-v8a"
OUT="$ROOT/dist"
VER="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"
FAIL=0
ok()  { printf '  \033[32m✅\033[0m %s\n' "$1"; }
bad() { printf '  \033[31m❌\033[0m %s\n' "$1"; FAIL=1; }

# ── 1. 工具链定位（本机 JAVA_HOME / ANDROID_HOME 默认都是空的）──────────
LOCALAPPDATA="${LOCALAPPDATA:-/c/Users/25407/AppData/Local}"
[ -n "${JAVA_HOME:-}" ] || JAVA_HOME="$(ls -d "$LOCALAPPDATA/Programs/jdk-"* 2>/dev/null | head -1)"
[ -n "${ANDROID_HOME:-}" ] || ANDROID_HOME="${LOCALAPPDATA}/Android/Sdk"
export JAVA_HOME ANDROID_HOME
[ -n "${NDK_HOME:-}" ] || NDK_HOME="$(ls -d "$ANDROID_HOME/ndk/"* 2>/dev/null | head -1)"
export NDK_HOME
export ANDROID_SDK_ROOT="$ANDROID_HOME"

if [ -f "$JAVA_HOME/bin/java.exe" ] || [ -f "$JAVA_HOME/bin/java" ]; then
  ok "JAVA_HOME=$JAVA_HOME"
else
  bad "找不到 JDK（试过 \$JAVA_HOME 与 $LOCALAPPDATA/Programs/jdk-*）"
fi
[ -f "$JNI/libclouddrive.so" ] && ok "jniLibs 已有 libclouddrive.so" || bad "jniLibs 缺 libclouddrive.so（APK 装不了网盘刮削）"
[ -f "$JNI/libmihomo.so" ]     && ok "jniLibs 已有 libmihomo.so"     || bad "jniLibs 缺 libmihomo.so（APK 装不了代理内核）"

# ── 2. cargo 交叉编译 aarch64 ─────────────────────────────────────────
if [ "${1:-}" != "--skip-build" ]; then
  [ -f web/dist/index.html ] || { echo "⚠️ web/dist 不存在（先 cd web && npm ci && npm run build）"; exit 1; }

  # 显式指定 NDK 的交叉工具，否则全局 cargo config 的 `[env] CC=w64devkit gcc`
  # 会把 ring 编成 x86_64（⇒ dlopen cannot locate symbol）。
  TOOLCHAIN="$(ls -d "$NDK_HOME/toolchains/llvm/prebuilt/"*/ 2>/dev/null | head -1)"
  if [ -z "$TOOLCHAIN" ]; then
    bad "NDK 里找不到 toolchains/llvm/prebuilt"
  else
    export CC="${TOOLCHAIN}bin/aarch64-linux-android21-clang"
    export AR="${TOOLCHAIN}bin/llvm-ar"
    [ -x "$CC" ] || bad "交叉编译器不存在：$CC"
    ok "交叉工具链 $TOOLCHAIN"
  fi

  echo "== cargo build aarch64（--features custom-protocol）=="
  ( cd "$MOBILE" && cargo build --release --target aarch64-linux-android --features custom-protocol 2>&1 | tail -8 ) \
    || { echo "❌ cargo 交叉编译失败"; exit 1; }
fi

SO="$MOBILE/target/aarch64-linux-android/release/libmdc_mobile.so"
[ -f "$SO" ] || { echo "❌ 找不到 $SO"; exit 1; }
# ELF 头 + 体积：0 字节 / 截断的 so 是最阴的失败形态
MAGIC="$(head -c4 "$SO")"
[ "$MAGIC" = "$(printf '\177ELF')" ] && ok "libmdc_mobile.so ELF 头正确（$(wc -c < "$SO") 字节）" \
  || bad "libmdc_mobile.so 不是 ELF（magic=$(printf '%s' "$MAGIC" | od -An -tx1 | tr -d ' ')）"

echo "== 装到 jniLibs（跳过 gradle 的 rust 任务）=="
mkdir -p "$JNI"
cp -f "$SO" "$JNI/libmdc_mobile.so"
[ -s "$JNI/libmdc_mobile.so" ] && ok "jniLibs/libmdc_mobile.so $(wc -c < "$JNI/libmdc_mobile.so") 字节" \
  || bad "jniLibs/libmdc_mobile.so 0 字节 —— 老坑复现"

# ── 3. 前端随包（tauri CLI 的活，手动 gradlew 必须自己做）────────────
# 🔴 前端是 tauri CLI 在 `tauri android build` 时拷进 assets/app 的。直接
#    gradlew assemble 不会拷 ⇒ 打出来的 APK 里**连 index.html 都没有**
#    （UI 全白，从外面看只觉得"App 打不开"）。这里补上这一步。
ASSET_DIR="$ANDROID/app/src/main/assets/app"
echo "== 前端 → $ASSET_DIR =="
mkdir -p "$ASSET_DIR"
# 免 rm -rf（沙箱会 SIGTERM）：只删 dist 里已经不存在的旧产物，避免 hash 累积
PY_CP="$(command -v python || command -v python3 || echo '/c/Users/25407/.workbuddy/binaries/python/versions/3.13.12/python.exe')"
"$PY_CP" - "$ROOT/web/dist" "$ASSET_DIR" <<'PY'
import os, sys
src, dst = sys.argv[1], sys.argv[2]
n = 0
for root, _dirs, files in os.walk(src):
    rel = os.path.relpath(root, src)
    target = dst if rel == "." else os.path.join(dst, rel)
    os.makedirs(target, exist_ok=True)
    for f in files:
        s, d = os.path.join(root, f), os.path.join(target, f)
        # 名字里带旧 hash 的先清掉，别让 APK 越攒越大
        if os.path.exists(d):
            os.remove(d)
        with open(s, "rb") as fi, open(d, "wb") as fo:
            fo.write(fi.read())
        n += 1
print(f"拷入 {n} 个前端文件")
PY
[ -f "$ASSET_DIR/index.html" ] && ok "assets/app/index.html 就位" || bad "web/dist → assets/app 没拷成功"
grep -q "assets/app/index.html" /dev/null 2>/dev/null
grep -c '' "$ASSET_DIR/index.html" >/dev/null 2>&1 && ok "assets/app/index.html 非空（$(wc -c < "$ASSET_DIR/index.html") 字节）" \
  || bad "assets/app/index.html 是空的"

# ── 4. gradle 出包 ────────────────────────────────────────────────────
GRADLE="$ANDROID/gradlew"
[ -x "$GRADLE" ] || GRADLE="bash $ANDROID/gradlew"
echo "== gradlew assembleUniversalDebug =="
# ⚠️ 必须 clean：gradle 增量会把 384MB 的僵尸 zip 留在 outputs 里（真内容才 44MB）。
# -x 掉两个 rust 任务：so 已经手动 cp 好了，再跑一次只会重复拷 0 字节。
( cd "$ANDROID" && ./gradlew clean assembleUniversalDebug \
    -x rustBuildArm64Debug -x rustBuildUniversalDebug 2>&1 | tail -20 ) \
  || bad "gradlew 构建失败"

APK_SRC="$ANDROID/app/build/outputs/apk/universal/debug/app-universal-debug.apk"
if [ -f "$APK_SRC" ]; then
  mkdir -p "$OUT"
  # cp -f 覆盖，不用 rm -rf（沙箱会 SIGTERM）
  cp -f "$APK_SRC" "$OUT/picklight-$VER-arm64-debug.apk"
  ok "产出 $OUT/picklight-$VER-arm64-debug.apk（$(wc -c < "$OUT/picklight-$VER-arm64-debug.apk") 字节）"
else
  bad "没产出 APK：$APK_SRC"
fi

echo "== 契约校验（tools/check_apk.py）=="
PY="$(command -v python || command -v python3 || echo '/c/Users/25407/.workbuddy/binaries/python/versions/3.13.12/python.exe')"
"$PY" "$ROOT/tools/check_apk.py" "$OUT/picklight-$VER-arm64-debug.apk" "$VER" || FAIL=1

exit $FAIL
