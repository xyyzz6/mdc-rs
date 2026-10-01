#!/usr/bin/env python3
"""重写 fpk（tar.gz）：把 cmd/* 下的脚本权限位改成 0755。

Windows 上 fnpack 打出来的成员权限是 0666，在 Linux 上无法执行，
症状是安装时报 `Permission denied` 或 `bad interpreter`。

注意：本脚本**不改动 app.tgz 的字节内容**，因为它只重写 fpk 这一层的 tar 条目，
而 manifest 里的 checksum 算的是 app.tgz 这个文件本身的 md5，所以依然有效。
（但如果需要修 app.tgz **内部** 的权限，就必须重算 checksum，见 build.sh 的说明。）

另外支持顺手把 manifest 的 `desc` 换掉（**内嵌镜像版专用**）：两个包共用同一份
`fpk/manifest`，而标准版的 desc 写着「首次安装会在 NAS 上本地构建镜像，约需 2-5 分钟」——
那句话对 `.with-image` 包是**反的**（内嵌版存在的意义就是免掉这一步），
用户装完内嵌包看到这句会以为自己拿错了文件。
在这里改而不是改源文件，好处是**源 manifest 永远不动**，不会污染下一次标准版打包；
manifest 的 `checksum` 算的是 app.tgz 的 md5，改 desc 不影响它。

用法：fix_perm.py <path/to/xxx.fpk> [--desc "<新的 desc 文案>"]
"""
import argparse
import io
import os
import re
import sys
import tarfile

# fpk 顶层需要保持可执行的条目
EXEC_PREFIXES = ("cmd/",)


def patch_manifest_desc(blob: bytes, desc: str) -> bytes:
    """把 manifest 里的 `desc = ...` 换成新文案，**保留原有的等号对齐**。

    找不到 desc 行就报错退出 —— 静默不改会让人以为改成功了（那才是最坏的结果）。
    """
    text = blob.decode("utf-8")
    lines = text.splitlines()
    for i, ln in enumerate(lines):
        if re.match(r"^desc\s*=", ln):
            key = ln.split("=", 1)[0]           # 连同上面对齐用的空格一起保留
            lines[i] = f"{key}= {desc}"
            break
    else:
        raise SystemExit("[fix-perm] 错误：manifest 里找不到 desc 行，无法替换")
    out = "\n".join(lines)
    if text.endswith("\n"):
        out += "\n"
    return out.encode("utf-8")


def main(src: str, desc: str | None = None) -> None:
    if not os.path.isfile(src):
        print(f"[fix-perm] 找不到文件：{src}")
        sys.exit(1)

    tmp = src + ".tmp"
    with tarfile.open(src, "r:gz") as tin:
        members = tin.getmembers()
        blobs = {}
        for m in members:
            if m.isfile():
                f = tin.extractfile(m)
                blobs[m.name] = f.read() if f else b""

    if desc:
        if "manifest" not in blobs:
            print("[fix-perm] 错误：包里没有 manifest，无法改 desc")
            sys.exit(1)
        blobs["manifest"] = patch_manifest_desc(blobs["manifest"], desc)

    changed = 0
    with tarfile.open(tmp, "w:gz") as tout:
        for m in members:
            info = tarfile.TarInfo(m.name)
            info.mtime = m.mtime
            if m.isdir():
                info.type = tarfile.DIRTYPE
                info.mode = 0o755
                tout.addfile(info)
            elif m.issym() or m.islnk():
                info.type = m.type
                info.linkname = m.linkname
                info.mode = m.mode
                tout.addfile(info)
            else:
                want = 0o755 if m.name.startswith(EXEC_PREFIXES) else 0o644
                if m.mode != want:
                    changed += 1
                info.size = len(blobs[m.name])       # 改过 desc 后必须按新长度写
                info.mode = want
                tout.addfile(info, io.BytesIO(blobs[m.name]))

    os.replace(tmp, src)
    extra = "，并按内嵌版改写了 manifest 的 desc" if desc else ""
    print(f"[fix-perm] 已修正 {changed} 个条目的权限位（cmd/* -> 0755）{extra}: {src}")


if __name__ == "__main__":
    ap = argparse.ArgumentParser(add_help=True, description="重写 fpk：修权限位 / 改 manifest desc")
    ap.add_argument("fpk", help="要处理的 .fpk 路径")
    ap.add_argument("--desc", default=None,
                    help="替换 manifest 的 desc 文案（内嵌镜像版用；不给则只修权限位）")
    a = ap.parse_args()
    main(a.fpk, a.desc)
