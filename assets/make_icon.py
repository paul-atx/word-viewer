"""Draws assets/word-viewer.ico: a document page with a magnifying glass.

Each size is drawn separately at 4x and downsampled, and the smallest sizes
drop detail so they stay crisp. Requires Pillow: python3 make_icon.py
"""
from pathlib import Path

from PIL import Image, ImageDraw

PAGE = (255, 255, 255, 255)
EDGE = (120, 136, 152, 255)
FOLD = (214, 223, 232, 255)
LINE = (150, 163, 176, 255)
ACCENT = (18, 124, 138, 255)
GLASS = (226, 244, 246, 255)
SIZES = [16, 24, 32, 48, 64, 128, 256]


def draw(size: int) -> Image.Image:
    k = 4
    s = size * k
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    u = s / 32  # design grid: 32 x 32 units

    def box(x0, y0, x1, y1):
        return [x0 * u, y0 * u, x1 * u, y1 * u]

    # page with a folded top-right corner
    x0, y0, x1, y1, f = 5, 2, 25, 30, 7
    w = max(1, round(1.2 * u))
    page = [(x0 * u, y0 * u), ((x1 - f) * u, y0 * u), (x1 * u, (y0 + f) * u), (x1 * u, y1 * u), (x0 * u, y1 * u)]
    d.polygon(page, fill=PAGE, outline=EDGE, width=w)
    d.polygon([((x1 - f) * u, y0 * u), ((x1 - f) * u, (y0 + f) * u), (x1 * u, (y0 + f) * u)], fill=FOLD, outline=EDGE, width=w)

    # heading and text lines
    lw = 1.6 if size >= 32 else 2.4
    d.rectangle(box(8.5, 8, 16, 8 + lw + 0.6), fill=ACCENT)
    rows = [12.5, 16, 19.5, 23] if size >= 32 else [13.5, 18.5]
    for i, y in enumerate(rows):
        end = 21.5 if i % 2 == 0 else 18.5
        if size >= 32 and y > 15:
            end = min(end, 16)  # leave room for the magnifier
        d.rectangle(box(8.5, y, end, y + lw), fill=LINE)

    if size >= 32:
        # magnifying glass, bottom right
        cx, cy, r = 21, 21.5, 5.2
        d.line([((cx + r * 0.62) * u, (cy + r * 0.62) * u), (29.5 * u, 30 * u)], fill=ACCENT, width=round(3.2 * u))
        d.ellipse(box(cx - r, cy - r, cx + r, cy + r), fill=GLASS, outline=ACCENT, width=round(1.8 * u))
    return img.resize((size, size), Image.LANCZOS)


if __name__ == "__main__":
    out = Path(__file__).with_name("word-viewer.ico")
    imgs = [draw(n) for n in SIZES]
    imgs[-1].save(out, sizes=[(n, n) for n in SIZES], append_images=imgs[:-1])
    imgs[-1].save(out.with_name("word-viewer.png"))
    print("wrote", out)
