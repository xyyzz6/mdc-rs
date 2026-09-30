# P4：自带影视库（一站式：海报墙 + 播放）

> 定位：不想装 Emby/Jellyfin/飞牛影视的用户，打开 mdc-rs 自带页面就是完整观影体验。
> 刮削层、代理层、目录源全部复用现有设施，**不加任何外部组件**。

## 目标形态

- **海报墙**：全部已刮削条目按封面网格排布（iOS 风格，延续 `web/src/app.css`）；
- **详情页**：标题 / 演员 / 标签 / 发行日期 / 无码标记 / 封面大图 + 文件列表；
- **播放**：点开即播，`<video>` 标签流式播放，支持进度条拖动（HTTP Range）；
- **继续观看**：记录每条播放进度，首页置顶「看到一半」的条目；
- **搜索 / 筛选**：番号 / 标题 / 演员 / 标签；按日期 / 番号排序；
- **多端同享**：web（Docker/exe）与 APK 是同一份前端，天然全端。

## 数据源（全部已有）

- manifest（增量清单）：`number → 云盘路径 / strm 落点`；
- `videos` 表：人工精选的完整 meta（VideoMeta：标题/演员/标签/封面/时长/日期）；
- strm 输出目录里已落盘的 `<名>.nfo` / `<名>.jpg` —— 海报直接读本地文件，不打 115。

## API 草案

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/library?query=&tag=&page=&sort=` | 条目列表（含海报 URL、是否有进度） |
| GET | `/api/library/{number}` | 详情：meta + 文件列表 + 可播 URL |
| GET | `/api/library/{number}/poster` | 海报图（鉴权区静态文件） |
| GET | `/api/library/{number}/play` | 播放入口（见下「流式策略」） |
| PUT | `/api/library/{number}/progress` | 上报播放进度 `{pos, dur}` |
| GET | `/api/library/continue` | 继续观看列表（有进度未播完） |

所有路由进**鉴权区**（沿用 JWT 中间件），海报/播放一样要过鉴权 —— 不重复
mdc-ng `/server/image?path=` 免鉴权任意读的老路。

## 流式策略（两档）

1. **默认：302 重定向**到 strm 里的 CD2 直链（`/static/http/...`）。
   优点：零拷贝、NAS CPU 无压力；前提：客户端能直连 CD2 端口（家庭内网天然成立）。
2. **兜底：Range 代理** `/api/library/{number}/stream`：服务端按 Range 转发直链
   （复用 `net.rs` 出网层 + 内网豁免）。适用：直链失效、客户端到 CD2 不通、
   或将来 APK 内置 CD2 引擎（19798 在本机回环）时的统一出口。
   实现要点：`reqwest` 透传 `Range` / `Content-Range` / `Accept-Ranges`，流式 body，
   不落盘；连接断开要 cancel 上游（`hyper` 的 `Body::stream` 取消语义）。

## 前端

- 新页签「影视库」（底部 tab bar 与现有 5 页签同体系，`app.css` 加 `.wall`/`.detail`
  等类，不动已有钩子类名）；
- 详情页 + 播放页：`<video controls preload="metadata" src="/api/library/{number}/play">`；
  APK WebView 硬解直链 mp4/mkv 没问题，**mkv 若遇到编码不兼容**再议转码（明确不做，
  播不动就提示用 Jellyfin 路线）；
- 进度上报：`timeupdate` 节流（5s）+ `pause`/`ended` 补报；继续观看 = 上次 pos/dur < 95%。

## 里程碑

- **P4a（能看）**：library 索引（manifest + videos 表合并视图）、列表/详情/海报、
  302 播放；web + APK 验证。
- **P4b（好用）**：进度记录 + 继续观看 + Range 代理兜底。
- **P4c（整齐）**：搜索/标签筛选/排序、按演员聚合、收藏。

## 依赖与风险

- 无新外部依赖；APK 端播放不受 DirSource 抽象影响（直链/代理都与本地挂载无关）；
- 115 直链时效：CD2 直链通常有时效，302 目标每次播放现算（不走缓存）；
- 多用户：先单用户（家庭自用），进度存本地 DB，不做账号体系。
