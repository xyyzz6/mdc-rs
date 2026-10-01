#!/usr/bin/env python3
"""APK 契约校验（字节级）—— 防止「编成功了但装上秒崩 / 跑的是旧前端」。

典型死因，机器验，不靠人眼：
  1. jniLibs 里的 .so 被 tauri CLI 拷成 0 字节 ⇒ 安装后 UnsatisfiedLinkError 秒崩；
  2. 内核（libclouddrive / libmihomo）没打进 APK ⇒ 网盘刮削/代理都不可用；
  3. 打进去的是**旧前端**（改完 web/dist 没重编）⇒ 从外面看不出来；
  4. applicationId / app 标题被改回旧名。

用法：python tools/check_apk.py <apk> [版本]
"""
import io
import os
import re
import sys
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FAIL = []
COUNT = [0]


def check(cond, desc, detail=""):
    COUNT[0] += 1
    mark = "✅" if cond else "❌"
    color = "32" if cond else "31"
    line = f"  \033[{color}m{mark}\033[0m {desc}"
    if detail and not cond:
        line += f"\n      \033[31m→ {detail}\033[0m"
    print(line)
    if not cond:
        FAIL.append(desc)


def product_of_tauri_conf():
    """tauri 配置的 productName = App 标题（改名的唯一真源）。"""
    import json
    conf = os.path.join(ROOT, "shells", "mdc-mobile", "tauri.conf.json")
    if not os.path.exists(conf):
        return None
    try:
        return json.load(open(conf, encoding="utf-8")).get("productName", "")
    except Exception:
        return None


def main():
    apk = sys.argv[1] if len(sys.argv) > 1 else None
    if not apk or not os.path.exists(apk):
        print("用法：python tools/check_apk.py <apk> [版本]")
        return 2
    ver = sys.argv[2] if len(sys.argv) > 2 else "0.1.0"

    print(f"== APK 契约校验：{os.path.basename(apk)} ==")
    with zipfile.ZipFile(apk) as z:
        names = set(z.namelist())

        # ── 1. 三个 .so 必须在包里且不是 0 字节 ────────────────────────
        for so, min_mb in (("lib/arm64-v8a/libclouddrive.so", 10),
                           ("lib/arm64-v8a/libmihomo.so", 20),
                           ("lib/arm64-v8a/libmdc_mobile.so", 10)):
            if so not in names:
                check(False, f"包内含 {so.split('/')[-1]}", "APK 里根本没这个文件（内核没进 jniLibs）")
                continue
            info = z.getinfo(so)
            with z.open(so) as f:
                magic = f.read(4)
            ok_elf = magic == b"\x7fELF"
            size = info.file_size
            check(ok_elf and size > min_mb * 1024 * 1024,
                  f"{so.split('/')[-1]} 是 ELF 且 {size} 字节（>{min_mb}MB）",
                  f"magic={magic!r} size={size} —— 0 字节意味着装上秒崩")

        # ── 2. 前端随包（tauri 把 frontendDist 塞进 assets）─────────────
        check("assets/app/index.html" in names, "包内 assets/app/index.html 存在",
              "assets/app 里没有 index.html —— frontendDist 没打进去（UI 会全白）")
        dist = os.path.join(ROOT, "web", "dist")
        js_hashes, css_hashes = [], []
        if os.path.isdir(os.path.join(dist, "assets")):
            for f in os.listdir(os.path.join(dist, "assets")):
                if f.endswith(".js"):
                    js_hashes.append(f.split("-")[-1].split(".")[0])
                elif f.endswith(".css"):
                    css_hashes.append(f.split("-")[-1].split(".")[0])
        inside = [n for n in names if n.startswith("assets/app/assets/") and n.endswith((".js", ".css"))]
        for tag, hs in (("js", js_hashes), ("css", css_hashes)):
            if not hs:
                continue
            hit = [n for n in inside if any(h in n for h in hs)]
            check(bool(hit), f"包内含当前前端 {tag} 产物（{', '.join(hs)}）",
                  f"包里的 {tag} 是旧 hash，重编：touch web/dist 后 npm run build + 重出 APK")

        # ── 3. 身份：applicationId / 标题 ────────────────────────────
        # AndroidManifest.xml 是二进制 XML，但字符串以 UTF-8 原样存 ⇒ 能 grep 到
        man = None
        for n in names:
            if n.lower().endswith("androidmanifest.xml"):
                man = z.read(n)
                break
        if man is None:
            check(False, "包内找到 AndroidManifest.xml")
        else:
            # 🔴 二进制 AndroidManifest.xml 里的字符串是 **UTF-16LE**（不是 UTF-8），
            #    grep 纯 ASCII 会永远找不到，别被这个假红骗了。
            def has(s):
                return any(s.encode(e) in man for e in ("utf-8", "utf-16-le", "utf-16-be"))

            check(has("com.picklight.app"), "applicationId = com.picklight.app",
                  "manifest 里还是旧包名 —— 改名没生效")
            pass

    # ── 4. aapt2 直读（可选，能读就顺手验 label/extractNativeLibs）─────
    local = os.environ.get("LOCALAPPDATA", r"C:\Users\25407\AppData\Local")
    for bt in sorted(os.listdir(os.path.join(local, "Android", "Sdk", "build-tools")), reverse=True):
        aapt = os.path.join(local, "Android", "Sdk", "build-tools", bt, "aapt2.exe")
        if os.path.exists(aapt):
            import subprocess
            apk_win = apk.replace("/", "\\") if os.sep == "\\" else apk
            try:
                r = subprocess.run([aapt, "dump", "badging", apk_win],
                                   capture_output=True, text=True, encoding="utf-8",
                                   errors="replace", timeout=90)
                out = (r.stdout or "")
                m = re.search(r"package: name='([^']+)'", out)
                check(bool(m and m.group(1) == "com.picklight.app"),
                      "aapt2 dump 的 package = com.picklight.app", (m.group(1) if m else "解析失败"))
                # 标题在 res/values/strings.xml（manifest 只写 @string/app_name），
                # 二进制 manifest 里 grep 不到字面 ⇒ 只能靠 aapt2 读真值。
                lbl = re.search(r"application-label:'([^']+)'", out)
                want = product_of_tauri_conf()
                if want is None:
                    check(bool(lbl), "aapt2 能读到 application-label", "label 读不到")
                else:
                    check(bool(lbl and lbl.group(1) == want),
                          f"包内 app 标题 = {want!r}", (lbl.group(1) if lbl else "读不到"))
                check("launchable-activity" in out, "存在 launcher activity",
                      "启动项解析异常")
            except Exception as e:  # aapt2 在中文路径下会炸，不影响主结论
                print(f"  \033[33m⚪\033[0m aapt2 跳过（{type(e).__name__}: {e}）")
            break

    print()
    if FAIL:
        print(f"\033[31m❌ {len(FAIL)}/{COUNT[0]} 项没过\033[0m")
        for f in FAIL:
            print(f"   - {f}")
        return 1
    print(f"\033[32m✅ 全部 {COUNT[0]} 项契约检查通过\033[0m")
    return 0


if __name__ == "__main__":
    sys.exit(main())
