# 拾光 · PickLight —— 飞牛 fnOS 版**镜像构建用**的 Dockerfile。
#
# ⚠️ 这个文件是 fpk 工程自己的副本（源码来源 = 仓库根），不是 docker/Dockerfile：
#    fpk 里镜像是在**用户的 NAS 上就地构建**的，基础镜像默认必须走国内加速源
#    （国内 NAS 直连 registry-1.docker.io 基本必然 context deadline exceeded），
#    所以这里给最终阶段加了 `ARG BASE_IMAGE`，由向导 / 候选链逐源传入。
#
# 构建上下文 = fpk/app/src（contents：Cargo.toml Cargo.lock crates/ web/dist/ ）
#
# ⚠️ 这里**刻意不重编前端**：镜像是在用户的 NAS 上就地构建的，国内 NAS 再拉一次
#    npm 依赖既慢又不稳（npm ci 几百 MB），而前端产物 web/dist 本来就随 fpk 安装，
#    build.rs 直接把它编进二进制即可 —— 镜像体积小一个数量级，构建时间也短得多。
#    想改前端就重新打 fpk 包，不用重装镜像。

# ---- 后端构建（前端产物由 build.rs 内嵌）----
FROM rust:1-slim AS build
WORKDIR /build
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config \
    && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# build.rs 从这里读前端产物（路径 = crates/mdc-server/../../web/dist）
COPY web/dist ./web/dist
RUN cargo build --release --locked -p mdc-server --features embed-web

# ---- 运行镜像 ----
ARG BASE_IMAGE=docker.m.daocloud.io/library/debian:bookworm-slim
FROM ${BASE_IMAGE}
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /build/target/release/mdc-server /usr/local/bin/mdc-server
# 单文件服务：前端已编进二进制（embed-web），不需要再拷 web/dist
ENV TZ=Asia/Shanghai \
    MDC_CONFIG_PATH=/data \
    MDC_BIND=0.0.0.0:9208 \
    MDC_NO_OPEN_BROWSER=1
RUN mkdir -p /data /media
VOLUME ["/data", "/media"]
EXPOSE 9208
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD curl -fsS http://127.0.0.1:9208/api/health || exit 1
CMD ["/usr/local/bin/mdc-server"]
