#!/usr/bin/env bash
### 组装「离线镜像归档」—— fpk 里带着一个 docker save 格式的镜像 tar，
### 安装时 `docker load -i`，全程**不联网、不编译、不拉基础镜像**。
###
### 产物：fpk/app/image/picklight-<版本>-<架构>.tar.gz
###
### 输入（都由本机交叉编译 / 抓取得到，见 fpk-tools/build.sh）：
###   build/linux-x86_64/mdc-server      静态 musl 二进制（zig 交叉编译）
###   build/linux-aarch64/mdc-server     同上，arm64
###   build/kernel/mihomo-linux-amd64   mihomo 代理内核（可选）
###   build/kernel/mihomo-linux-arm64
###
### 用法：
###   bash fpk-tools/build_offline_image.sh            # 能出的都出
###   bash fpk-tools/build_offline_image.sh amd64      # 只出 x86_64
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJ_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
VER="$(grep '^version' "${PROJ_ROOT}/fpk/manifest" | sed 's/.*=[[:space:]]*//' | tr -d '[:space:]')"
BUILD="${PROJ_ROOT}/build"
OUTDIR="${PROJ_ROOT}/fpk/app/image"
PY="${PROJ_ROOT}/tools/make_offline_image.py"
mkdir -p "${OUTDIR}"

pick_py() {
    local c
    # ⚠️ 必须按名字筛掉非解释器：这里的全局变量 PY 是 **组装脚本自身的路径**，
    #    `[ -x PY ]` 成立会让 PY_BARE 变成 "跑自己" —— 现象是
    #    `unrecognized arguments: .../make_offline_image.py`（脚本把自己的
    #    路径当成多余参数传给了 argparse）。
    for c in "${PY:-}" \
             "${HOME}/.workbuddy/binaries/python/envs/default/Scripts/python.exe" \
             "$(command -v python3)" "$(command -v python)"; do
        [ -n "${c}" ] && [ -x "${c}" ] || continue
        case "$(basename "${c}")" in
            python|python3|python*.exe|python3*.exe) echo "${c}"; return 0 ;;
            *) continue ;;
        esac
    done
    return 1
}
PY_BARE="$(pick_py || true)"

assemble() {   # assemble <arch: x86_64|aarch64> <server> [kernel]
    local arch="$1" server="$2" kernel="${3:-}" out="${OUTDIR}/picklight-${VER}-${1/x86_64/amd64}.tar.gz"
    if [ ! -f "${server}" ]; then
        echo "  ⚠️ 缺 ${arch} 的 mdc-server：${server}（跳过该架构）"; return 1
    fi
    local head; head="$(head -c4 "${server}")"
    local expect; expect="$(printf '\177ELF')"
    if [ "${head}" != "${expect}" ]; then
        echo "  ⚠️ ${arch} 的 mdc-server 不是 ELF（magic=$(printf '%s' "${head}" | od -An -tx1 | tr -d ' ')），跳过"; return 1
    fi
    local args=(--bin "${server}" --kernel "${kernel}" --arch "${arch}" --version "${VER}" --out "${out}")
    [ -s "${kernel}" ] && args+=(--kernel "${kernel}")
    echo "  → ${arch}"
    "${PY_BARE}" "${PY}" "${args[@]}" || return 1
    return 0
}

FAILED=0
if [ "${1:-}" = "amd64" ] || [ "${1:-}" = "arm64" ] || [ -z "${1:-}" ]; then
    if [ "${1:-}" = "amd64" ] || [ -z "${1:-}" ]; then
        assemble x86_64 "${BUILD}/linux-x86_64/mdc-server" "${BUILD}/kernel/mihomo-linux-amd64" || FAILED=1
    fi
    if [ "${1:-}" = "arm64" ] || [ -z "${1:-}" ]; then
        assemble aarch64 "${BUILD}/linux-aarch64/mdc-server" "${BUILD}/kernel/mihomo-linux-arm64" || FAILED=1
    fi
fi

echo "== 产物 =="
ls -la "${OUTDIR}" 2>/dev/null
exit 0
