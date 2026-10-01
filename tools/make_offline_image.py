#!/usr/bin/env python3
"""把「最小 rootfs + 静态 mdc-server + 代理内核」手工组装成一个 docker save 格式的镜像 tar。

为什么要手写而不是 docker build：
  fpk 装到飞牛上时**不能联网构建**（国内 NAS 拉 rust:1-slim / debian 必超时，
  用户看到的弹窗就是 `base name (${BASE_IMAGE}) should not be blank` 或
  `context deadline exceeded`）。所以要把做好的镜像直接打进 fpk，安装时
  `docker load -i` —— 全程零下载、零编译。

  docker save 的目录格式非常简单（纯标准库就能造）：
      <config_sha256>.json   镜像配置（architecture/os/config/rootfs.diff_ids/history）
      <layer_sha256>.tar     只读层（这里就是 rootfs 本身，未压缩）
      manifest.json          [{"Config":..,"RepoTags":["picklight:x"],"Layers":[..]}]

  ⚠️ diff_ids 是**未压缩层**的 sha256，docker load 会用它校验层内容；
     直接给未压缩 tar 就天然一致（压缩层要记 gzip 后的 sha256，别搞混）。

依赖产物（都应由 fpk-tools/build.sh / tools/build_apk.sh 准备好）：
  --rootfs    alpine miniROOTFS tar.gz（x86_64 / aarch64）
  --bin       Linux 静态 mdc-server（zig 交叉编译出来的 musl 二进制）
  --kernel    mihomo 的 Linux 二进制（可选；没有也能装，只是代理内核要自己放）
  --out       输出的 tar.gz（docker save 格式）

用法：
    python tools/make_offline_image.py --rootfs dl/alpine-....tar.gz --bin dist/mdc-server-x86_64 \\
        --kernel dl/kernel/mihomo-linux-amd64 --out fpk/app/image/picklight-0.1.0-amd64.tar.gz \\
        --arch x86_64 --version 0.1.0
"""
import argparse
import gzip
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time

SPECIAL_MODE = 0o755


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def base_img_url(arch):
    return "file://"


def make_layer_tar(rootfs_dir, extra_files, out_path):
    """把 rootfs + 额外文件打成一个**未压缩**的层 tar，返回该层的 sha256（diff_id）。"""
    with tarfile.open(out_path, "w", format=tarfile.GNU_FORMAT) as tf:
        for name in sorted(os.listdir(rootfs_dir)):
            p = os.path.join(rootfs_dir, name)
            tf.add(p, arcname=name, recursive=True)
        # 额外文件（二进制 / 内核 / 配置）以绝对路径放进层里
        for host_path, arc in extra_files:
            tf.add(host_path, arcname=arc.lstrip("/"), recursive=False)
    return sha256_file(out_path)


def build():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rootfs", default="",
                    help="可选：alpine miniROOTFS tar.gz；留空 = scratch（推荐，静态二进制不需要 C 库）")
    ap.add_argument("--bin", required=True, dest="binpath")
    ap.add_argument("--kernel", default="")
    ap.add_argument("--out", required=True)
    ap.add_argument("--arch", default="x86_64", choices=["x86_64", "aarch64"])
    ap.add_argument("--version", required=True)
    ap.add_argument("--tag", default="latest")
    ap.add_argument("--entry", default="/usr/local/bin/mdc-server")
    args = ap.parse_args()

    tmp = tempfile.mkdtemp(prefix="picklight-img-")
    work = os.path.join(tmp, "rootfs")
    os.makedirs(work)

    # ── rootfs：默认 scratch（空的）───────────────────────────────────────
    # ⚠️ mdc-server / mihomo 都是 **musl 静态**链接，运行期不需要 loader、不需要 C 库。
    #    所以 rootfs 可以完全是空的 —— 比 alpine mini rootfs 更小（省 ~3MB gzip）、
    #    也不依赖「能不能从国内镜像源拉到 alpine tarball」这种外部条件。
    #    docker 启动时会自己 bind mount /etc/resolv.conf /etc/hosts /etc/hostname，
    #    TLS 用的是 rustls 内嵌 webpki-roots（不是 rustls-native-certs），
    #    所以连 CA 证书目录都不需要。要调试/加 shell 时传 --rootfs 挂 alpine 即可。
    print(f"[1/5] rootfs：{'scratch（空）' if not args.rootfs else os.path.basename(args.rootfs)}")
    if args.rootfs:
        if args.rootfs.endswith(".xz"):
            subprocess.run(["tar", "-xJf", args.rootfs, "-C", work], check=True)
        else:
            subprocess.run(["tar", "-xzf", args.rootfs, "-C", work], check=True)

    # 静态镜像里手工补上最少的 passwd/group（避免 musl 在找不到 /etc/passwd 时
    # 走 nsswitch 反复失败；getpwuid 返回不回来会让一些库的报错信息变得很费解）
    for name, content in (("etc/passwd", "root:x:0:0:root:/data:/bin/sh\n"),
                          ("etc/group", "root:x:0:\n")):
        p = os.path.join(work, name)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        if not os.path.exists(p):
            with open(p, "w") as f:
                f.write(content)
    for d in ("/data", "/media", "/tmp"):
        os.makedirs(os.path.join(work, d.strip("/")), exist_ok=True)

    extra = []
    server_path = os.path.join(work, "usr/local/bin")
    os.makedirs(server_path, exist_ok=True)
    shutil.copy2(args.binpath, os.path.join(server_path, "mdc-server"))
    extra.append((args.binpath, "/usr/local/bin/mdc-server"))
    print(f"      mdc-server {os.path.getsize(args.binpath)} 字节 → /usr/local/bin/mdc-server")

    if args.kernel and os.path.exists(args.kernel):
        shutil.copy2(args.kernel, os.path.join(server_path, "mihomo"))
        extra.append((args.kernel, "/usr/local/bin/mihomo"))
        print(f"      mihomo {os.path.getsize(args.kernel)} 字节 → /usr/local/bin/mihomo")

    print("[2/5] 组装层 tar（未压缩）")
    layer = os.path.join(tmp, "layer.tar")
    diff_id = make_layer_tar(work, extra, layer)
    size_layer = os.path.getsize(layer)
    print(f"      层 {size_layer / 1e6:.1f} MB，diff_id=sha256:{diff_id[:16]}…")

    print("[3/5] 写 config.json")
    created = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    cfg = {
        "architecture": "amd64" if args.arch == "x86_64" else "arm64",
        "os": "linux",
        "created": created,
        "config": {
            "Env": [
                "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                "TZ=Asia/Shanghai",
                "MDC_CONFIG_PATH=/data",
                "MDC_BIND=0.0.0.0:9208",
                "MDC_NO_OPEN_BROWSER=1",
            ],
            "Cmd": [args.entry],
            "WorkingDir": "/data",
            "ExposedPorts": {"9208/tcp": {}},
            "Volumes": {"/data": {}, "/media": {}},
        },
        "rootfs": {"type": "layers", "diff_ids": [f"sha256:{diff_id}"]},
        "history": [{"created": created,
                     "created_by": "/bin/sh -c #(nop) ADD offline-image: "
                                   f"{args.version}/{args.arch} in / "}],
    }
    cfg_name = f"{diff_id}.json"
    with open(os.path.join(tmp, cfg_name), "w") as f:
        json.dump(cfg, f, indent=2)

    print("[4/5] 写 manifest.json")
    tag = f"picklight:{args.tag}"
    manifest = [{"Config": cfg_name,
                 "RepoTags": [tag],
                 "Layers": [f"{diff_id}.tar"]}]
    with open(os.path.join(tmp, "manifest.json"), "w") as f:
        json.dump(manifest, f)

    print(f"[5/5] 打包 → {args.out}")
    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    # 未压缩层 tar + 两个 json，整体 gzip（docker load 认 .tar.gz 里的 tar）
    with tempfile.NamedTemporaryFile(suffix=".tar", delete=False) as tmpf:
        tmpf.close()
        with tarfile.open(tmpf.name, "w") as tf:
            tf.add(os.path.join(tmp, cfg_name), arcname=cfg_name)
            tf.add(layer, arcname=f"{diff_id}.tar")
            tf.add(os.path.join(tmp, "manifest.json"), arcname="manifest.json")
        with open(tmpf.name, "rb") as src, gzip.open(args.out, "wb") as dst:
            shutil.copyfileobj(src, dst, 1 << 20)
    os.unlink(tmpf.name)

    size = os.path.getsize(args.out)
    print(f"      ✅ {args.out}（{size / 1e6:.1f} MB，tag={tag}）")
    print(f"      校验：diff_ids 与 config.rootfs 一致 = {diff_id}")
    if size > 400 * 1e6:
        print("      ⚠️ 镜像偏大，fpk 安装会慢（建议检查二进制是否strip过）")
    shutil.rmtree(tmp, ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(build())
