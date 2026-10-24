import pystray
from PIL import Image, ImageDraw

def create_icon():
    img = Image.new("RGB", (64, 64), (40, 40, 40))
    draw = ImageDraw.Draw(img)
    draw.ellipse((8, 8, 56, 56), fill=(0, 180, 255))
    return img

def on_quit(icon, item):
    icon.stop()

menu = pystray.Menu(
    pystray.MenuItem("显示状态", lambda icon, item: print("运行中")),
    pystray.MenuItem("退出", on_quit),
)

icon = pystray.Icon("my_app", create_icon(), "我的托盘应用", menu)
icon.run()