"""アプリアイコン（src-tauri/icons/icon.ico）を生成する。

デザイン: 赤い角丸の正方形に、プレイリストを表す白いバー3本と再生マーク。
使い方: python tools/make_icon.py
"""
from pathlib import Path

from PIL import Image, ImageDraw

SIZE = 1024
OUT = Path(__file__).resolve().parent.parent / "src-tauri" / "icons"
SIZES = [16, 24, 32, 48, 64, 128, 256]


def draw_master() -> Image.Image:
    img = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))

    # 背景: 上から下へ赤のグラデーションを角丸でクリップ
    grad = Image.new("RGBA", (SIZE, SIZE))
    gd = ImageDraw.Draw(grad)
    top, bottom = (240, 52, 40), (176, 18, 18)
    for y in range(SIZE):
        t = y / (SIZE - 1)
        gd.line([(0, y), (SIZE, y)], fill=tuple(round(a + (b - a) * t) for a, b in zip(top, bottom)) + (255,))
    mask = Image.new("L", (SIZE, SIZE), 0)
    ImageDraw.Draw(mask).rounded_rectangle([32, 32, SIZE - 32, SIZE - 32], radius=210, fill=255)
    img.paste(grad, (0, 0), mask)

    d = ImageDraw.Draw(img)
    white = (255, 255, 255, 255)

    # プレイリストのバー（左側に3本）
    bar_h, gap, x0, x1 = 84, 84, 170, 520
    y0 = 512 - (bar_h * 3 + gap * 2) // 2
    for i in range(3):
        y = y0 + i * (bar_h + gap)
        d.rounded_rectangle([x0, y, x1, y + bar_h], radius=bar_h // 2, fill=white)

    # 再生マーク（右側）
    d.polygon([(600, 330), (600, 694), (884, 512)], fill=white)
    return img


def main() -> None:
    master = draw_master()
    OUT.mkdir(parents=True, exist_ok=True)
    master.save(OUT / "icon.ico", sizes=[(s, s) for s in SIZES])
    master.resize((512, 512), Image.LANCZOS).save(OUT / "icon.png")
    print("written:", OUT / "icon.ico")


if __name__ == "__main__":
    main()
