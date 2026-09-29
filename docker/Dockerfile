# ---- 前端构建 ----
FROM node:22-slim AS web
WORKDIR /build
# 只先拷依赖清单：依赖没变时这一层能命中缓存。
# ⚠️ 必须先拷 package-lock.json —— 用 `npm ci` 需要它，而且能保证可复现
#    （`npm install` 会顺手改 lock，容器里装出来的和本地不是一套）。
COPY web/package.json web/package-lock.json web/.npmrc ./
RUN npm ci
# 再拷源码。这里**依赖根目录的 .dockerignore** 把 web/node_modules 排除掉 ——
# 否则宿主机的 node_modules（Windows 上还是 win32-x64 的原生二进制）
# 会覆盖容器里刚装好的 Linux 版，esbuild/rollup 直接跑不起来。
COPY web/ ./
RUN npm run build

# ---- 后端构建 ----
FROM rust:1-slim AS build
WORKDIR /build
# pkg-config 是 sqlx/reqwest 之类构建脚本的常见依赖；reqwest 走 rustls 不需要 OpenSSL
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config \
    && rm -rf /var/lib/apt/lists/*
# 先只拷 workspace 清单（`shells/` 被 Cargo.toml 的 exclude 排除，不必拷进来）
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# 依赖编译占了这一步的绝大部分时间。想加速可以再引入「先拷假源码编依赖、
# 再拷真源码」的分层技巧，但那要求假文件与真实结构严格对齐、很脆；
# 这里先用最直白的写法，正确优先。
RUN cargo build --release -p mdc-server

# ---- 运行镜像 ----
FROM debian:bookworm-slim
# curl 只给 HEALTHCHECK 用；ca-certificates 是刮削走 HTTPS 必需的
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /build/target/release/mdc-server /usr/local/bin/mdc-server
COPY --from=web /build/dist /app/web/dist
ENV MDC_BIND=0.0.0.0:9208 \
    MDC_CONFIG_PATH=/config \
    MDC_WEB_DIST=/app/web/dist \
    TZ=Asia/Shanghai
VOLUME ["/config"]
EXPOSE 9208
# /api/health 是公开路由（不要求鉴权），适合做健康检查
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -fsS http://127.0.0.1:9208/api/health || exit 1
CMD ["/usr/local/bin/mdc-server"]
