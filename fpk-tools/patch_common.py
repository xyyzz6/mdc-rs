#!/usr/bin/env python3
"""把 fpk/cmd/common 从「NAS 就地 docker build」改造成「离线 docker load」。

本机 core.autocrlf=true，工作区里是 CRLF，用 Edit 工具按子串匹配会失败
（Edit 匹配到的是 CRLF 行），所以这里走 Python 定点替换 + 顺手转 LF。
"""
import pathlib
import sys

P = pathlib.Path(__file__).resolve().parents[1] / "fpk" / "cmd" / "common"
raw = P.read_bytes().decode("utf-8")
src = raw.replace("\r\n", "\n")

edits = []


def sub(old, new, name):
    global src
    if old not in src:
        print(f"  ⚠️ 未匹配到片段：{name}")
        sys.exit(2)
    src = src.replace(old, new, 1)
    edits.append(name)


# 1) render_compose 里去掉 BASE_IMAGE 取值与 RENDER_FORCE_BASE_IMAGE 分支
sub(
    '''    if [ -n "${RENDER_FORCE_BASE_IMAGE:-}" ]; then
        BASE_IMAGE="${RENDER_FORCE_BASE_IMAGE}"
    else
        w="$(wiz base_image)"; BASE_IMAGE="${w:-${BASE_IMAGE:-${DEFAULT_BASE_IMAGE}}}"
    fi

''',
    "",
    "render_compose 去 BASE_IMAGE 分支",
)

sub(
    '''    [ -z "${PORT}" ] && PORT="${DEFAULT_PORT}"
    [ -z "${BASE_IMAGE}" ] && BASE_IMAGE="${DEFAULT_BASE_IMAGE}"
''',
    '''    [ -z "${PORT}" ] && PORT="${DEFAULT_PORT}"
''',
    "render_compose 去 BASE_IMAGE 兜底",
)

# 2) compose 去掉 build 段（离线镜像不构建）
sub(
    '''        echo "    # 源码随之安装到 ${SRC_DIR}，镜像不存在时 docker compose 会就地构建"
        echo "    build:"
        echo "      context: \\"${SRC_DIR}\\""
        echo "      dockerfile: Dockerfile"
        echo "      args:"
        echo "        BASE_IMAGE: \\"${BASE_IMAGE}\\""
''',
    '',
    "compose 去 build 段",
)

# 3) 删掉基础镜像默认值与候选链（离线镜像不需要任何基础镜像）
sub(
    '''# 基础镜像默认走国内加速源：国内 NAS 直连 registry-1.docker.io 基本必然
# context deadline exceeded，默认值指向 Docker Hub 就是给用户埋雷。
DEFAULT_BASE_IMAGE="docker.m.daocloud.io/library/debian:bookworm-slim"

BASE_IMAGE_MIRRORS="
docker.m.daocloud.io/library/debian:bookworm-slim
docker.1ms.run/library/debian:bookworm-slim
docker.fnnas.com/library/debian:bookworm-slim
debian:bookworm-slim
"

''',
    '',
    "删 DEFAULT_BASE_IMAGE / BASE_IMAGE_MIRRORS",
)

sub(
    '''base_image_candidates() {
    printf '%s\\n' \\
        "$(wiz base_image)" \\
        "${BASE_IMAGE_WORKING:-}" \\
        ${BASE_IMAGE_MIRRORS}
}

''',
    '',
    "删 base_image_candidates",
)

# 4) save_conf 不再记 BASE_IMAGE
sub(
    '''        echo "BASE_IMAGE=\\"${BASE_IMAGE}\\""
        # 上一次真正构建成功的那个基础镜像，下次优先复用它
        echo "BASE_IMAGE_WORKING=\\"${BASE_IMAGE_WORKING:-}\\""
''',
    '',
    "save_conf 去 BASE_IMAGE",
)

# 5) ensure_image → 离线 load
old_ensure = src[src.index("ensure_image() {"):src.index("# 真正拉起容器")]
new_ensure = '''# 载入随包带来的离线镜像。
#   force=1（升级）时重新 load —— 镜像内容变了，必须覆盖旧 tag。
load_image() {
    local force="${1:-0}"

    docker_env

    if ! command -v docker >/dev/null 2>&1; then
        fail "系统里找不到 docker 命令 —— 本应用依赖飞牛的 Docker 服务。
请先在「应用中心」里装好 / 启用 Docker，再重试安装。"
    fi

    if [ ! -f "${IMAGE_TAR}" ]; then
        fail "离线镜像归档丢失：${IMAGE_TAR}。
这个归档是本应用自带的（随 fpk 安装），请卸载后重新装一次最新包。"
    fi

    if [ "${force}" != "1" ] && docker image inspect "${IMAGE}" >/dev/null 2>&1; then
        echo "[picklight] 离线镜像 ${IMAGE} 已存在，跳过载入。"
        return 0
    fi

    echo "[picklight] 载入离线镜像 ${IMAGE_TAR}"
    # 管道会吞退出码 ⇒ 先拿 rc 再展示
    local out rc
    out="$(docker load -i "${IMAGE_TAR}" 2>&1)"; rc=$?
    echo "${out}" | tail -6
    if [ ${rc} -ne 0 ] || ! docker image inspect "${IMAGE}" >/dev/null 2>&1; then
        fail "离线镜像载入失败（${IMAGE_TAR}）。
请到飞牛「Docker → 设置」确认 Docker 服务正常、磁盘有足够剩余空间（镜像约需 100MB），然后卸载重装本应用。"
    fi
    echo "[picklight] 离线镜像 ${IMAGE} 载入完成。"
    return 0
}

# 兼容旧调用点：ensure_image <force>
ensure_image() {
    load_image "${1:-0}"
}

'''
src = src.replace(old_ensure, new_ensure, 1)
edits.append("ensure_image → load_image")

# 6) 顶部注释更新
sub(
    """### 拾光 · PickLight —— 公用函数与变量
### 被 install_init / install_callback / config_callback / upgrade_callback /
### uninstall_* / main 统一 source，避免逻辑漂移。
""",
    """### 拾光 · PickLight —— 公用函数与变量
### 被 install_init / install_callback / config_callback / upgrade_callback /
### uninstall_* / main 统一 source，避免逻辑漂移。
###
### 🔴 离线镜像路线：镜像（alpine mini rootfs + 静态 mdc-server + mihomo）已随 fpk
### 打包，安装时 `docker load -i` 载入，**不联网、不编译、不拉任何基础镜像**。
""",
    "顶部注释",
)

sub(
    "# 镜像名（由 fpk-tools/build.sh 注入，必须与渲染出的 compose 里的 image 一致）",
    "# 镜像名（离线镜像 tar 里写的 RepoTags 就是它，compose 只写 image:，不再 build）",
    "IMAGE 注释",
)

P.write_bytes((src.replace("\n", "\n")).encode("utf-8"))
print(f"  ✅ 已应用 {len(edits)} 处改动：")
for e in edits:
    print("    -", e)
