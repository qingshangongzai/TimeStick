#!/usr/bin/env python3
"""从 assets/icon.svg（设计源）生成多尺寸 assets/icon.ico（exe/托盘图标）。

用法：
    python scripts/make_icon.py

依赖（一次性安装）：
    pip install resvg-py pillow

说明：resvg 直接渲染 SVG 源，保证 .ico 与 .svg 完全一致（单一设计源）。
      产物 .ico 已提交仓库，日常构建无需再运行本脚本；仅在设计改动后重新生成。
"""

import io
import sys
from pathlib import Path

import resvg_py
from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
SVG = ROOT / "assets" / "icon.svg"
ICO = ROOT / "assets" / "icon.ico"

# exe 图标标准尺寸；托盘运行时加载 32px 帧，窗口小图标（hIconSm）用 16px 帧
SIZES = [16, 24, 32, 48, 64, 128, 256]


def main() -> int:
    if not SVG.exists():
        print(f"错误：找不到 {SVG}", file=sys.stderr)
        return 1

    # 逐尺寸用 resvg 栅格化 SVG（保持透明背景）
    frames = []
    for s in SIZES:
        png = resvg_py.svg_to_bytes(svg_path=str(SVG), width=s, height=s)
        frames.append(Image.open(io.BytesIO(png)).convert("RGBA"))

    # 打包 ICO：全部尺寸写 PNG 条目（Vista+ 支持，体积小）；
    # 基准图必须是最大尺寸，否则 Pillow 会跳过更大的尺寸
    frames[-1].save(
        ICO,
        format="ICO",
        sizes=[(s, s) for s in SIZES],
        append_images=frames[:-1],
    )

    # 校验：重新读取确认尺寸齐全
    with Image.open(ICO) as check:
        print(f"已生成 {ICO}，内含 {len(check.info.get('sizes', []))} 个尺寸:",
              " ".join(f"{w}x{h}" for w, h in sorted(check.info["sizes"])))
    return 0


if __name__ == "__main__":
    sys.exit(main())
