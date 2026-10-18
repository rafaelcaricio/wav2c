#!/usr/bin/env python3
"""Regenerate the inline QR code in the slides and the standalone QR image.

The slide deck must stay self-contained, so the QR code is inlined as SVG
between the marker comments in index.html. This script rewrites that block and
also writes qr.png for standalone use.

The deck carries two codes: the demo URL on the try-it slide and the source
repository URL on the cost slide.

Usage:
    uv run --with qrcode --with pillow python demos/slides/make_qr.py
"""

import argparse
import re
from pathlib import Path

import qrcode

DARK = "#0b0f0d"
LIGHT = "#e8f1ea"


def matrix_for(url, border=2):
    code = qrcode.QRCode(border=border, box_size=1)
    code.add_data(url)
    code.make(fit=True)
    return code.get_matrix()


def svg_for(url):
    matrix = matrix_for(url)
    size = len(matrix)
    modules = "".join(
        f"M{column} {row}h1v1h-1z"
        for row, values in enumerate(matrix)
        for column, value in enumerate(values)
        if value
    )
    return (
        f'<svg class="qr" viewBox="0 0 {size} {size}" role="img" '
        f'aria-label="QR code for {url}" xmlns="http://www.w3.org/2000/svg">'
        f'<rect width="{size}" height="{size}" fill="{LIGHT}"/>'
        f'<path d="{modules}" fill="{DARK}"/>'
        f"</svg>"
    )


def png_for(url, target, box_size=12, border=2):
    code = qrcode.QRCode(border=border, box_size=box_size)
    code.add_data(url)
    code.make(fit=True)
    image = code.make_image(fill_color=DARK, back_color=LIGHT).convert("RGB")
    image.save(target)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--demo-url", default="https://static.caric.io/vdd26/av2demo")
    parser.add_argument(
        "--source-url",
        default="https://github.com/rafaelcaricio/wav2c/tree/av2-v1.0.0-conformance",
    )
    parser.add_argument("--page", default=str(Path(__file__).with_name("index.html")))
    parser.add_argument("--png", default=str(Path(__file__).with_name("qr.png")))
    parser.add_argument(
        "--source-png", default=str(Path(__file__).with_name("qr-source.png"))
    )
    arguments = parser.parse_args()

    page = Path(arguments.page)
    html = page.read_text()
    for kind, url in (("DEMO", arguments.demo_url), ("SOURCE", arguments.source_url)):
        block = f"<!-- QR:{kind}:START -->\n{svg_for(url)}\n<!-- QR:{kind}:END -->"
        html, count = re.subn(
            rf"<!-- QR:{kind}:START -->.*?<!-- QR:{kind}:END -->", block, html, flags=re.S
        )
        if count != 1:
            raise SystemExit(f"expected one QR:{kind} block in {page}, found {count}")
    page.write_text(html)
    png_for(arguments.demo_url, Path(arguments.png))
    png_for(arguments.source_url, Path(arguments.source_png))
    print(f"updated {page}, {arguments.png} and {arguments.source_png}")


if __name__ == "__main__":
    main()
