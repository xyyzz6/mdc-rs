# 观影端搭建指南（刮削已完成，这里只接「看」）

mdc-rs 的分工：**刮削归 mdc-rs，观看归媒体服务器**。mdc-rs 不搬视频，只往
`strm.root` 写几十字节的 `.strm` 指针 + NFO + 海报；媒体服务器扫这个目录
就有海报墙，点开播放时按 strm 里的直链取流。

## 一、mdc-rs 这边要先有的东西

1. mdc-rs 跑起来（Docker / Windows exe / APK 任一），目录源指向你的 CloudDrive2：
   - NAS / 桌面：CD2 已把 115 挂成本机目录 → 选「本地挂载」，填挂载根（如 `/mnt/clouddrive` 或 `Z:\`）；
   - 安卓：选「WebDAV」，基址 `http://<CD2的IP>:19798/dav`。
2. `.strm 输出目录` 必须是**媒体服务器也能看到的路径**：
   - 全家桶都在 NAS 上 → 填 NAS 上的共享文件夹路径（如 `/vol1/1000/media/strm`）；
   - mdc-rs 跑在 Windows exe 上 → 输出目录填映射到 NAS 的网络盘（如 `Z:\mdc-strm`），
     让飞牛/Docker 挂同一个共享。
3. 网盘刮削页点「运行一轮」，产出长这样（命名模板可在配置里改）：

```
strm_root/
└─ MIDV-567/
   ├─ MIDV-567.strm          ← 指针（内容是一行 URL，指向 CD2 直链）
   ├─ MIDV-567.nfo           ← 已刮好的元数据
   ├─ MIDV-567.jpg           ← 封面
   └─ MIDV-567-poster.jpg    ← 海报（Jellyfin/Emby 命名约定）
```

## 二、路线 A：飞牛影视（fnOS 自带，零安装）

1. fnOS 桌面打开「影视」App → 设置 → 资料库 → 新建资料库；
2. 类型选「电影」，目录选 **strm 输出目录对应的共享文件夹**；
3. 元数据来源：**勾选/优先「使用本地 NFO」**（mdc-rs 已刮好，别让它再上网刮一遍，
   既慢又可能覆盖人工精选的结果）；
4. 扫描完成 → 海报墙出来，手机 / 电视装「飞牛影视」客户端登录同一账号即可观看；
5. 播放走向：客户端 → 飞牛影视读 strm 里的 URL → 请求 `<CD2的IP>:19798/static/http/...`
   → **CD2 必须在线**，流量经 CD2 出 115。播放设备要能访问 CD2 端口（默认 19798）。

> 注： strm 直链支持是飞牛影视的标配能力；若你的 fnOS 版本太老扫 strm 出不了片，
> 直接走下面路线 B。

## 三、路线 B：Docker Jellyfin（免费开源，全平台客户端）

在 NAS 上加一个 Jellyfin 容器（若已用 docker-compose 管理，可直接并进现有文件）：

```yaml
services:
  jellyfin:
    image: jellyfin/jellyfin:latest
    container_name: jellyfin
    network_mode: host          # 直链播放要能直连 CD2，host 网络最省事
    volumes:
      - ./jellyfin/config:/config
      - ./jellyfin/cache:/cache
      - /vol1/1000/media/strm:/media/strm:ro   # ← 改成你的 strm 输出目录
    restart: unless-stopped
```

1. `docker compose up -d`，浏览器开 `http://<NAS的IP>:8096` 完成初始化；
2. 控制台 → 媒体库 → 添加媒体库：类型「电影」，目录 `/media/strm`；
3. 元数据下载器：**只留「NFO」并置顶**，其余（TMDB 等）可以全关 —— 元数据 mdc-rs 已写好；
4. 图片：本地图片优先即可，`-poster.jpg` 的命名 Jellyfin 直接认；
5. 客户端：手机 / 电视盒子装 Jellyfin 官方 App，填 NAS 地址即可。

## 四、排坑（都实测过或命过）

- **出不了片先看三处**：① strm 文件内容是不是一条 URL（`cat` 一下）；
  ② CD2 在不在线（浏览器开 `http://<CD2的IP>:19798`）；③ 播放设备到 CD2 通不通。
- **别让两套刮削打架**：媒体服务器侧一律「本地 NFO 优先」，否则它上网重刮会把
  mdc-rs 的人工精选结果顶掉。
- **增量不重刮**：mdc-rs 有 manifest，已生成的会跳过；「强制全量重写」只在你改了
  模板/想刷新海报时用。
- **115 风控**：走 CD2 的 `/static/http/` 中转链路，不碰 115 老接口，这是最稳的姿势；
  不要自己解析 115 网页会话（msg_code 50028 那条老路）。
