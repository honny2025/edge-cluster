# -*- coding: utf-8 -*-
"""生成 edge-cluster 应用图标（边缘计算集群主题）。

画一个「中心枢纽 + 六边形分布的边缘节点」的集群网格：
  - 深蓝→藏青渐变圆角方底
  - 中央高亮枢纽节点（平台）
  - 六个卫星节点（手机/边缘设备）按六边形环绕，用连线连成 mesh
产出一张 icon.png（512）+ 一个多尺寸 icon.ico。
"""
import math
from PIL import Image, ImageDraw, ImageFilter

S = 1024          # 逻辑画布尺寸
SS = 4            # 超采样倍数（渲染 4x 再缩回，抗锯齿）
R = S * SS

# ---- 调色板 ----
BG_TOP = (0x4A, 0x63, 0xE7)   # 亮靛蓝
BG_BOT = (0x0E, 0x13, 0x30)   # 深藏青
LINE = (255, 255, 255)        # 连线（低透明度）
HUB_A = (0x3F, 0xE0, 0xF0)    # 枢纽亮青
HUB_B = (0x2B, 0x7B, 0xFF)    # 枢纽蓝
SAT_A = (0xC7, 0xD8, 0xFF)    # 卫星浅蓝
SAT_B = (0x6E, 0x8F, 0xF5)    # 卫星蓝


def lerp(a, b, t):
    return tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(3))


def gradient(size, top, bottom):
    """自上而下的线性渐变。"""
    w, h = size
    img = Image.new("RGB", (w, h))
    px = img.load()
    for y in range(h):
        c = lerp(top, bottom, y / (h - 1))
        for x in range(w):
            px[x, y] = c
    return img


def hex_positions(cx, cy, radius):
    """六个顶点，从正上方开始顺时针。"""
    pts = []
    for k in range(6):
        a = math.radians(30 + 60 * k)   # 30° 起点让一个顶点朝正上
        pts.append((cx + radius * math.cos(a), cy + radius * math.sin(a)))
    return pts


def build():
    img = gradient((R, R), BG_TOP, BG_BOT)

    # 圆角方形遮罩（半径 ~ 0.22*S，接近现代桌面图标圆角）
    mask = Image.new("L", (R, R), 0)
    md = ImageDraw.Draw(mask)
    rad = int(0.22 * R)
    md.rounded_rectangle([0, 0, R - 1, R - 1], radius=rad, fill=255)
    img.putalpha(mask)

    # 在带透明度的图层上画，避免遮罩之外留像素
    layer = Image.new("RGBA", (R, R), (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)

    cx = cy = R // 2
    hub_r = int(0.105 * R)
    sat_r = int(0.062 * R)
    orbit = int(0.32 * R)
    pts = hex_positions(cx, cy, orbit)

    # 连线：枢纽→卫星 与 卫星→相邻卫星（mesh），半透明白
    for p in pts:
        d.line([(cx, cy), p], fill=(*LINE, 92), width=int(0.010 * R))
    for i in range(6):
        d.line([pts[i], pts[(i + 1) % 6]], fill=(*LINE, 58), width=int(0.008 * R))

    # 卫星节点：浅蓝圆 + 亮边
    for (x, y) in pts:
        d.ellipse([x - sat_r, y - sat_r, x + sat_r, y + sat_r],
                  fill=(*SAT_A, 235), outline=(*SAT_B, 255), width=int(0.006 * R))

    # 中央枢纽：径向渐变（近似用两层椭圆），亮青→蓝
    d.ellipse([cx - hub_r, cy - hub_r, cx + hub_r, cy + hub_r], fill=(*HUB_B, 255))
    core_r = int(hub_r * 0.72)
    d.ellipse([cx - core_r, cy - core_r, cx + core_r, cy + core_r], fill=(*HUB_A, 255))
    # 中心一点高光
    hi_r = int(hub_r * 0.34)
    d.ellipse([cx - hi_r, cy - hi_r, cx + hi_r, cy + hi_r], fill=(255, 255, 255, 235))

    # 合成
    img = Image.alpha_composite(img, layer)

    # 轻微柔化发光：给亮色图层做一次高斯模糊再叠
    glow = layer.filter(ImageFilter.GaussianBlur(int(0.006 * R)))
    img = Image.alpha_composite(img, Image.blend(glow, Image.new("RGBA", (R, R), (0, 0, 0, 0)), 0.0))

    img = img.resize((S, S), Image.LANCZOS)
    return img


if __name__ == "__main__":
    import os
    here = os.path.dirname(os.path.abspath(__file__))
    big = build()
    png = big.resize((512, 512), Image.LANCZOS)
    png.save(os.path.join(here, "icon.png"))
    # 多尺寸 .ico（含 16/24/32/48/64/128/256）
    sizes = [16, 24, 32, 48, 64, 128, 256]
    big.save(os.path.join(here, "icon.ico"), format="ICO",
             sizes=[(s, s) for s in sizes])
    print("wrote icon.png (512) + icon.ico", sizes)
