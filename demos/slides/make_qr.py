#!/usr/bin/env python3
"""Regenerate the inline QR code in the slides and the standalone QR image.

The slide deck must stay self-contained, so the QR code is inlined as SVG
between the marker comments in index.html. This script rewrites that block and
also writes qr.png for standalone use.

Usage:
    uv run --with qrcode --with pillow python demos/slides/make_qr.py \
        --url https://static.caric.io/vdd26/av2demo
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
    parser.add_argument("--url", required=True)
    parser.add_argument("--page", default=str(Path(__file__).with_name("index.html")))
    parser.add_argument("--png", default=str(Path(__file__).with_name("qr.png")))
    arguments = parser.parse_args()

    page = Path(arguments.page)
    html = page.read_text()
    block = "<!-- QR:START -->\n" + svg_for(arguments.url) + "\n<!-- QR:END -->"
    updated, count = re.subn(
        r"<!-- QR:START -->.*?<!-- QR:END -->", block, html, flags=re.S
    )
    if count != 1:
        raise SystemExit(f"expected one QR block in {page}, found {count}")
    page.write_text(updated)
    png_for(arguments.url, Path(arguments.png))
    print(f"updated {page} and {arguments.png} for {arguments.url}")


if __name__ == "__main__":
    main()
