"""生成 LeonMusic 应用图标（1024x1024 PNG）。

设计：深色圆角底 + 紫青渐变音符 + 一道高光。
不用外部素材，保证仓库可复现。
"""
from PIL import Image, ImageDraw, ImageFilter
import math

S = 1024
img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
d = ImageDraw.Draw(img)

# 圆角矩形底
radius = 210
d.rounded_rectangle([0, 0, S - 1, S - 1], radius=radius, fill=(18, 18, 26, 255))

# 渐变（用像素级插值，简单可靠）
grad = Image.new("RGB", (S, S))
gd = ImageDraw.Draw(grad)
c1 = (124, 92, 255)   # #7c5cff
c2 = (61, 214, 208)   # #3dd6d0
for y in range(S):
    t = y / (S - 1)
    r = int(c1[0] + (c2[0] - c1[0]) * t)
    g = int(c1[1] + (c2[1] - c1[1]) * t)
    b = int(c1[2] + (c2[2] - c1[2]) * t)
    gd.line([(0, y), (S, y)], fill=(r, g, b))

# 渐变遮罩：只保留一个圆
mask = Image.new("L", (S, S), 0)
md = ImageDraw.Draw(mask)
md.ellipse([170, 170, S - 170, S - 170], fill=255)
mask = mask.filter(ImageFilter.GaussianBlur(2))
img.paste(grad, (0, 0), mask)

# 音符：符头 + 符干 + 符尾
note = Image.new("RGBA", (S, S), (0, 0, 0, 0))
nd = ImageDraw.Draw(note)
white = (245, 245, 255, 255)

# 符头（椭圆，略微倾斜用两个圆叠加）
nd.ellipse([330, 640, 530, 790], fill=white)
# 符干
nd.rounded_rectangle([498, 320, 540, 720], radius=20, fill=white)
# 符尾（用多边形近似弧形）
nd.polygon(
    [
        (540, 320),
        (700, 360),
        (700, 470),
        (540, 430),
    ],
    fill=white,
)
# 高光
glow = note.filter(ImageFilter.GaussianBlur(18))
img.alpha_composite(glow)
img.alpha_composite(note)

img.save("app-icon.png")
print("wrote app-icon.png", img.size)
