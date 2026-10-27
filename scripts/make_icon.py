"""生成应用图标：crates/app/assets/icon.png 与多尺寸 crates/app/assets/icon.ico。

设计是紫 → 靛 → 青的渐变圆角方块，中间是白色声波柱。
小尺寸（16/24/32）单独改用三根更粗的柱子，避免缩小时糊成一团。

只在重新生成图标时需要运行，依赖 Pillow 与 numpy：

    python scripts/make_icon.py
"""

from pathlib import Path

import numpy as np
from PIL import Image, ImageChops, ImageDraw, ImageFilter

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "crates" / "app" / "assets"

# 渐变停点：左上紫、中部靛蓝、右下青。
STOPS = (
    (0.00, (139, 92, 246)),
    (0.55, (99, 102, 241)),
    (1.00, (34, 211, 238)),
)

ICO_SIZES = (16, 24, 32, 40, 48, 64, 128, 256)


def gradient(size: int) -> np.ndarray:
    y, x = np.mgrid[0:size, 0:size].astype(np.float32)
    t = (x + y) / (2 * max(size - 1, 1))

    rgb = np.zeros((size, size, 3), dtype=np.float32)
    for (start, start_color), (end, end_color) in zip(STOPS, STOPS[1:]):
        span = t[(t >= start) & (t <= end)]
        local = ((span - start) / (end - start))[:, None]
        colors = np.array(start_color, dtype=np.float32) * (1 - local)
        colors += np.array(end_color, dtype=np.float32) * local
        rgb[(t >= start) & (t <= end)] = colors
    rgb[t >= STOPS[-1][0]] = STOPS[-1][1]

    # 左上角柔光，让纯渐变多一点层次。
    glow = np.exp(-(((x / size - 0.26) ** 2 + (y / size - 0.18) ** 2) / 0.08))
    rgb += glow[..., None] * 34
    return np.clip(rgb, 0, 255)


def bar_spec(size: int) -> tuple[float, float, tuple[float, ...]]:
    """返回（柱宽, 柱间距, 高度比例）。尺寸都相对圆角方块边长。"""
    if size <= 32:
        return 0.155, 0.100, (0.40, 0.70, 0.40)
    if size < 48:
        return 0.110, 0.075, (0.36, 0.58, 0.82, 0.58, 0.36)
    return 0.078, 0.052, (0.33, 0.55, 0.84, 0.55, 0.33)


def render(size: int) -> Image.Image:
    scale = 4
    s = size * scale
    inset = round(s * 0.025)
    radius = round((s - 2 * inset) * 0.235)

    tile_mask = Image.new("L", (s, s), 0)
    ImageDraw.Draw(tile_mask).rounded_rectangle(
        (inset, inset, s - inset - 1, s - inset - 1),
        radius=radius,
        fill=255,
    )

    tile = Image.fromarray(gradient(s).astype(np.uint8), "RGB").convert("RGBA")
    tile.putalpha(tile_mask)

    tile_side = s - 2 * inset
    width_ratio, gap_ratio, heights = bar_spec(size)
    bar_width = tile_side * width_ratio
    gap = tile_side * gap_ratio
    total = len(heights) * bar_width + (len(heights) - 1) * gap
    left = (s - total) / 2
    center_y = s / 2

    bars_mask = Image.new("L", (s, s), 0)
    draw = ImageDraw.Draw(bars_mask)
    for index, height_ratio in enumerate(heights):
        height = tile_side * height_ratio
        x = left + index * (bar_width + gap)
        y = center_y - height / 2
        draw.rounded_rectangle(
            (x, y, x + bar_width, y + height),
            radius=bar_width / 2,
            fill=255,
        )

    # 柱子底部投一层很淡的深色阴影，贴住渐变底。
    shadow_mask = bars_mask.filter(ImageFilter.GaussianBlur(s * 0.018))
    shadow_mask = shadow_mask.point(lambda value: int(value * 0.30))
    shadow_mask = ImageChops.multiply(shadow_mask, tile_mask)
    shadow = Image.new("RGBA", (s, s), (28, 22, 66, 255))
    shadow.putalpha(shadow_mask)
    result = Image.alpha_composite(tile, shadow)

    # 柱子用近白到淡紫的竖向微渐变，避免纯白显得死板。
    y = np.linspace(0, 1, s, dtype=np.float32)[:, None, None]
    start = np.array([255, 255, 255], dtype=np.float32)
    end = np.array([236, 232, 255], dtype=np.float32)
    bar_rgb = np.repeat(start * (1 - y) + end * y, s, axis=1)
    bars = Image.fromarray(bar_rgb.astype(np.uint8), "RGB").convert("RGBA")
    bars.putalpha(bars_mask)
    result = Image.alpha_composite(result, bars)

    # 内描边：一条极细的白色高光，让边缘在浅色背景上也立得住。
    border = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    border_width = max(round(s * 0.0045), 1)
    ImageDraw.Draw(border).rounded_rectangle(
        (
            inset + border_width,
            inset + border_width,
            s - inset - border_width - 1,
            s - inset - border_width - 1,
        ),
        radius=radius,
        outline=(255, 255, 255, 46),
        width=border_width,
    )
    result = Image.alpha_composite(result, border)

    return result.resize((size, size), Image.Resampling.LANCZOS)


def main() -> None:
    ASSETS.mkdir(exist_ok=True)

    render(512).save(ASSETS / "icon.png", optimize=True)

    frames = {size: render(size) for size in ICO_SIZES}
    base = frames[256]
    ordered = [frames[size] for size in ICO_SIZES if size != 256]
    base.save(
        ASSETS / "icon.ico",
        sizes=[(size, size) for size in ICO_SIZES],
        append_images=ordered,
        bitmap_format="bmp",
    )

    print(f"已生成 {ASSETS / 'icon.png'} 与 {ASSETS / 'icon.ico'}")


if __name__ == "__main__":
    main()
