#!/usr/bin/env bash
# 在 NAS 上验证 picklight 的 Docker 镜像**能不能真跑起来**。
#
#   bash docker/verify.sh                 # 构建并验证（镜像名 picklight:verify）
#   bash docker/verify.sh picklight:latest   # 换镜像名
#   MDC_VERIFY_PORT=19208 bash docker/verify.sh
#   DOCKER="sudo docker" bash docker/verify.sh     # docker 需要 sudo 时
#
# 设计原则：
#   * **全程只用一次性临时目录**（mktemp -d），不碰你的真实 config / strm / 媒体库；
#   * 跑完自动删容器（trap EXIT），失败也删；
#   * 每一步都有 ✅/❌，最后给出退出码 —— 有失败就把输出整段贴回来。
#
# 为什么必须真跑：镜像「构建成功」不代表能用。最典型的坑是
# **前端静态文件没打进镜像 ⇒ 打开页面一片白**，构建日志里完全看不出来。

set -u

IMAGE="${1:-picklight:verify}"
DOCKER="${DOCKER:-docker}"
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
WORK="$(mktemp -d)"
PORT="${MDC_VERIFY_PORT:-19208}"
CONTAINER="picklight-verify-$$"
FAIL=0

ok()   { printf '  \033[32m✅\033[0m %s\n' "$1"; }
bad()  { printf '  \033[31m❌\033[0m %s\n' "$1"; FAIL=1; }
step() { printf '\n== %s ==\n' "$1"; }
cleanup() { $DOCKER rm -f "$CONTAINER" >/dev/null 2>&1 || true; }
trap cleanup EXIT

# 宿主机没 curl 就退到「用容器里的 curl」（镜像里装了 curl）
CURL_IN_CONTAINER=0
command -v curl >/dev/null 2>&1 || CURL_IN_CONTAINER=1

# req <GET|POST> <path> [json_body]
req() {
  local method="$1" path="$2" body="${3:-}"
  if [ "$CURL_IN_CONTAINER" = "1" ]; then
    if [ "$method" = "POST" ]; then
      $DOCKER exec "$CONTAINER" curl -fsS -X POST \
        -H 'Content-Type: application/json' -d "${body:-{\}}" \
        "http://127.0.0.1:9208$path" 2>/dev/null
    else
      $DOCKER exec "$CONTAINER" curl -fsS "http://127.0.0.1:9208$path" 2>/dev/null
    fi
  else
    if [ "$method" = "POST" ]; then
      curl -fsS -X POST -H 'Content-Type: application/json' -d "${body:-{\}}" \
        "http://127.0.0.1:$PORT$path" 2>/dev/null
    else
      curl -fsS "http://127.0.0.1:$PORT$path" 2>/dev/null
    fi
  fi
}

step "0. 前置检查"
if ! command -v "${DOCKER%% *}" >/dev/null 2>&1; then
  echo "找不到 docker（可用 DOCKER=\"sudo docker\" 重跑）"; exit 2
fi
if ! $DOCKER info >/dev/null 2>&1; then
  echo "docker daemon 不可用（权限或未启动）"; exit 2
fi
ok "docker 可用：$($DOCKER --version)"
[ "$CURL_IN_CONTAINER" = "1" ] && echo "  （宿主机没有 curl，改用容器内的 curl 做 HTTP 检查）"

step "1. 构建镜像"
if $DOCKER build -f "$HERE/Dockerfile" -t "$IMAGE" "$ROOT"; then
  ok "镜像构建成功：$IMAGE"
else
  bad "镜像构建失败"; exit 1
fi

step "2. 造一次性靶子（$WORK）"
mkdir -p "$WORK/config" "$WORK/strm" "$WORK/mount/115/看剧" "$WORK/mount/other"
printf 'x' > "$WORK/mount/115/看剧/MIDV-567 1080p.mp4"
printf 'x' > "$WORK/mount/115/看剧/SSIS-424.mp4"
printf 'x' > "$WORK/mount/115/看剧/readme.txt"   # 非视频，应被过滤掉
printf 'x' > "$WORK/mount/other/ABP-123.mp4"     # 在挂载根之外，直链应算不出
cat > "$WORK/config/config.toml" <<'TOML'
[scrape]
# 关掉两个内置源 → 全程离线，不碰 javbus / FC2
disabled = ["javbus", "fc2"]

[netdisk]
host = "192.168.1.15"
port = 19798
mount_root = "/mnt/clouddrive/115"
path_prefix = "115"

[strm]
root = "/out"
recursive = true
jobs = ["/mnt/clouddrive/115/看剧", "/mnt/clouddrive/other"]
TOML
ok "靶子就绪（2 个视频 + 1 个 txt + 1 个挂载根外的视频）"

step "3. 启动容器"
if $DOCKER run -d --name "$CONTAINER" -p "127.0.0.1:$PORT:9208" \
  -v "$WORK/config:/config" \
  -v "$WORK/strm:/out" \
  -v "$WORK/mount:/mnt/clouddrive:ro" \
  "$IMAGE" >/dev/null; then
  ok "容器已启动（$CONTAINER）"
else
  bad "容器起不来"; exit 1
fi

step "4. 健康检查"
UP=0
for _ in $(seq 1 30); do
  if req GET /api/health | grep -q '"ok":true'; then UP=1; break; fi
  sleep 1
done
if [ "$UP" = "1" ]; then ok "/api/health 正常"; else bad "/api/health 30 秒内没起来"; fi

step "5. 前端静态文件（黑屏检查 —— 最关键的一条）"
IDX="$(req GET /)"
if printf '%s' "$IDX" | grep -q 'id="root"'; then
  ok "index.html 已托管"
else
  bad "index.html 没托管 —— 镜像里漏了 web/dist？"
fi
ASSET="$(printf '%s' "$IDX" | grep -o 'assets/[^"]*\.js' | head -1)"
if [ -n "$ASSET" ]; then
  if [ "$CURL_IN_CONTAINER" = "1" ]; then
    BODY="$($DOCKER exec "$CONTAINER" curl -fsS "http://127.0.0.1:9208/$ASSET" 2>/dev/null | wc -c)"
    [ "${BODY:-0}" -gt 1000 ] && ok "JS bundle 可取（$ASSET，$BODY 字节）" || bad "JS bundle 取不到或过小（$BODY 字节）"
  else
    CODE="$(curl -s -o "$WORK/asset.js" -w '%{http_code}' "http://127.0.0.1:$PORT/$ASSET")"
    SIZE="$(wc -c < "$WORK/asset.js" 2>/dev/null || echo 0)"
    if [ "$CODE" = "200" ] && [ "$SIZE" -gt 1000 ]; then
      ok "JS bundle 可取（$ASSET，$SIZE 字节）"
    else
      bad "JS bundle 异常：HTTP $CODE，$SIZE 字节（页面会白屏）"
    fi
  fi
else
  bad "index.html 里找不到 assets/*.js 引用"
fi

step "6. 网盘接入状态"
ST="$(req GET /api/netdisk)"
printf '%s' "$ST" | grep -q '"mount_ok":true'    && ok "挂载根可见" || bad "挂载根不可见（volume 没挂对？）"
printf '%s' "$ST" | grep -q '"out_writable":true' && ok "输出目录可写" || bad "输出目录不可写"

step "7. 扫描预览（只扫不写）"
SC="$(req POST /api/strm/scan '{}')"
printf '%s' "$SC" | grep -q '"total":3'      && ok "扫到 3 个视频（txt 被正确过滤）" || bad "扫描数量不对：$(printf '%s' "$SC" | head -c 200)"
printf '%s' "$SC" | grep -q '%2F115%2F'      && ok "CD2 直链格式正确（斜杠已编码为 %2F）" || bad "直链格式不对"
printf '%s' "$SC" | grep -q '不在 CD2 挂载根下' && ok "挂载根外的文件如实报错" || bad "挂载根外没报错"

step "8. 输出卷双向可见"
$DOCKER exec "$CONTAINER" sh -c 'echo probe > /out/.probe' 2>/dev/null
if [ -f "$WORK/strm/.probe" ]; then ok "容器写的文件宿主机能看到（volume 通）"; else bad "volume 没通"; fi

step "9. 运行一轮"
RUN="$(req POST /api/strm/run '{"force":false}')"
if printf '%s' "$RUN" | grep -q '"total":3'; then
  ok "/api/strm/run 正常返回统计"
  echo "     统计：$(printf '%s' "$RUN" | tr -d '\n' | head -c 240)"
else
  bad "/api/strm/run 异常：$(printf '%s' "$RUN" | head -c 200)"
fi

step "10. 容器日志"
if $DOCKER logs "$CONTAINER" 2>&1 | grep -q "web/dist 不存在"; then
  bad "日志说「web/dist 不存在」—— 前端没打进镜像"
else
  ok "日志没有「web/dist 不存在」"
fi
echo "--- 最近 15 行 ---"
$DOCKER logs --tail 15 "$CONTAINER" 2>&1 | sed 's/^/    /'

step "结果"
if [ "$FAIL" = "0" ]; then
  printf '\033[32m✅ 全部通过 —— 镜像可以用了\033[0m\n'
else
  printf '\033[31m❌ 有失败项 —— 把上面整段输出贴回来\033[0m\n'
fi
exit "$FAIL"
