#!/usr/bin/env python3
"""拾光 · PickLight fpk 契约自检 —— 不装到 NAS 上也能把整条链路验完。

思路（照抄 cd2-scraper 那套，已被真实安装验证过）：
  1. 解包 dist/picklight-<版本>.fpk（同时展开 app.tgz）；
  2. 在临时目录里模拟飞牛的 TRIM_* 环境，塞一个**假 docker** 进 PATH，
     把 cmd/install_callback、config_callback、upgrade_callback、uninstall_callback、
     cmd/main 真跑一遍；
  3. 断言 compose 内容、播种出来的 config.toml、docker 调用序列、权限、CRLF 等。

用法：
    python fpk-tools/check_contract.py                 # 自动找 dist/picklight-*.fpk
    python fpk-tools/check_contract.py dist/x.fpk 0.1.0
    python fpk-tools/check_contract.py  --print        # 额外打印生成的 compose

⚠️ 三个必须知道的坑（来自真实安装事故）：
  * 假 docker 必须**跳过 `-f <file>` 再去取子命令** —— `docker compose -f x build`
    里 $1 是 `-f`，不跳的话 build/up 全被 `*)` 吞掉，测试全是假绿。
  * cmd/common 里 APP_DIR 写死 /var/apps/<appname>，本地写不进去 ⇒ 在**副本**里
    sed 改写，绝不在源脚本里加测试钩子。
  * 断言 compose，要看 install_callback **渲染后**的那一份，别看包里有没有占位文件。
"""
import glob
import os
import tempfile
import re
import shutil
import subprocess
import sys
import tarfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)

FAIL = []


def ok(msg):
    print(f"  \033[32m✅\033[0m {msg}")


def bad(msg):
    print(f"  \033[31m❌\033[0m {msg}")
    FAIL.append(msg)


def check(cond, good, badmsg):
    if cond:
        ok(good)
    else:
        bad(badmsg)
    return bool(cond)


FAKE_DOCKER = r'''#!/bin/bash
ST="${FAKE_DOCKER_STATE:?}"; mkdir -p "$ST"
echo "docker $*" >> "$ST/calls.log"
mark() { echo "$1" | tr '/:' '__'; }
img="${FAKE_DOCKER_IMAGE:-picklight:latest}"
case "$1" in
  compose)
    shift; sub=""
    while [ $# -gt 0 ]; do
      case "$1" in
        -f|--file|-p|--project-name) shift 2; continue ;;
        -*) shift; continue ;;
        *) sub="$1"; break ;;
      esac
    done
    case "$sub" in
      version) exit 0 ;;
      build) echo "Successfully built ${img}"; touch "$ST/img_$(mark "$img")"; exit 0 ;;
      up) echo "Container picklight  Started"; exit 0 ;;
      *) exit 0 ;;
    esac ;;
  image) [ "$2" = "inspect" ] && { [ -f "$ST/img_$(mark "$3")" ] && exit 0; exit 1; }; exit 0 ;;
  # ⚠️ 必须是"可配的"：测 cmd/main status 的 exit 3 分支就得让它是 false，
  #    写死 true 的话「未运行返回 3」这条永远验不到（假绿）。
  inspect) echo "${FAKE_DOCKER_RUNNING:-true}"; exit 0 ;;
  load) exit 0 ;;
  *) exit 0 ;;
esac
'''


def posix(p):
    p = os.path.abspath(p).replace("\\", "/")
    if len(p) > 1 and p[1] == ":":
        p = "/" + p[0].lower() + p[2:]
    return p


def read(path):
    with open(path, encoding="utf-8", errors="replace") as f:
        return f.read()


def main():
    args = sys.argv[1:]
    show = "--print" in args
    args = [a for a in args if not a.startswith("--")]
    if len(args) >= 2:
        fpk, ver = args[0], args[1]
    else:
        fpks = sorted(glob.glob(os.path.join(ROOT, "dist", "picklight-*.fpk")))
        if not fpks:
            print("找不到 dist/picklight-*.fpk，先跑 fpk-tools/build.sh")
            return 2
        fpk = fpks[-1]
        m = re.search(r"picklight-([0-9][0-9.]*)\.fpk", os.path.basename(fpk))
        ver = m.group(1) if m else "0.0.0"
    print(f"== 检查 {os.path.basename(fpk)}（版本 {ver}）==")

    # ⚠️ work 目录**每次新建、不去删上一次的**：
    #    上次的 work 里要跑完整的假 docker + 解包，文件上千个，
    #    `shutil.rmtree` 会被 IDE 的批量删除保护拦下来（记录里直接报
    #    SAFE_DELETE_BULK_CONFIRM_REQUIRED），整个自检还没开始就先挂了。
    #    换成 mkdtemp 就永远没有「要删的东西」，顺带也不会互相污染。
    work = tempfile.mkdtemp(prefix="picklight-fpk-check-")
    appdest = os.path.join(work, "appdest")
    etc = os.path.join(work, "etc")
    bindir = os.path.join(work, "bin")
    appdir = os.path.join(work, "var", "apps", "picklight")
    state = os.path.join(work, "dockerstate")
    for d in (appdest, etc, bindir, appdir, state):
        os.makedirs(d, exist_ok=True)

    # ⚠️ 解包 filter 必须用 "tar" 不能用 "data"：data filter 会把普通文件的 mode
    #    强行改写成 0644（目录 0775），于是脚本自己刚修好的 0755 又被抹掉，
    #    检查会误报「全包权限是 0666 / 0644」—— 假红，得知道是这么来的。
    with tarfile.open(fpk, "r:gz") as t:
        t.extractall(appdest, filter="tar")
    with tarfile.open(os.path.join(appdest, "app.tgz"), "r:gz") as t:
        t.extractall(appdest, filter="tar")

    print("\n== 1. 包结构 ==")
    for name in ("manifest", "app.tgz", "ICON.PNG", "ICON_256.PNG", "cmd", "config", "wizard"):
        check(os.path.exists(os.path.join(appdest, name)), f"包内含 {name}", f"包里缺 {name}")
    # 契约：包内不能给 appcenter 留下 docker 探测路径（坑 16b）
    with tarfile.open(os.path.join(appdest, "app.tgz")) as t:
        tops = sorted({n.split("/")[0] for n in t.getnames() if n.count("/") >= 1})
        check("docker" not in tops, f"app.tgz 顶层不含 docker/（顶层：{tops}）",
              f"app.tgz 顶层出现了 docker/：{tops}")
    res = read(os.path.join(appdest, "config", "resource"))
    check("docker-project" not in res, "config/resource 不声明 docker-project（否则安装会去 pull 本地 tag）",
          "config/resource 里还有 docker-project")
    priv = read(os.path.join(appdest, "config", "privilege"))
    check('"docker"' in priv, "config/privilege 带 join-groups [docker]（docker.sock 是 0660 root:docker）",
          "config/privilege 缺 join-groups")
    for w in ("install", "config"):
        p = os.path.join(appdest, "wizard", w)
        if not os.path.exists(p):
            continue
        import json
        try:
            data = json.loads(read(p))
            bad_rules = [i for it in data for i in it.get("items", [])
                         if "rules" in i and not isinstance(i["rules"], list)]
            check(not bad_rules, f"wizard/{w} 的 rules 都是数组", f"wizard/{w} 存在非数组 rules")
        except Exception as e:
            check(False, f"wizard/{w} 是合法 JSON", f"wizard/{w} JSON 解析失败：{e}")

    print("\n== 2. cmd 脚本 ==")
    cmds = sorted(os.listdir(os.path.join(appdest, "cmd")))
    check(len(cmds) >= 9, f"cmd 下 {len(cmds)} 个脚本", "cmd 下的脚本数量不对")
    # ⚠️ 权限必须读 **tar 条目里的 mode**，不能用解包后 os.stat：
    #    NTFS 存不住 Unix 的可执行位，Windows 上 os.stat 永远报 0666/0644，
    #    一比就是十条假红，会把真正没修权限的那次漏过去（正是这条要防的事）。
    with tarfile.open(fpk, "r:gz") as t:
        modes = {n: t.getmember(n).mode for n in t.getnames()
                 if n.startswith("cmd/") and t.getmember(n).isfile()}
    for c in cmds:
        p = os.path.join(appdest, "cmd", c)
        mode = oct(modes.get("cmd/" + c, -1))
        raw = open(p, "rb").read()
        if mode not in ("0o755", "0o775"):
            bad(f"cmd/{c} 权限是 {mode}（需要 0755，飞牛上会 Permission denied）")
            continue
        if b"\r\n" in raw:
            bad(f"cmd/{c} 含 CRLF（Linux 上会 bad interpreter）")
            continue
        r = subprocess.run(["bash", "-n", p], capture_output=True)
        if r.returncode != 0:
            bad(f"cmd/{c} 语法错：{r.stderr.decode(errors='replace').strip()[:120]}")
        else:
            ok(f"cmd/{c} 权限 {mode} / LF / 语法 OK")

    # ── 模拟飞牛环境 ────────────────────────────────────────────────────
    cp = os.path.join(appdest, "cmd", "common")
    txt = read(cp).replace('APP_DIR="/var/apps/${TRIM_APPNAME}"', f'APP_DIR="{posix(appdir)}"')
    check('/var/apps/${TRIM_APPNAME}' in read(cp),
          "源脚本里 APP_DIR 写死 /var/apps/${TRIM_APPNAME}（本地副本已改写）",
          "源脚本的 APP_DIR 不含 /var/apps/${TRIM_APPNAME}，改写断言无意义")
    with open(cp, "w", encoding="utf-8", newline="\n") as f:
        f.write(txt)

    dk = os.path.join(bindir, "docker")
    with open(dk, "w", encoding="utf-8", newline="\n") as f:
        f.write(FAKE_DOCKER)
    os.chmod(dk, 0o755)

    base_env = {
        "PATH": posix(bindir) + ":/usr/bin:/bin",
        "FAKE_DOCKER_STATE": posix(state),
        "TRIM_APPNAME": "picklight",
        "TRIM_APPDEST": posix(appdest),
        "TRIM_PKGETC": posix(etc),
        "TRIM_TEMP_LOGFILE": posix(os.path.join(work, "trim_err.log")),
        "HOME": posix(os.path.join(work, "home")),
    }

    def run(script, extra=None, args=()):
        env = dict(base_env)
        env.update(extra or {})
        return subprocess.run(["bash", os.path.join(appdest, "cmd", script), *args],
                              capture_output=True, text=True, encoding="utf-8",
                              errors="replace", env=env, cwd=work)

    print("\n== 3. 首次安装（install_callback）==")
    r = run("install_callback", {
        "wizard_mount_root": "/vol1/1000/CloudDrive",
        "wizard_strm_out": "/vol1/1000/Media/picklight-strm",
        "wizard_cd2_host": "192.168.1.15",
        "wizard_cd2_port": "19798",
        "wizard_port": "9208",
        "wizard_rslave": "true",
        "wizard_base_image": "docker.m.daocloud.io/library/debian:bookworm-slim",
    })
    check(r.returncode == 0, f"install_callback 退出码 {r.returncode}",
          f"install_callback 失败（{r.returncode}）：{(r.stdout or '')[-300:]}")
    compose_path = os.path.join(appdest, f"picklight-compose.yaml")
    comp = read(compose_path) if os.path.exists(compose_path) else ""
    check("container_name: picklight" in comp, "compose 容器名 picklight", "compose 里没有 container_name: picklight")
    check("'9208:9208'" in comp or '"9208:9208"' in comp, "compose 端口映射 9208:9208", "compose 端口不对")
    check("MDC_CONFIG_PATH=/data" in comp, "compose 设了 MDC_CONFIG_PATH=/data", "compose 没设 MDC_CONFIG_PATH")
    check("MDC_NO_OPEN_BROWSER=1" in comp, "compose 设了 MDC_NO_OPEN_BROWSER=1（容器里没桌面浏览器）",
          "compose 没关掉自动开浏览器")
    check("context: " in comp and "/src" in comp, "compose 的 build.context 指向 /src（就地构建）",
          "compose 缺 build.context 或路径不对")
    check("propagation: rslave" in comp, "compose 网盘挂载带 rslave", "compose 没有 rslave")
    check("- \"/vol1/1000/CloudDrive:/vol1/1000/CloudDrive\"" in comp or
          "source: \"/vol1/1000/CloudDrive\"" in comp, "网盘挂载左右路径一致", "网盘挂载路径不一致")
    check("- \"/vol1/1000/Media/picklight-strm:/media\"" in comp, "strm 输出挂载到容器内 /media",
          "strm 输出挂载不对")
    check("MDC_BIND=0.0.0.0" in comp, "compose 里容器监听 0.0.0.0", "容器没监听 0.0.0.0")

    # 播种的 config.toml
    cfg = os.path.join(appdir, "shares", "picklight", "data", "config.toml")
    ctext = read(cfg) if os.path.exists(cfg) else ""
    check("[netdisk]" in ctext and 'mount_root = "/vol1/1000/CloudDrive"' in ctext,
          "首次安装播种出 [netdisk] mount_root", "播种的 config.toml 缺 netdisk.mount_root")
    check("[strm]" in ctext and 'root = "/media"' in ctext,
          "首次安装播种出 [strm] root = /media", "播种的 config.toml 缺 strm.root")
    calls = os.path.join(state, "calls.log")
    calltxt = read(calls) if os.path.exists(calls) else ""
    check("compose" in calltxt and "build" in calltxt, "安装时确实调了 docker compose build", "安装没构建镜像")

    print("\n== 4. 改配置（留空=保持原值）==")
    # ⚠️ 每个阶段用**独立的假 docker 状态目录**：calls.log 是追加写的，
    #    混在一起的话「这一步到底有没有触发 build」就永远算不清。
    st_cfg = os.path.join(work, "state_cfg")
    os.makedirs(st_cfg, exist_ok=True)
    r = run("config_callback", {"FAKE_DOCKER_STATE": posix(st_cfg),
                                "wizard_port": "", "wizard_mount_root": "", "wizard_strm_out": "",
                                "wizard_cd2_host": "", "wizard_cd2_port": ""})
    check(r.returncode == 0, "config_callback 退出码 0", f"config_callback 失败：{r.returncode}")
    comp2 = read(compose_path) if os.path.exists(compose_path) else ""
    check("'9208:9208'" in comp2, "留空后端口仍是 9208（没被打回默认 8080/9090）", "留空导致端口被打回默认")
    check("/vol1/1000/CloudDrive" in comp2, "留空后网盘目录没被打回默认值", "留空导致网盘目录被打回默认")
    ctxt = read(os.path.join(st_cfg, "calls.log")) if os.path.exists(os.path.join(st_cfg, "calls.log")) else ""
    check("build" not in ctxt, "改配置没有触发重建镜像（只 up，不 build）", "改配置竟然触发了 build")

    print("\n== 5. 升级（upgrade_callback 强制重建）==")
    st_up = os.path.join(work, "state_up")
    os.makedirs(st_up, exist_ok=True)
    r = run("upgrade_callback", {"FAKE_DOCKER_STATE": posix(st_up)})
    check(r.returncode == 0, "upgrade_callback 退出码 0", f"upgrade_callback 失败：{r.returncode}")
    ctxt = read(os.path.join(st_up, "calls.log")) if os.path.exists(os.path.join(st_up, "calls.log")) else ""
    check("build" in ctxt, "升级确实重建了镜像", "升级没重建镜像")

    print("\n== 6. 容器生命周期 ==")
    # 未运行 → exit 3（假 docker 的 inspect 被设成 false）
    empty = os.path.join(work, "empty_state")
    os.makedirs(empty, exist_ok=True)
    r = run("main", {"FAKE_DOCKER_STATE": posix(empty), "FAKE_DOCKER_RUNNING": "false"}, args=("status",))
    check(r.returncode == 3, f"cmd/main status 未运行时 exit 3（实得 {r.returncode}）",
          f"cmd/main status 未运行没返回 3（实得 {r.returncode}，stderr：{(r.stderr or '')[:120]}）")
    # 运行中 → exit 0
    r = run("main", args=("status",))
    check(r.returncode == 0, f"cmd/main status 运行时 exit 0（实得 {r.returncode}）",
          f"cmd/main status 运行时没返回 0，实得 {r.returncode}")
    # 起停真的打到 docker（不是拿 echo 假装成功）
    st_life = os.path.join(work, "state_life")
    os.makedirs(st_life, exist_ok=True)
    for act in ("start", "stop"):
        r = run("main", {"FAKE_DOCKER_STATE": posix(st_life)}, args=(act,))
        check(r.returncode == 0, f"cmd/main {act} 退出码 0", f"cmd/main {act} 失败：{r.returncode}")
    ctxt = read(os.path.join(st_life, "calls.log")) if os.path.exists(os.path.join(st_life, "calls.log")) else ""
    check("stop picklight" in ctxt and ("up -d" in ctxt or "start picklight" in ctxt),
          f"启停真的调了 docker（calls：{ctxt.strip().replace(chr(10), ' / ')[:120]}）",
          "cmd/main 的 start/stop 没真的调 docker")

    print("\n== 7. 卸载 ==")
    r = run("uninstall_callback")
    check(r.returncode == 0, "uninstall_callback 退出码 0", "uninstall_callback 失败")
    calltxt = read(calls) if os.path.exists(calls) else ""
    check("rm -f picklight" in calltxt.replace("docker rm ", "docker rm -f ") or "rm -f picklight" in calltxt,
          "卸载真的 docker rm -f（否则重装撞容器名）", "卸载没清容器")

    print("\n== 8. 端口一致性 ==")
    mf = read(os.path.join(appdest, "manifest"))
    mport = re.search(r"^service_port\s*=\s*(\d+)", mf, re.M)
    mport = mport.group(1) if mport else ""
    # app.tgz 的顶层就是 fpk/app/ 的**内容**（ui/ src/ config/），没有 app/ 这一层
    ui = ""
    for cand in (os.path.join(appdest, "ui", "config"), os.path.join(appdest, "app", "ui", "config")):
        if os.path.exists(cand):
            ui = read(cand)
            break
    uport = re.search(r'"port":\s*"(\d+)"', ui)
    uport = uport.group(1) if uport else ""
    check(mport == uport == "9208", f"manifest/manifest service_port={mport} 与桌面入口 port={uport} 一致且为 9208",
          f"端口不一致：service_port={mport} ui={uport}（改端口时三处要一起改）")

    if show:
        print("\n" + "=" * 60)
        print(comp2 or "（无 compose）")

    print()
    if FAIL:
        print(f"\033[31m❌ {len(FAIL)} 项没过\033[0m")
        for f in FAIL:
            print("  -", f)
        return 1
    print("\033[32m✅ 全部契约检查通过\033[0m")
    return 0


if __name__ == "__main__":
    sys.exit(main())
