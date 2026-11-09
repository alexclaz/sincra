"""Draws the Sincra icon and writes every size the builds need.

    python packaging/icon/make_icon.py

Outputs (next to this script / in packaging/macos):
  sincra.ico                      Windows executables (16-256 px)
  sincra-256.png                  window icon (embedded by the GUI)
  ../macos/Sincra.iconset/*.png   turned into Sincra.icns by `iconutil` in the release workflow
"""
from pathlib import Path

from PIL import Image, ImageDraw

HERE = Path(__file__).resolve().parent
S = 1024          # master size
SS = 4            # supersampling factor

BG_TOP = (0x24, 0x28, 0x2e)
BG_BOTTOM = (0x14, 0x16, 0x1a)
TRACKS = [  # (colour, start, end) as fractions of the width
    ((0x3f, 0xa8, 0x9b), 0.17, 0.66),
    ((0x5b, 0x8b, 0xd9), 0.30, 0.83),
    ((0x95, 0x7f, 0xd6), 0.22, 0.74),
]
ACCENT = (0xe8, 0x93, 0x2a)


def master() -> Image.Image:
    n = S * SS
    img = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    # background: rounded square with a vertical gradient (macOS-style margin so it sits well in the Dock)
    margin = int(n * 0.06)
    radius = int(n * 0.2)
    grad = Image.new("RGBA", (n, n))
    gd = ImageDraw.Draw(grad)
    for y in range(n):
        t = y / (n - 1)
        c = tuple(int(BG_TOP[i] + (BG_BOTTOM[i] - BG_TOP[i]) * t) for i in range(3))
        gd.line([(0, y), (n, y)], fill=c + (255,))
    mask = Image.new("L", (n, n), 0)
    ImageDraw.Draw(mask).rounded_rectangle([margin, margin, n - margin, n - margin], radius=radius, fill=255)
    img.paste(grad, (0, 0), mask)

    d = ImageDraw.Draw(img)
    # three timeline tracks, staggered
    bar_h = int(n * 0.105)
    gap = int(n * 0.07)
    top = n // 2 - (3 * bar_h + 2 * gap) // 2
    for k, (col, a, b) in enumerate(TRACKS):
        y = top + k * (bar_h + gap)
        d.rounded_rectangle([int(n * a), y, int(n * b), y + bar_h], radius=bar_h // 3, fill=col + (255,))
    # the sync cursor: an orange line with a playhead triangle, crossing every track
    x = int(n * 0.5)
    lw = int(n * 0.028)
    y0 = top - int(n * 0.085)
    y1 = top + 3 * bar_h + 2 * gap + int(n * 0.06)
    d.rectangle([x - lw // 2, y0, x + lw // 2, y1], fill=ACCENT + (255,))
    tri = int(n * 0.075)
    d.polygon([(x - tri, y0 - int(tri * 0.2)), (x + tri, y0 - int(tri * 0.2)), (x, y0 + int(tri * 0.9))], fill=ACCENT + (255,))
    return img.resize((S, S), Image.LANCZOS)


def main() -> None:
    img = master()
    img.resize((256, 256), Image.LANCZOS).save(HERE / "sincra-256.png")
    img.save(HERE / "sincra.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    iconset = HERE.parent / "macos" / "Sincra.iconset"
    iconset.mkdir(parents=True, exist_ok=True)
    for base in (16, 32, 128, 256, 512):
        img.resize((base, base), Image.LANCZOS).save(iconset / f"icon_{base}x{base}.png")
        img.resize((base * 2, base * 2), Image.LANCZOS).save(iconset / f"icon_{base}x{base}@2x.png")
    print("icons written")


if __name__ == "__main__":
    main()
