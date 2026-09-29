# MDC-RS

Yet another Movie Data Capture tool —— 一套 Rust 核心覆盖三端：

| 形态 | 说明 |
| --- | --- |
| 🐳 **Docker** | NAS/服务器部署，`docker/docker-compose.yml` 一键起 |
| 🪟 **Windows exe** | 单文件服务，双击即用（或 Tauri 桌面安装包） |
| 📱 **Android APK** | 完整引擎跑在手机本地，离线可用（Tauri 2） |

架构与设计细节见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)。

## 快速开始（本机开发）

前置：Rust（windows-gnu 可用）+ Node 20+。

```bash
# 1. 前端
cd web && npm install && npm run build && cd ..

# 2. 后端（默认 127.0.0.1:9208，自动加载 web/dist）
cargo run -p mdc-server
```

打开 http://127.0.0.1:9208 即可：解析测试 → 扫描目录 → 运行整理。

环境变量：

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `MDC_CONFIG_PATH` | `./data` | 配置/数据库/密钥目录 |
| `MDC_BIND` | `127.0.0.1:9208` | 监听地址（Docker 里用 `0.0.0.0:9208`） |
| `MDC_WEB_DIST` | `./web/dist` | 前端静态文件目录 |
| `MDC_USERNAME` / `MDC_PASSWORD` | — | 两者都设才启用登录鉴权（只覆盖内存，不落盘） |

## 代理

刮削站在国内直连实测 `http_code=000`，必须有出网通道。配置里 `common.proxy`
填 `http://127.0.0.1:7890` 之类即可，**内网（192.168.\*、10.\*、172.16-31.\*、
\*.local、回环）默认直连** —— 代理不认识内网地址会直接回 404，而且现象是「时好时坏」，
极难查。要额外豁免就往 `common.no_proxy` 里加。

下一步是**内置代理内核**（自带 mihomo，用户只填订阅链接），设计见
[docs/PROXY.md](docs/PROXY.md)。

## 刮削源现状

两个内置源，**都需要代理**（国内直连实测 `http_code=000`），在 `[common] proxy` 里配
`socks5://127.0.0.1:10808` 之类。引擎按 `supports()` 分工，各管各的番号形态。

| | javbus | FC2（官方站） |
| --- | --- | --- |
| 管什么 | **有码 + 無碼**番号（`MIDV-567`、`HEYZO-1673`） | FC2（`FC2-PPV-4680562`） |
| 为什么需要它 | 主力站，字段最全；两个分区共用一套解析 | **javbus 不索引 FC2**（实测 0 结果），靠它补 |
| 抓到什么 | 有码：标题/演员/导演/时长/发行日期/制作商/标签/封面/预览图<br>無碼：标题/时长/发行日期/制作商/封面（**没有演员和预览图**，那是站点就没提供） | 标题/时长/上架时间/标签/封面/预览图/卖家（FC2 是素人，**没有演员**） |

搜索顺序是**先有码、再無碼**，命中即返回。無碼里的**日期式**番号（`080918_002`、
`091626-001`）和 **Tokyo-Hot**（`n1234`）现在都能认；`T28620` 这类含数字的厂牌前缀
走前缀表（目前只放了实测过的 `T28`）。

支持的番号形态：`MIDV-567`、`FC2-PPV-3141592`、`SSIS00424`、`T28620`/`T28-620`、
`080918_002`、`091626-001`、`n1234`。命名里的下划线不再是问题
（`MIDV-567_1080p`、`xxx_MIDV-567` 都能解析）。

**防刮错片**：javbus 的模糊搜索会把别的番号排前面（搜 `SSIS-4` 第一条是 `SSIS-984`），
所以只认**番号对得上**的候选，对不上就报错而不是硬贴；FC2 对不存在的 id 会返回
**HTTP 200 + 错误页**，也做了显式识别。

检测站点改版（默认跳过，手动跑）：

```bash
MDC_PROXY=socks5://127.0.0.1:10808 \
  cargo test -p mdc-core --test providers_live -- --ignored --nocapture
```

## Docker

```bash
cd docker
docker compose up -d        # http://NAS_IP:9208
```

**先验证再上线**（推荐）：镜像「构建成功」不代表能用 —— 最典型的坑是
前端静态文件没打进镜像 ⇒ 打开页面一片白，而构建日志里完全看不出来。

```bash
bash docker/verify.sh                    # 构建 + 一键验证，只碰临时目录
DOCKER="sudo docker" bash docker/verify.sh
```

它会在一次性临时目录里造一套假 CD2 挂载，逐项检查：健康检查、**index.html 与 JS bundle
都能取到（黑屏检查）**、挂载根可见、输出目录可写、扫描能算出 CD2 直链、
挂载根外的文件如实报错、输出卷双向可见、容器日志没有「web/dist 不存在」。

> ⚠️ **`.dockerignore` 是必需的，别删。** 没有它时构建上下文会把 `target/`（8 GB+）
> 一起发给 daemon，更要命的是 `COPY web/ ./` 会把宿主机的 `node_modules`
> 覆盖到容器的 Linux 环境上（Windows 上装的是 win32-x64 原生二进制），
> esbuild/rollup 直接跑不起来。

## Windows exe

```bash
cargo build --release -p mdc-server
# target/release/mdc-server.exe + web/dist 一起分发即可
```

## Android APK

完整引擎（刮削/整理/数据库/UI）全部跑在手机上，构建需要 Android SDK/NDK：

```bash
rustup target add aarch64-linux-android
cargo install tauri-cli --version '^2'
cd shells/mdc-mobile && cargo tauri android init && cargo tauri android build --apk
```

CI（`.github/workflows/release.yml`）在打 tag 时自动产出三端制品。

## 功能状态

- ✅ 番号解析（标准/FC2/紧凑/含数字厂牌前缀/無碼日期式/Tokyo-Hot，CD 分段，分辨率）
- ✅ 刮削引擎 + 优先级 + 结果合并 + 按 id 停用某个源
- ✅ 内置两个源：**javbus**（有码 + 無碼）+ **FC2 官方站**（补 FC2），选择器均已线上实测校准
- ✅ **多源人工精选**：各源结果不合并、原样列出，你挑一条；挑过之后该番号直接用它、不再刮削
- ✅ YAML 自定义刮削源（`data/providers/*.yaml`，含示例）
- ✅ 五种整理模式（硬链/复制/移动/软链/原地）+ 命名模板
- ✅ **网盘刮削（CD2 挂载 → `.strm` + NFO + 海报，视频不搬，增量跳过，可定时）**
- ✅ NFO 生成（Kodi/Emby 兼容）+ 海报下载裁剪（2:3 中心裁剪）
- ✅ 目录监控（实时/轮询双模式）
- ✅ Web UI + JWT 鉴权（每实例随机密钥）
- ⬜ **内置代理内核**（mihomo + 订阅链接，用户不再另开代理容器）—— 见 `docs/PROXY.md`
- ⬜ **APK 内置 CloudDrive2 引擎**（手机本地挂 115open，走 WebDAV 读目录）
- ⬜ AI 人脸定位裁剪 / 水印（v0.3）
- ⬜ 翻译 / FlareSolverr / Emby 联动 / SQLCipher（v0.4）

> 🔴 APK 端不能沿用「网盘挂在本机绝对路径」的假设 —— 安卓**没有 FUSE 挂载**，
> 所以网盘目录源会抽象成 `DirSource`（LocalFs / WebDAV 两种实现）。这是 APK 端
> 支持网盘的前提，也是 `docs/PROXY.md` 里最重要的一条架构影响。

## 网盘刮削（115 等）

网盘挂到本地后直接给 Emby 扫库有两个麻烦：扫库要逐个读文件头取时长（几百 G 流量、
几小时等待、还容易被网盘风控），元数据匹配率也低。`.strm` 是几十字节的文本、
里面写播放地址，Emby 扫到它只读文本、不碰视频 —— 扫库从几小时变成几分钟。

mdc-rs **不自己实现网盘协议**，而是读 CloudDrive2 挂载出来的目录，
按 CD2 直链格式写 `.strm`：

```toml
[netdisk]
host = "192.168.1.15"        # CD2 的 HTTP 地址（Emby 也要能访问到，别填 127.0.0.1）
port = 19798
mount_root = "/mnt/clouddrive"   # CD2 挂载根在本机的绝对路径
path_prefix = "115"              # 可选：网盘路径前缀

[strm]
root = "/media/mdc-strm"     # 输出目录，Emby 扫这里；留空 = <数据目录>/strm
jobs = ["/mnt/clouddrive/115/看剧"]   # 要监控的网盘目录，与「片源」解耦
recursive = true
interval_hours = 0           # 自动运行间隔（小时）；0 = 只手动触发
```

```bash
cargo run -p mdc-server    # 打开 http://127.0.0.1:9208 → 「网盘刮削」区块
```

> ⚠️ **为什么不直连 115 官方接口**：115 对第三方客户端有强风控 ——
> 网页会话能列目录但**取不到下载直链**（大文件报
> `msg_code 50028`「文件大小超出限制，请使用115电脑端下载」），
> App 扫码会话能取直链但限速绕不过。所以登录与取链交给 CD2 这类成熟网关。

**增量**：签名只跟源路径有关，命中跳过时**连刮削都不做** —— 零网盘流量、零刮削请求。
落点被删会自动补写。

**定时**：`interval_hours > 0` 就开启。进程不常驻也没关系 ——
`last_run` 落盘，启动时若已超一个周期会**立刻补跑一轮**。
手动触发和定时共用一把单飞锁，不会同时打网盘。

## 安全声明

鉴权密钥按安装实例随机生成、不存在免鉴权的文件读取接口、桌面端默认只绑
127.0.0.1——这些是对同类闭源工具（mdc-ng）逆向审查后确立的底线，详见架构文档。
