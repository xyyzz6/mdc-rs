"""把指定目录打成 ZIP_STORED 的 zip（单独一个文件，方便沙箱里跑）。

用法：python tools/zip_stored.py <目录> <输出的.zip>

为什么 ZIP_STORED：两个 exe（PickLight + mihomo）合起来 100MB 级，
deflate 在本机会跑好几分钟，而且实测会让调用方进程被 SIGTERM；
二进制本来就压不动，存档即可（GitHub Release 传 103MB 完全没问题）。
"""

import os
import sys
import zipfile


def main() -> int:
    src, out = sys.argv[1], sys.argv[2]
    names = sorted(n for n in os.listdir(src) if not n.endswith(".zip"))
    # 先写临时名再 rename：中途被打断不会留下半个 zip 污染 dist/
    tmp = out + ".tmp"
    z = zipfile.ZipFile(tmp, "w", zipfile.ZIP_STORED)
    try:
        for n in names:
            z.write(os.path.join(src, n), n)
    finally:
        z.close()
    if os.path.exists(out):
        os.remove(out)
    os.rename(tmp, out)
    print(f"zip: {out} ({os.path.getsize(out)} 字节, {len(names)} 个文件)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
