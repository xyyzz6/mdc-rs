#!/usr/bin/env python3
"""生成「拾光 PickLight」飞牛应用图标（64x64 / 256x256）。

图形语义：渐变圆角底 + 白色云（网盘目录来源）+ 云里的播放三角（刮削出来的 strm 直链）+ 右上角闪光（刮削）。
输出到 fpk 工程的 4 个位置：
  fpk/ICON.PNG            fpk/ICON_256.PNG
  fpk/app/ui/images/icon_64.png   fpk/app/ui/images/icon_256.png

用法：python gen_icon.py
"""
import os

from PIL import Image, ImageDraw

HERE = os.path.dirname(os.path.abspath(__file__))
OUT_ROOT = os.path.abspath(os.path.join(HERE, "..", "fpk"))

SIZES = {"64": 64, "256": 256}

# 渐变配色：天空蓝 -> 靛蓝
TOP = (56, 189, 248)
BOTTOM = (37, 99, 235)
WHITE = (255, 255, 255, 255)


def lerp(a, b, t):
    return tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(3))


def build(size: int) -> Image.Image:
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))

    # ── 渐变圆角底 ──────────────────────────────────────────────
    radius = int(size * 0.24)
    grad = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    gd = ImageDraw.Draw(grad)
    for y in range(size):
        gd.line([(0, y), (size, y)], fill=lerp(TOP, BOTTOM, y / max(size - 1, 1)) + (255,))
    mask = Image.new("L", (size, size), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, size - 1, size - 1], radius=radius, fill=255)
    img.paste(grad, (0, 0), mask)
    d = ImageDraw.Draw(img)

    # ── 云（网盘）──────────────────────────────────────────────
    # 以 256 为基准设计，再按比例缩放
    s = size / 256.0

    def box(*v):
        return [x * s for x in v]

    # 三个圆 + 一条底边构成云朵轮廓
    # 整体向左上偏移 15px，让云朵（含播放三角）视觉居中
    dx, dy = 47, 100
    d.ellipse(box(dx, dy, dx + 76, dy + 76), fill=WHITE)                 # 左圆
    d.ellipse(box(dx + 50, dy - 26, dx + 138, dy + 62), fill=WHITE)      # 中圆（最大）
    d.ellipse(box(dx + 98, dy, dx + 162, dy + 64), fill=WHITE)           # 右圆
    d.rounded_rectangle(box(dx, dy + 32, dx + 162, dy + 76), radius=22 * s, fill=WHITE)

    # ── 云里的播放三角（挖空成底色）────────────────────────────
    # 用背景渐变上的颜色填充，形成“镂空”观感
    cx, cy = 128 * s, 134 * s
    tri = 34 * s
    hole = lerp(TOP, BOTTOM, 0.62) + (255,)
    d.polygon(
        [(cx - tri * 0.42, cy - tri * 0.58),
         (cx - tri * 0.42, cy + tri * 0.58),
         (cx + tri * 0.62, cy)],
        fill=hole,
    )

    # ── 右上角闪光（刮削 / 更新）──────────────────────────────
    sx, sy = 205 * s, 58 * s
    outer, inner = 28 * s, 8.5 * s
    spark = [
        (sx, sy - outer), (sx + inner * 0.72, sy - inner * 0.72),
        (sx + outer, sy), (sx + inner * 0.72, sy + inner * 0.72),
        (sx, sy + outer), (sx - inner * 0.72, sy + inner * 0.72),
        (sx - outer, sy), (sx - inner * 0.72, sy - inner * 0.72),
    ]
    d.polygon(spark, fill=WHITE)

    return img


def main():
    for name, size in SIZES.items():
        img = build(size)
        targets = [
            os.path.join(OUT_ROOT, "ICON.PNG" if size == 64 else "ICON_256.PNG"),
            os.path.join(OUT_ROOT, "app", "ui", "images", f"icon_{size}.png"),
        ]
        for t in targets:
            os.makedirs(os.path.dirname(t), exist_ok=True)
            img.save(t, "PNG")
            print(f"saved {t} ({size}x{size})")


if __name__ == "__main__":
    main()
