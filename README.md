# 拾光 · PickLight

Yet another Movie Data Capture tool —— 一套 Rust 核心覆盖三端：

| 形态 | 说明 |
| --- | --- |
| 🐳 **Docker** | NAS/服务器部署，`docker/docker-compose.yml` 一键起 |
| 🪟 **Windows exe** | 单文件服务，双击即用（或 Tauri 桌面安装包） |
| 📱 **Android APK** | 完整引擎跑在手机本地，离线可用（Tauri 2） |
| 📦 **飞牛 fnOS（.fpk）** | 离线镜像随包带走，`docker load` 载入，NAS 上不联网不编译 |

架构与设计细节见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)。

> 项目原名 **MDC-RS**，2026-10 更名为 **拾光 · PickLight**（安卓包名
> `com.picklight.app`）。APK/桌面走「首次启动时自动把旧 `com.mdcrs.mobile`
> 数据目录（config / 数据库 / CD2 home）拷到新目录」，老机器升级不用重新配置。

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
| `MDC_NO_OPEN_BROWSER` | — | 设任意值就别自动开浏览器（Docker/无桌面环境） |
| `MDC_PROXY_KERNEL` | — | 代理内核路径；不设就按「exe 同目录 → PATH」探测 |

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
docker compose up -d        # http://NAS_IP:9208（服务名/镜像都叫 picklight）
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

## 飞牛 fnOS（.fpk）

打包 → 得到 `dist/picklight-<版本>.fpk`，在飞牛「应用中心 → 本地安装」里选这个文件装上就行：

```bash
bash tools/build_linux_cross.sh      # 交叉编译两个架构的静态 mdc-server（本机不需要 Docker/无需目标机）
bash fpk-tools/build.sh              # → dist/picklight-0.1.1.fpk（≈120 MB）
```

> 🔴 **离线镜像路线**：包里带的是**完整的 Docker 镜像归档**
> （`fpk/app/image/picklight-<版本>-<架构>.tar.gz`，amd64 / arm64 各一份），
> 安装时 `docker load -i` 直接载入，**不联网、不编译、不拉任何基础镜像**，装完即跑
> —— 老 NAS / 家里拉不动 `registry-1.docker.io` 也能装。
> 代价是包体从 ~270 KB 涨到 ~120 MB（主要就是 mihomo 内核）。

包里**没有**源码、也没有 `fpk/Dockerfile`：镜像里的 `mdc-server` 是在开发机上用
**zig 交叉编译**出来的真·静态 musl ELF（`PT_INTERP` 为空，scratch 镜像都跑得起来），
内核是预先下好的 `mihomo-linux-<arch>`。这条链的好处是：

- 打 fpk **不需要本机有 Docker，也不需要目标机是同一架构**（x86 开发机照样出 arm64 包）；
- 用户在 NAS 上永远看不到 `base name (...) should not be blank` 这类就地构建错误。

> 换机器 / 换架构打包时用 `bash tools/build_linux_cross.sh <amd64|arm64>`（默认两架构都出）；
> 内核缺失时按 `tools/build.sh` 里的提示重新从 mihomo release 拉对应架构的
> `mihomo-linux-<arch>-<版本>.gz` 放到 `build/kernel/` 下。

安装时向导会问四件事，装完自动写进 `config.toml`，进界面不用再填一遍：

| 向导项 | 说明 |
| --- | --- |
| CD2 挂载根目录 | 网盘目录**在容器里的**绝对路径，左右两侧必须一致；勾了 rslave 后 CD2 掉盘重挂容器能自动看到 |
| strm / NFO 输出目录 | 容器内固定落到 `/media`，映射到 NAS 上的目录给 Emby、Jellyfin 扫 |
| CD2 地址 / 端口 | HTTP 地址，别写 `127.0.0.1`（Emby 那边也要能访问到） |
| 访问端口 | 默认 9208，与 `manifest` / 桌面入口保持一致 |

**镜像载入失败**时（报错会直接打到 `TRIM_TEMP_LOGFILE` 并原样弹出）按文案走：
提示「磁盘剩余空间」就清一层 Docker 镜像腾地方（两个架构共约 100 MB）；
提示「找不到 docker / 载入失败」就到飞牛「Docker → 设置」确认 Docker 服务是开着的。
因为压根不走 registry，这里**不存在换镜像源这一说**，别去折腾加速地址。

> ⚠️ `cmd/common` 里选归档走的是 `picklight-*-<架构>.tar.gz` 通配（取版本号最大的一份）：
> 归档名带 fpk 版本号，而运行时脚本读不到版本号，写死文件名必然找不到。

> ⚠️ 容器起停（`docker compose up/down`）由本应用的 `cmd/*` 脚本自己管：
> 包里刻意**没有** `docker/docker-compose.yaml`、也没声明 `docker-project`。
> 否则应用中心会按约定路径去 `pull` 一个只存在于本地的 tag，
> 报出来的错是 `registry-1.docker.io ... context deadline exceeded`，跟真实原因完全无关。

### 改代码后重新出包

```bash
# 1) 改前端 → 必须先重新构建前端产物（build.rs 读的就是它）
cd web && npm run build && cd ..
# 2) 重新交叉编译静态二进制（改过 Rust 时；只改前端可跳过）
bash tools/build_linux_cross.sh
# 3) 重新打 fpk（默认走离线路线：自动重打镜像归档、再封包）
bash fpk-tools/build.sh
```

> 老版本是从 `fpk/app/src` 同步源码、在 NAS 上就地 `docker build` 的；
> 那个分支已经删了，源码目录也不再进包。`bash fpk-tools/build.sh --source`
> 保留着旧路径，仅作排障回退用。

`build.sh` 结尾会跑一遍**契约自检**（`fpk-tools/check_contract.py`）：不装到真机上，
用假 docker 在模拟的 `TRIM_*` 环境里把安装 / 改配置 / 升级 / 启停 / 卸载全跑一遍，
断言 compose 的端口与挂载、播种的 `config.toml`、脚本权限 0755 与 LF、manifest 与
桌面入口端口一致等三十来条 —— 「装不上」的头号死因全部机器验掉，不靠人眼。

## Windows exe（单文件版）

```bash
bash tools/package_exe.sh
# → dist/picklight-<版本>-windows-x64.zip
#   内含 PickLight.exe（前端已编进二进制，不需要 web-dist 目录）+ mihomo.exe（代理内核）
```

`--features embed-web` 会把 `web/dist` 在**编译期**打进 exe（build.rs 生成一张
资产表 + `include!` 进来），所以双击就跑、拷给别人也不缺文件：

```bash
cargo build --release -p mdc-server --features embed-web
```

- 内嵌前端优先于磁盘目录：真要热改 UI 不重新打包，直接给 exe 旁边的
  `web/dist` 目录（`MDC_WEB_DIST` 或当前目录下的 `web/dist`）。
- 启动后自动开浏览器（`MDC_NO_OPEN_BROWSER=1` 关掉）；只在本机地址监听时才开。
- 代理内核按「exe 同目录 → `MDC_PROXY_KERNEL` → PATH」探测，所以 zip 里
  带上 `mihomo.exe`；单独拷走 exe 不联网刮削仍然能用，只是内置内核找不着。
- 打包脚本会做**字节级校验**：grep exe 里的前端指纹串，防止 build.rs 用的
  是旧 `web/dist`（改完前端必须 `touch crates/mdc-server/build.rs` 才会重跑）。

## Android APK

完整引擎（刮削/整理/数据库/CD2 网盘 + mihomo 代理内核 + UI）全部跑在手机上，
产物是纯 arm64 的 debug APK，约 56MB。

```bash
bash tools/build_apk.sh          # 构建 + 契约校验 + dist/picklight-<版本>-arm64-debug.apk
bash tools/build_apk.sh --skip-build   # 只校验已有产物（快）
python tools/check_apk.py dist/picklight-<版本>-arm64-debug.apk <版本>
```

需要：Android SDK（`%LOCALAPPDATA%\Android\Sdk`）+ NDK + JDK 17 + `rustup target add aarch64-linux-android`。
脚本自己定位 JDK/SDK/NDK，本机 `JAVA_HOME`/`ANDROID_HOME` 默认是空的。

**内置的三根内核**（`jniLibs/arm64-v8a/`，共 ~129MB）：`libclouddrive.so`（CloudDrive2
官方安卓引擎，端口 19798）、`libmihomo.so`（代理内核）、`libmdc_mobile.so`（本 App 的 Rust 壳）。
它们都在 `app/.gitignore` 里，**不进版本库**（换机器重建前从参考工程抽出来放进 `jniLibs/` 即可）。

**三条踩过的坑**（`tools/build_apk.sh` 里已固化）：
1. tauri CLI 会把 `.so` 往 jniLibs 拷成 **0 字节** ⇒ 脚本手动 `cp` 并跳过 gradle 的 rust 任务
   （`-x rustBuildArm64Debug -x rustBuildUniversalDebug`）；APK 内 `.so` 为 0 字节 = 装上秒崩。
2. **手动 `gradlew assemble` 不会拷前端**：前端是 tauri CLI 在 `tauri android build` 时放进
   `assets/app/` 的，直接 gradlew 打出来的 APK **连 index.html 都没有**（UI 全白，外面看只是"打不开"）。
   脚本补上了这一步（`web/dist → app/src/main/assets/app/`）。
3. 全局 cargo config 的 `[env] CC=w64devkit gcc` 会污染 aarch64 交叉编译 ⇒ 显式指定 NDK 的 `CC`/`AR`。

校验是字节级的（`tools/check_apk.py`）：三个 `.so` 必须 ELF 且体积达标、`assets/app/index.html`
在包里、**包内前端 hash 与当前 `web/dist` 一致**（防打旧前端）、`applicationId`/标题与配置对齐；
`app 标题` 这条只能靠 `aapt2 dump badging` 读（二进制 manifest 里 grep 不到，标题其实在
`res/values/strings.xml`）。

本机验证：adb 装包 → 冷启截图 → `adb forward tcp:19208 tcp:9208` → `/api/health` 返回
`{"ok":true,"version":"..."}`，设置页显示「内置 CD2 引擎 · 运行中」。

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

> 🔴 APK 端不能沿用「网盘挂在本机绝对路径」的假设 —— 安卓**没有 FUSE 挂载**。
> 网盘目录源已抽象成 `DirSource`（`source.rs`）：本地挂载（LocalFs）与 CD2 WebDAV
>（`/dav`，安卓唯一可行路径）两种实现，刮削与 `.strm` 生成只认这个 trait。
> 同一份库在两种源之间切换，增量清单照样命中（manifest 的 key 是**网盘内路径**）。
> UI 上有「目录源」区块与「试连」按钮，能立刻看到网盘路径与直链样例。

## 网盘刮削（115 等）

网盘挂到本地后直接给 Emby 扫库有两个麻烦：扫库要逐个读文件头取时长（几百 G 流量、
几小时等待、还容易被网盘风控），元数据匹配率也低。`.strm` 是几十字节的文本、
里面写播放地址，Emby 扫到它只读文本、不碰视频 —— 扫库从几小时变成几分钟。

拾光 **不自己实现网盘协议**，而是读 CloudDrive2 挂载出来的目录，
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
