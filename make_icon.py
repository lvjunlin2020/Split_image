"""从项目自带的 assets/icon2.png 生成多尺寸 ico(exe 资源)与 icon_64.rgba(窗口图标)。"""
from PIL import Image
import os

os.chdir(os.path.dirname(os.path.abspath(__file__)))
im = Image.open("assets/icon2.png").convert("RGBA")

# 补成正方形(目前是 2048x2048,保险处理)
w, h = im.size
side = max(w, h)
canvas = Image.new("RGBA", (side, side), (0, 0, 0, 0))
canvas.paste(im, ((side - w) // 2, (side - h) // 2))

im256 = canvas.resize((256, 256), Image.LANCZOS)
im256.save("assets/icon.ico",
           sizes=[(16, 16), (20, 20), (24, 24), (32, 32), (40, 40),
                  (48, 48), (64, 64), (128, 128), (256, 256)])
im64 = canvas.resize((64, 64), Image.LANCZOS)
open("assets/icon_64.rgba", "wb").write(im64.tobytes())
print("OK: assets/icon.ico / assets/icon_64.rgba")
