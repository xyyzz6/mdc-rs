#!/usr/bin/env bash
### 拾光 · PickLight —— 飞牛 fnOS .fpk 打包脚本
###
### 用法：
###   bash fpk-tools/build.sh                  # 标准版：不含镜像，安装时在 NAS 上就地构建（约 5-10 分钟）
###   bash fpk-tools/build.sh <镜像tag>         # 指定镜像名（默认 picklight:latest）
###
### 产物：dist/picklight-<版本>.fpk
###
### ⚠️ 本机没有 Docker 时也能打：fpk 里带的是**源码**，镜像交给 NAS 自己构建。
###    真要打「内嵌镜像版」才需要本机有 docker（本脚本目前只出标准版）。
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJ_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
FPK_DIR="${PROJ_ROOT}/fpk"
OUT_DIR="${PROJ_ROOT}/dist"
SRC_DIR="${FPK_DIR}/app/src"

IMAGE="${1:-picklight:latest}"
VER="${2:-}"

# fnpack：优先用仓库自带的，其次本机那一份，再不行就下载
FNPACK="${SCRIPT_DIR}/fnpack.exe"
if [ ! -f "${FNPACK}" ]; then
  FNPACK="${PROJ_ROOT}/dist/fnpack.exe"
fi
if [ ! -f "${FNPACK}" ]; then
    echo "[build] 下载 fnpack ..."
    curl -fsSL -o "${FNPACK}" \
      https://static2.fnnas.com/fnpack/fnpack-1.2.3-windows-amd64 \
      && chmod +x "${FNPACK}"
fi
[ -f "${FNPACK}" ] || { echo "错误：找不到 fnpack，也下载失败"; exit 1; }

# ── 0. 挑解释器 ─────────────────────────────────────────────────────────
# 本机裸 python 通常没装 Pillow，而 gen_icon.py 需要它画图；Windows 上
# `python` 还有可能是 Microsoft Store 的 shim（启动一个商店弹窗就卡住）。
# 所以这里逐个探：带 PIL 的优先，其次纯标准库那个（契约自检 / 修权限只用标准库）。
pick_py() {                       # 参数：需要不需要 PIL
    local need_pil="$1" c
    for c in "${PY:-}" \
             "${PROJ_ROOT}/.venv/Scripts/python.exe" \
             "${PROJ_ROOT}/.venv/bin/python" \
             "${HOME}/.workbuddy/binaries/python/envs/default/Scripts/python.exe" \
             "${HOME}/.workbuddy/binaries/python/envs/default/bin/python" \
             "$(command -v python3)" "$(command -v python)" ; do
        [ -n "${c}" ] && [ -x "${c}" ] || continue
        if [ "${need_pil}" = "pil" ]; then
            "${c}" -c "import PIL" >/dev/null 2>&1 || continue
        fi
        echo "${c}"; return 0
    done
    return 1
}
PY_IMG="$(pick_py pil || true)"
PY_BARE="$(pick_py bare || true)"
[ -n "${PY_BARE}" ] || { echo "错误：系统里找不到可用的 python3" >&2; exit 1; }

[ -n "${VER}" ] || VER="$(grep '^version' "${FPK_DIR}/manifest" | sed 's/.*=[[:space:]]*//' | tr -d '[:space:]')"
echo "[build] 应用版本 : ${VER}"
echo "[build] 镜像 tag : ${IMAGE}"

# ── 1. 同步源码进 fpk/app/src（单一源：永远从项目根拷，不手工维护第二份）──
echo "[build] 同步源码 -> fpk/app/src ..."
# ⚠️ 这一句的 rm 绝对不能"尽力而为"：`cp -r crates src/crates` 在目标已存在时
#    会往里再嵌一层（src/crates/crates/**），脏包一旦发出去很难查。
wipe_dir() {
    local d="$1" junk="${PROJ_ROOT}/_stale"
    [ -e "${d}" ] || return 0
    rm -rf "${d}" 2>/dev/null
    [ -e "${d}" ] || return 0
    mkdir -p "${junk}"
    mv "${d}" "${junk}/$(basename "${d}")-$(date +%s)" 2>/dev/null
    [ -e "${d}" ] || return 0
    echo "错误：${d} 删不掉也挪不走，请手动清理后再打包" >&2
    return 1
}
wipe_dir "${SRC_DIR}" || exit 1
mkdir -p "${SRC_DIR}"
cp "${FPK_DIR}/Dockerfile" "${SRC_DIR}/"
cp "${PROJ_ROOT}/Cargo.toml" "${PROJ_ROOT}/Cargo.lock" "${SRC_DIR}/"
cp -r "${PROJ_ROOT}/crates" "${SRC_DIR}/crates"
# ⚠️ 只拷 web/dist，**绝不拷整个 web**：node_modules 一进去 app.tgz 就多两千多个
#    条目、几十 MB（飞牛安装要解包，白白慢一大截），而 build.rs 唯一读的就是
#    <src>/web/dist。Docker 镜像构建同样只读 dist（见 fpk/Dockerfile 的说明）。
mkdir -p "${SRC_DIR}/web"
tar -cf - -C "${PROJ_ROOT}/web" dist index.html | tar -xf - -C "${SRC_DIR}/web"
# 编译期要用到的工具脚本（build.rs / 打包校验）
mkdir -p "${SRC_DIR}/tools"
cp "${PROJ_ROOT}/crates/mdc-server/build.rs" "${SRC_DIR}/tools/" 2>/dev/null
cp "${PROJ_ROOT}/tools/package_exe.sh" "${SRC_DIR}/tools/" 2>/dev/null
# 兜底自检：上面那个 rm 一旦没生效，这里就会长出嵌套目录 —— 当场拦下
for nested in "${SRC_DIR}/crates/crates" "${SRC_DIR}/web/web"; do
    [ -e "${nested}" ] || continue
    echo "错误：检测到嵌套目录 ${nested#${FPK_DIR}/}（源目录没清干净，cp -r 往里嵌了一层）" >&2
    exit 1
done
# ⚠️ 这几条同样是"删不掉就静默失败"，残留会直接进包（体积 + 污染上下文）。硬校验。
find "${SRC_DIR}" -type d -name target -prune -exec rm -rf {} + 2>/dev/null
_left="$(find "${SRC_DIR}" -type d -name target 2>/dev/null)"
[ -z "${_left}" ] || { echo "错误：源码目录里还有 target 残留：${_left}" >&2; exit 1; }

# ── 2. 图标 ─────────────────────────────────────────────────────────────
if [ ! -f "${FPK_DIR}/ICON.PNG" ]; then
    if [ -z "${PY_IMG}" ]; then
        echo "错误：找不到带 Pillow 的 python，也找不到现成图标" >&2
        echo "      修法二选一：venv 里装 Pillow（pip install Pillow），" \
             "或把生成好的 fpk/ICON.PNG + fpk/ICON_256.PNG 放进来跳过生成" >&2
        exit 1
    fi
    echo "[build] 生成图标 ...（${PY_IMG}）"
    ( cd "${SCRIPT_DIR}" && "${PY_IMG}" gen_icon.py ) || { echo "错误：图标生成失败"; exit 1; }
fi

# ── 3. fnpack 打包 ──────────────────────────────────────────────────────
mkdir -p "${OUT_DIR}"
echo "[build] fnpack build ..."
( cd "${FPK_DIR}" && "${FNPACK}" build -d "${FPK_DIR}" ) || { echo "错误：fnpack build 失败"; exit 1; }

# ── 4. 修权限 + 改名 ────────────────────────────────────────────────────
# ⚠️ fnpack 只认 `-d`，产出的文件名**永远是 <appname>.fpk**（这里是 picklight.fpk），
#    跟版本号无关；而且落在 working directory（= fpk/ 下）。要带版本就得自己搬。
# ⚠️ Windows 打出来的 cmd/* 权限是 0666，Linux 上执行不了（Permission denied / bad interpreter）。
BUILT="${FPK_DIR}/picklight.fpk"
FPK_OUT="${OUT_DIR}/picklight-${VER}.fpk"
if [ -f "${BUILT}" ]; then
    FPK_TMP="${OUT_DIR}/.picklight-${VER}.fpk"
    cp -f "${BUILT}" "${FPK_TMP}"
    ( cd "${SCRIPT_DIR}" && "${PY_BARE}" fix_perm.py "${FPK_TMP}" ) || { echo "错误：修权限失败"; exit 1; }
    mv -f "${FPK_TMP}" "${FPK_OUT}"
fi
[ -f "${FPK_OUT}" ] || { echo "错误：没产出 ${FPK_OUT}"; exit 1; }
echo "[build] 产物：${FPK_OUT}"

# ── 5. 契约自检（这几条是装不上的头号死因，机器验，不靠人眼）──────────
echo "[build] 契约自检 ..."
PY="${PROJ_ROOT}/fpk-tools/check_contract.py"
if [ -f "${PY}" ]; then
    "${PY_BARE}" "${PY}" "${FPK_OUT}" "${VER}" || { echo "错误：契约自检没过"; exit 1; }
fi

exit 0
