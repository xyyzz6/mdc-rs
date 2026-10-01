"""快速预览 fpk 安装后会生成什么 compose（不装到 NAS 上也能看）。

用法：
    python fpk-tools/preview_compose.py                     # 默认参数
    python fpk-tools/preview_compose.py --port 9090         # 覆盖某一项
    python fpk-tools/preview_compose.py --no-rslave         # 关掉 rslave

会解包 dist 下最新的 fpk 到 _preview_fpk/，用假 docker 跑一遍 install_callback，
然后打印生成的 docker-compose.yaml、应用配置和 docker 调用序列。
"""
import argparse
import glob
import os
import shutil
import subprocess
import sys
import tarfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
WORK = os.path.join(ROOT, "_preview_fpk")

FAKE_DOCKER = r'''#!/bin/bash
ST="${FAKE_DOCKER_STATE:?}"; mkdir -p "$ST"
echo "docker $*" >> "$ST/calls.log"
mark() { echo "$1" | tr '/:' '__'; }
img="picklight:latest"
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
  inspect) echo "true"; exit 0 ;;
  load) exit 0 ;;
  *) exit 0 ;;
esac
'''


def posix(p):
    p = os.path.abspath(p).replace("\\", "/")
    if len(p) > 1 and p[1] == ":":
        p = "/" + p[0].lower() + p[2:]
    return p


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--mount-root", default="/vol1/1000/CloudDrive")
    ap.add_argument("--strm-out", default="/vol1/1000/Media")
    ap.add_argument("--cd2-host", default="192.168.1.15")
    ap.add_argument("--cd2-port", default="19798")
    ap.add_argument("--port", default="9090")
    ap.add_argument("--base-image", default="docker.m.daocloud.io/library/debian:bookworm-slim",
                    help="与 wizard/install 里的默认值保持一致（国内加速源）")
    ap.add_argument("--no-rslave", action="store_true")
    args = ap.parse_args()

    fpks = sorted(glob.glob(os.path.join(ROOT, "dist", "picklight-*.fpk")))
    if not fpks:
        print("找不到 dist/picklight-*.fpk，先跑 fpk-tools/build.sh")
        sys.exit(1)

    if os.path.exists(WORK):
        shutil.rmtree(WORK)
    appdest = os.path.join(WORK, "appdest")
    etc = os.path.join(WORK, "etc")
    bindir = os.path.join(WORK, "bin")
    appdir = os.path.join(WORK, "var", "apps", "picklight")
    state = os.path.join(WORK, "dockerstate")
    for d in (appdest, etc, bindir, appdir, state):
        os.makedirs(d, exist_ok=True)

    with tarfile.open(fpks[-1], "r:gz") as t:
        t.extractall(appdest, filter="data")
    with tarfile.open(os.path.join(appdest, "app.tgz"), "r:gz") as t:
        t.extractall(appdest, filter="data")

    dk = os.path.join(bindir, "docker")
    with open(dk, "w", encoding="utf-8", newline="\n") as f:
        f.write(FAKE_DOCKER)
    os.chmod(dk, 0o755)

    # 把 APP_DIR 换到沙箱，免得往 /var/apps 里写
    cp = os.path.join(appdest, "cmd", "common")
    txt = open(cp, encoding="utf-8").read()
    txt = txt.replace('APP_DIR="/var/apps/${TRIM_APPNAME}"', f'APP_DIR="{posix(appdir)}"')
    with open(cp, "w", encoding="utf-8", newline="\n") as f:
        f.write(txt)

    env = {
        "PATH": posix(bindir) + ":/usr/bin:/bin",
        "FAKE_DOCKER_STATE": posix(state),
        "TRIM_APPNAME": "picklight",
        "TRIM_APPDEST": posix(appdest),
        "TRIM_PKGETC": posix(etc),
        "TRIM_TEMP_LOGFILE": posix(os.path.join(WORK, "trim_err.log")),
        "wizard_cd2_mount": args.mount_root,
        "wizard_media_out": args.strm_out,
        "wizard_cd2_host": args.cd2_host,
        "wizard_cd2_port": args.cd2_port,
        "wizard_port": args.port,
        "wizard_rslave": "false" if args.no_rslave else "true",
        "wizard_base_image": args.base_image,
    }
    r = subprocess.run(["bash", os.path.join(appdest, "cmd", "install_callback")],
                       capture_output=True, text=True, encoding="utf-8", errors="replace",
                       env=env, cwd=WORK)

    compose = os.path.join(appdest, "picklight-compose.yaml")
    conf = os.path.join(appdir, "shares", "picklight", "data", "config.json")
    calls = os.path.join(state, "calls.log")

    print(f"fpk：{os.path.basename(fpks[-1])}")
    print(f"install_callback 退出码：{r.returncode}")
    if r.stdout.strip():
        print("--- stdout ---")
        print(r.stdout.strip())
    if r.stderr.strip():
        print("--- stderr ---")
        print(r.stderr.strip())
    print("\n" + "=" * 68)
    print("生成的 docker-compose.yaml")
    print("=" * 68)
    print(open(compose, encoding="utf-8").read() if os.path.exists(compose) else "（未生成）")
    print("=" * 68)
    print("首次安装播种的 /data/config.json")
    print("=" * 68)
    print(open(conf, encoding="utf-8").read() if os.path.exists(conf) else "（未生成）")
    print("=" * 68)
    print("docker 调用序列")
    print("=" * 68)
    print(open(calls, encoding="utf-8").read().strip() if os.path.exists(calls) else "（无）")


if __name__ == "__main__":
    main()
