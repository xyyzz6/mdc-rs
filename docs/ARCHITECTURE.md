# MDC-RS 架构

> Movie Data Capture 工具（对标 mdc-ng），一套 Rust 核心库覆盖 Docker / Windows exe / Android APK 三端。

## 总体设计

```
┌─────────────────────────────────────────────────────┐
│                    mdc-core（核心库）                 │
│  parser   scrape    organize   monitor   nfo   db    │
│  番号解析  刮削引擎  文件整理   目录监控  NFO  SQLite   │
│  config  pipeline  image   cd2  strm                │
│                            网盘直链  .strm 指针        │
└────────────┬──────────────┬──────────────┬──────────┘
             │              │              │
      ┌──────┴─────┐  ┌─────┴──────┐  ┌────┴───────┐
      │ mdc-server │  │ mdc-desktop│  │ mdc-mobile │
      │ axum HTTP  │  │ Tauri 2 壳 │  │ Tauri 2 壳 │
      │ + 静态托管  │  │ → exe      │  │ → APK      │
      └──────┬─────┘  └─────┬──────┘  └────┬───────┘
             │              │              │
      ┌──────┴──────────────┴──────────────┴──────┐
      │            web/（Vite + React SPA）        │
      │      三端共用同一套界面（WebView 加载）       │
      └───────────────────────────────────────────┘
```

输入侧有两条路：**本地目录**（hard_link/copy/move/symlink/in_place）与
**网盘目录**（CD2 挂载 → 写 `.strm` 指针，见下文「网盘刮削」）。

关键决策：

1. **核心库不依赖任何 UI 框架**。所有业务（刮削、整理、监控、NFO）在 `mdc-core`，
   三个壳都是薄封装。
2. **前后端同端口**（默认 9208）。mdc-ng 是 Rust(9207) + Next.js(9208) 双进程，
   反代一层；我们用 axum 同时出 API 和静态文件，部署简单、移动端也好包。
3. **前端是纯 SPA**（Vite + React），不用 Next.js 服务端渲染——SSR 在手机壳里
   是负担而不是收益（mdc-ng 不得不在镜像里塞一个 Node 运行时）。
4. **自定义刮削源走 YAML**（`providers/*.yaml`）：声明式 CSS 选择器，用户不改代码
   就能适配新站点；站改版时社区可以互相发 yaml。

## 安全设计（mdc-ng 逆向发现的教训）

逆向 mdc-ng v1.36.0 发现两个实际问题，本项目从第一行代码就规避：

| mdc-ng 的问题 | mdc-rs 的做法 |
| --- | --- |
| JWT 验签密钥硬编码在产品里，所有实例相同，token 可伪造 | `config.rs::jwt_secret()` 首次启动随机生成 32 字节密钥，落盘 `data/jwt_secret`，每实例唯一 |
| `/server/image?path=任意路径` 免鉴权读任意文件（middleware matcher 排除了 server 前缀） | 所有读文件的接口必须在鉴权保护区内 + 路径限定媒体库白名单；无免鉴权的文件接口 |
| 后端 9207 裸奔，只靠不暴露端口保护 | 桌面/移动端默认绑 127.0.0.1；Docker 内绑 0.0.0.0 但仅容器网络可达，对外只有同端口的前端 |
| 数据库无加密 | 预留 `sqlcipher` 特性位（路线图 v0.4） |

## 模块说明

### parser.rs — 番号识别
按优先级依次尝试 6 类形态：

| 形态 | 例 | 说明 |
| --- | --- | --- |
| FC2 | `FC2-PPV-3141592` | 统一成 `FC2-PPV-<数字>` |
| 含数字的厂牌前缀 | `T28620` / `T28-620` | 查 `NUMERIC_PREFIXES` 表（`T28` 自带数字，通用形态认不出） |
| 带连字符标准 | `ABP-123` / `MIDV-567` | 字母 2-7 + 数字 2-5 |
| 紧凑 | `SSIS00424` | 字母 2-5 + 数字 3-6，**去前导零**；刻意不加 `(?i)`，否则 `video1080` 会被误当番号 |
| 無碼日期式 | `080918_002` / `091626-001` | `MMDDYY[-_]NNN`，**保留原分隔符**（与 javbus slug 一致）；校验月/日合法 |
| Tokyo-Hot | `n1234` | 字母 `n` + 4~5 位数字；优先级最低 |

同时解析 CD 分段、分辨率 token（720p/1080p/4k/8k）。

🔴 **边界一律不能用 `\b`，要用 `token_re` 的显式守卫。**
`_` 在正则里是**单词字符**，所以 `\b` 在 `MIDV-567_1080p` 的 `7` 与 `_` 之间**不成立** ——
`MIDV-567_1080p`、`xxx_MIDV-567`、`SSIS00424_1080p` 这些**极常见**的命名曾**全都解析不出来**。
`regex` crate 不支持 look-around，只能用 `(?:^|[^0-9A-Za-z])` / `(?:$|[^0-9A-Za-z])` 守卫。
（分辨率与分段的检测也是同一个坑，一并修了。注意守卫会吃掉两侧各一个字符，
所以 `replace_all` 要换成空格而不是空串。）

**厂牌前缀表纪律**：只放**实测过**的前缀。猜出来的前缀会把别的片子解析成**错的番号**，
而错番号会静默贴上别人的元数据 —— 比解析失败严重得多。
加一条之前先在 javbus 上搜 `<前缀>-<数字>`，确认 slug 就是这个形态。

### scrape/ — 刮削引擎
- `Provider` trait：`id()` + `label()` + `supports(number)` + `search(ctx, number)`；
- `Engine` 按 `scrape.priorities` 排序逐源调用，结果 `merge` 合并（首个非空字段优先，
  演员/标签取并集）；`supports()` 为 false 的源**直接跳过**，不发请求；
  `scrape.disabled` 可按 id 停用某个源；
- 内置源：`javbus`（**有码 + 無碼两个分区**）+ `fc2`（补 javbus 不索引的 FC2，官方站）；
- `custom.rs` 通用 YAML 引擎：`search.url` 模板 + `result_selector` 找详情页 +
  `detail.*` CSS 选择器提取字段，`@attr` 取属性。

**内置源的线上实测事实**（改之前先看）：

| | javbus | FC2（官方站） |
| --- | --- | --- |
| **代理** | 必须（直连 `http_code=000`） | 必须（同样 `000`） |
| 入口 | 有码 `/search/{番号}`；無碼 `/uncensored/search/{番号}` | `/article/{商品ID}/` |
| 拦截 | 搜索会 302 一次（设 cookie/年龄门），靠 `cookie_store` 跟随 | 无 |
| 覆盖 | 有码 + 無碼；**不索引 FC2**（两个分区搜 FC2 均为 0 结果） | 只有 FC2（素人，**没有演员**） |
| 坑 | **模糊搜索会把别的番号排前面**（搜 `SSIS-4` 第一条是 `SSIS-984`）；CD/日期变体 slug | 🔴 **不存在的 id 返回 HTTP 200 + 错误页**；🔴 **`<h3>` 标题被插反爬噪声 span，必须用 `og:title`/`ld+json`** |

**javbus 两个分区共用一份解析**：实测 `<h3>` / `a.bigImage` / `div.col-md-3.info` 结构一致，
**只有搜索 URL 前缀和「是否无码」不同**。無碼页天生字段更少 —— 没有演员（`star-div` 为空）、
没有预览图，封面在 `/imgs/cover/`（有码是 `/pics/cover/`）。
`uncensored` 标记**由分区决定**（無碼分区出来的必然无码），不靠标签里有没有「無碼」二字去猜。

搜索顺序是**先有码、再無碼**（有码内容多得多），命中即返回，所以大多数番号只多花一次搜索。
無碼里的**日期式** slug（`092426_001`、`091626-001`）当前 parser 认不出来，
会停在「无法识别番号」—— 那是 parser 的前缀表问题（v0.2），不是源的问题。

🔴 **必须校验番号**：
- javbus 只取「slug 与请求番号一致」的候选，一个都没有就**拒绝**（不拿第一条兜底），
  拿到详情页后再用页面上的 `識別碼` 复核一次；
- FC2 靠「有没有 `ld+json`」区分真实页与 200 错误页。

番号归一化会**抹平前导零**（`MIDV-012` ≡ `MIDV-12` ≡ `MIDV00567`）——
番号里的前导零只是排版，不抹平的话「请求紧凑形态、站点写标准形态」会被误判成不符而拒掉。

贴错元数据（错标题/错演员/错海报/错目录名）比失败严重得多，而且是**静默**的。

线上实测（默认 `#[ignore]`，一条命令发现站点改版）：
```bash
MDC_PROXY=socks5://127.0.0.1:10808 \
  cargo test -p mdc-core --test providers_live -- --ignored --nocapture
```

**实现注意**：`scraper::Html` 非 `Send`（内部 tendril 非原子），不能跨 `.await`
持有——先取回文本再同步解析，否则 async_trait 的 Send 约束编译不过。

### organize.rs — 六种整理模式
hard_link / copy / move / symlink / in_place / **strm**，前五种语义与 mdc-ng 对齐。
跨盘 move 自动退化 copy+delete；Windows symlink 提示需要开发者模式。
命名模板 `{number} {title}` 单括号风格。

⚠️ **模板里的 `/` 是结构、变量值里的 `/` 是数据**，两者必须分开处理：
清洗变量值（标题里的斜杠变空格）、保留模板自己的斜杠，`{actor}/{number} {title}`
才能真正分层。对整个渲染结果跑一次清洗，就只能生成一层目录。
每段按**字节**限长 200（单文件名上限 255 字节，中日文 3 字节/字）；
纯点段（`.` / `..`）一律丢弃 —— 标题是外部数据，不能让它穿越出输出根目录。

### cd2.rs — CloudDrive2 直链
`http://<host>:<port>/static/http/<internal>/False/<URL编码的网盘内绝对路径>`。
三个易错点：`/d/` 是 AList 的格式（用在 CD2 上必 404）；**斜杠也要编码**（`%2F`）；
网盘路径相对**挂载根**算。
`quote_path` 手写百分号编码，并有 `parity_with_python_quote` 测试与
`cd2-scraper`（Python）**逐字节对拍** —— 两个项目读同一个 CD2，编码差一个字符就是 404。

### strm.rs — `.strm` 指针 + 增量清单 + 单飞运行态
内容 = `[BOM] + url + \n`；走 `.part` 写完再 `rename`（网盘/SMB 抖动不留半份文件）。
manifest（`<数据目录>/strm_manifest.json`）记 `源路径 → (落点, 签名)`，
签名就是 URL；**不删旧条目**（挂载暂时掉线恢复后仍能命中跳过）。

`StrmRunner` 管运行态，两件事**分开持久化**：

| 状态 | 存哪 | 为什么 |
| --- | --- | --- |
| `running`（单飞令牌） | 只在内存 | 进程重启后就该是 false |
| `last_run` | `<数据目录>/strm_last_run` | **不能进 `config.toml`** —— 否则用户每改一次配置都会连带改掉时间戳，定时节奏被配置操作带偏 |

单飞令牌的释放**只靠 `Drop`**（RAII），不写手动的收尾代码 —— 漏一行就是
「第一轮看着全部正常、之后一切触发都返回 already-running」的静默失效。
失败**不记** `last_run`，否则一次配错要干等一个完整周期。

### 定时调度
`interval_hours = 0` 表示只手动触发。调度循环每轮重新读配置（改间隔立刻生效），
用 `sleep` 而不是固定 `interval` tick（一轮跑几十分钟时固定 tick 会背靠背连跑）。
**冷启动补偿**：进程不常驻，`last_run` 超一个周期或从没跑过就立刻补跑 ——
否则 24h 档永远赶不上。失败退避 10 分钟重试（而不是干等一个周期）。

### nfo.rs — Kodi/Emby/Jellyfin 兼容 NFO
minijinja 模板输出 `<movie>` XML，写入视频同名 `.nfo`。

### monitor.rs — 目录监控
`notify` crate 双模式：Performance（实时）与 Compatible（30s 轮询，网盘挂载用）。

### pipeline.rs — 处理管线
两条入口：

| 入口 | 流程 |
| --- | --- |
| `create_tasks_for_dir` + `process_pending` | 本地：扫描 → 建任务 → 解析番号 → 刮削 → 模板渲染 → 整理 → NFO+海报 → SQLite |
| `run_strm_jobs` | 网盘：扫网盘目录 → 解析番号 → 刮削 → 写 `.strm` + NFO + 海报（**视频不搬**）。带单飞令牌，手动触发与定时器共用 |

海报写两份：`<片名>.jpg`（Kodi 惯例）与 `<片名>-poster.jpg`（Jellyfin/Emby 最认）。
⚠️ 都是**按视频名**命名，绝不用目录级 `poster.jpg` —— 模板扁平化时同一目录会有多个视频，
目录级海报会互相覆盖。

## 网盘刮削（CD2 挂载 → `.strm`）

网盘挂到本地后直接给 Emby 扫有两个麻烦：扫库要逐个读文件头取时长（几百 G 流量 +
几小时等待 + 容易被风控）、元数据匹配率低。`.strm` 是几十字节的文本，里面写播放地址，
Emby 扫到它只读文本不碰视频 —— 扫库从几小时变成几分钟。

**为什么只有 `strm` 模式适用于网盘**：跨盘 hardlink 必然失败；
copy/move 等于真下载再上传（慢，且必然触发网盘风控）。

**为什么不用 mdc-rs 直连 115 协议**：115 的直链接口对第三方客户端有强风控 ——
网页会话能列目录但**取不到下载直链**（大文件报 `msg_code 50028`
「文件大小超出限制，请使用115电脑端下载」），App 扫码会话能取直链但限速绕不过。
所以 115 的登录与取链交给 **CloudDrive2**（或 OpenList 之类成熟网关），
mdc-rs 只读它暴露出来的挂载目录、按 CD2 直链格式写 `.strm`。

**增量语义**：签名只跟源路径有关，所以命中跳过时**连刮削都不做** ——
等于零网盘流量、零刮削请求。这是「反复重扫触发风控」的正解。
落点被删掉时会自动补写（`is_fresh` 同时校验落点文件是否还在）。

## 三端构建矩阵

| 目标 | 产物 | 构建方式 |
| --- | --- | --- |
| Docker | 镜像 <100MB | `docker/Dockerfile`（多阶段：node 构建 web → rust 构建 server → slim 运行）；**用 `docker/verify.sh` 验证** |
| Windows | mdc-server.exe（可双击，自动开浏览器）+ Tauri 安装包 | `cargo build --release -p mdc-server`；打包用 MSVC 工具链 |
| Android | APK | `cargo tauri android build --apk`，核心 cdylib 进 APK，引擎全本地 |

### Docker 构建的两个硬要求

1. **必须有 `.dockerignore`。** 没有它时构建上下文会把 `target/`（实测 8.8 GB）一起发给
   daemon；更要命的是 `COPY web/ ./` 会把宿主机的 `node_modules` 覆盖到容器的 Linux 环境上
   （Windows 上装的是 win32-x64 原生二进制）⇒ esbuild/rollup 跑不起来。
2. **前端要 `npm ci` 而不是 `npm install`**，并把 `package-lock.json` 一起拷进镜像 ——
   保证容器里装的依赖与本地是同一套。

**构建上下文的最小集合**（已在本机模拟验证过：只拷这些就能 `cargo check -p mdc-server`
与 `npm run build` 成功）：`Cargo.toml`、`Cargo.lock`、`crates/`、
`web/{package.json,package-lock.json,.npmrc,index.html,tsconfig.json,vite.config.ts,src/}`。
`shells/` 被 workspace 的 `exclude` 排除，**不必拷**。

**为什么镜像要单独验证**：镜像「构建成功」不代表能用。最典型的坑是前端静态文件没打进镜像
⇒ 页面一片白，而构建日志里完全看不出来（服务端只在 `web/dist` 缺失时打一行 WARN，
然后静默退化成「只提供 API」）。`docker/verify.sh` 里最关键的两条就是
「index.html 能取到」+「它引用的 JS bundle 也能取到且体积正常」。

## 数据布局

```
<数据目录>（默认 ./data，可用 MDC_CONFIG_PATH 覆盖）
├─ config.toml     # AppConfig
├─ jwt_secret      # 每实例随机生成（600 权限）
├─ mdc.db          # SQLite（WAL）
└─ providers/      # 自定义刮削源 *.yaml（含 _example.yaml）
```

## 路线图

- **v0.1（当前）**：骨架闭环——扫描/解析/刮削引擎/整理/NFO/WebUI/鉴权/三端构建
  + 网盘刮削（CD2 → `.strm`，增量 + 定时）
  + 番号前缀表与無碼日期式番号
- **v0.2**：**内置代理内核**（mihomo + 订阅）/ **网盘目录源抽象 `DirSource`**（✅ LocalFs + WebDAV
  已落地，`source.rs`；WebDAV 是 APK 端唯一可行的挂载方式，安卓没有 FUSE。manifest 增量
  以网盘内路径为 key，两种源之间切换不丢增量）/ 更多刮削源（javdb **需登录、不可用**）
- **v0.3**：AI 人脸定位裁剪（rustface + seetaface 模型）、水印、预览图
- **v0.4**：翻译（OpenAI/DeepL）、FlareSolverr、SQLCipher、Emby 联动
- **v0.5**：移动端后台监控（Android foreground service）、演员库（gfriends）
