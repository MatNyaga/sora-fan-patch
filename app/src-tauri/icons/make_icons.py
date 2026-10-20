"""Draw our own app icon (night tile, gold/blue ring, 4-point star). No game art.
   python3 -I icons/make_icons.py  -> icons/*.png, icon.ico, icon.icns"""
import os, sys
from PIL import Image, ImageDraw
here = os.path.dirname(os.path.abspath(__file__))
N = 1024
img = Image.new("RGBA", (N, N), (0, 0, 0, 0))
# rounded tile with vertical night gradient
grad = Image.new("RGBA", (N, N))
gd = ImageDraw.Draw(grad)
for y in range(N):
    f = y / (N - 1)
    c = tuple(int(a + (b - a) * f) for a, b in zip((11, 16, 38), (36, 66, 140))) + (255,)
    gd.line([(0, y), (N, y)], fill=c)
mask = Image.new("L", (N, N), 0)
ImageDraw.Draw(mask).rounded_rectangle([40, 40, N - 40, N - 40], radius=220, fill=255)
img.paste(grad, (0, 0), mask)
d = ImageDraw.Draw(img)
cx = cy = N / 2
# ring: gold (top-left) to blue (bottom-right), drawn as many short arcs
R, W = 360, 46
for a in range(0, 360, 2):
    f = ((a - 225) % 360) / 360
    f = 1 - abs(1 - 2 * f)
    col = tuple(int(x + (y - x) * f) for x, y in zip((255, 217, 120), (127, 182, 255))) + (255,)
    d.arc([cx - R, cy - R, cx + R, cy + R], a, a + 3, fill=col, width=W)
# 4-point star with curved sides (sampled cubic Beziers)
def bez(p0, p1, p2, p3, n=40):
    return [tuple((1 - t) ** 3 * a + 3 * (1 - t) ** 2 * t * b + 3 * (1 - t) * t ** 2 * c + t ** 3 * e
                  for a, b, c, e in zip(p0, p1, p2, p3)) for t in (i / n for i in range(n + 1))]
s = 9.2  # svg units (64) -> 1024 tile around centre
P = lambda x, y: (cx + (x - 32) * s * 1.1, cy + (y - 32) * s * 1.1)
pts = []
pts += bez(P(32, 9), P(33.6, 23), P(41, 30.4), P(55, 32))
pts += bez(P(55, 32), P(41, 33.6), P(33.6, 41), P(32, 55))
pts += bez(P(32, 55), P(30.4, 41), P(23, 33.6), P(9, 32))
pts += bez(P(9, 32), P(23, 30.4), P(30.4, 23), P(32, 9))
d.polygon(pts, fill=(255, 246, 222, 255))
d.ellipse([cx + 228, cy - 300, cx + 300, cy - 228], fill=(255, 217, 120, 255))
img.save(os.path.join(here, "icon.png"))
for size, name in [(32, "32x32.png"), (128, "128x128.png"), (256, "128x128@2x.png")]:
    img.resize((size, size), Image.LANCZOS).save(os.path.join(here, name))
img.save(os.path.join(here, "icon.ico"), sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
img.save(os.path.join(here, "icon.icns"))
print("icons written")
