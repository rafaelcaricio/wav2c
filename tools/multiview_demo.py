#!/usr/bin/env python3
"""End-to-end multiview demo: av2codec encodes, avmdec decodes.

Builds a stereoscopic pair and a three-view conference layout from synthesized
clips, encodes both as AV2 multistream bitstreams, decodes every extended layer
with avmdec, verifies lossless reconstruction and reports lossy PSNR, then
rebuilds the atlas canvas declared by the encoder and writes PNG snapshots.
"""

import argparse
import math
import shutil
import subprocess
import sys
from pathlib import Path

from PIL import Image

REPO_ROOT = Path(__file__).resolve().parents[1]
FRAME_RATE = "24"
BACKGROUND = (16, 16, 16)
STEREO_DISPARITY = 64
OBU_NAMES = {
    1: "SEQ", 2: "TD", 3: "MFH", 4: "CLK", 5: "OLK", 6: "LTG", 7: "RTG",
    8: "MD_SHORT", 9: "MD_GROUP", 10: "SWITCH", 16: "LCR", 17: "ATLAS",
    18: "OPS", 20: "MSDO", 25: "PADDING",
}


def run(command, quiet=True):
    result = subprocess.run(
        command,
        stdout=subprocess.PIPE if quiet else None,
        stderr=subprocess.PIPE if quiet else None,
        text=True,
    )
    if result.returncode != 0:
        raise SystemExit(
            f"command failed: {' '.join(str(part) for part in command)}\n"
            f"{result.stderr or ''}"
        )
    return result


def source_clip(pattern, width, height, frames, out_path):
    run(
        [
            "ffmpeg", "-v", "error", "-y",
            "-f", "lavfi",
            "-i", f"{pattern}=size={width}x{height}:rate={FRAME_RATE}",
            "-frames:v", str(frames),
            "-pix_fmt", "yuv420p",
            str(out_path),
        ]
    )


def crop_clip(src, width, height, x, out_path):
    run(
        [
            "ffmpeg", "-v", "error", "-y",
            "-i", str(src),
            "-vf", f"crop={width}:{height}:{x}:0",
            "-pix_fmt", "yuv420p",
            str(out_path),
        ]
    )


def read_y4m(path):
    data = Path(path).read_bytes()
    header_end = data.index(b"\n") + 1
    header = data[:header_end].decode("ascii").split()
    width = int(header[1][1:])
    height = int(header[2][1:])
    frames = []
    offset = header_end
    chroma_width = width // 2
    chroma_height = height // 2
    luma_size = width * height
    chroma_size = chroma_width * chroma_height
    while offset < len(data):
        assert data[offset:offset + 6] == b"FRAME\n", "unexpected y4m frame marker"
        offset += 6
        planes = []
        for size in (luma_size, chroma_size, chroma_size):
            planes.append(data[offset:offset + size])
            offset += size
        frames.append(tuple(planes))
    return width, height, frames


def planes_of(frame):
    return b"".join(frame)


def read_leb128(data, offset):
    value = 0
    shift = 0
    while True:
        byte = data[offset]
        offset += 1
        value |= (byte & 0x7F) << shift
        shift += 7
        if not byte & 0x80:
            return value, offset


def obu_layout(bitstream):
    obus = []
    offset = 0
    while offset < len(bitstream):
        size, offset = read_leb128(bitstream, offset)
        header = bitstream[offset]
        offset += 1
        extension = (header >> 7) & 1
        obu_type = (header >> 2) & 0x1F
        tlayer = header & 0x03
        mlayer = 0
        xlayer = 31 if obu_type in (2, 20) else 0
        if extension:
            byte = bitstream[offset]
            offset += 1
            mlayer = (byte >> 5) & 0x07
            xlayer = byte & 0x1F
        obus.append((OBU_NAMES.get(obu_type, f"TYPE{obu_type}"), tlayer, mlayer, xlayer, size))
        offset += size - 1 - extension
    return obus


def yuv_to_rgb(y_plane, u_plane, v_plane, width, height):
    luma = Image.frombytes("L", (width, height), y_plane)
    chroma_width, chroma_height = width // 2, height // 2
    u = Image.frombytes("L", (chroma_width, chroma_height), u_plane).resize(
        (width, height), Image.BICUBIC
    )
    v = Image.frombytes("L", (chroma_width, chroma_height), v_plane).resize(
        (width, height), Image.BICUBIC
    )
    return Image.merge("YCbCr", (luma, u, v)).convert("RGB")


def psnr(reference, decoded):
    squared_error = 0
    for ref_byte, dec_byte in zip(reference, decoded):
        difference = ref_byte - dec_byte
        squared_error += difference * difference
    if squared_error == 0:
        return math.inf
    mse = squared_error / len(reference)
    return 10.0 * math.log10(255.0 * 255.0 / mse)


def compose_atlas(layout, views):
    if layout == "row":
        columns = [view[0] for view in views]
        rows = [max(view[1] for view in views)]
        cell_x = []
        cursor = 0
        for width in columns:
            cell_x.append(cursor)
            cursor += width
    else:
        columns = [max(view[0] for view in views)]
        rows = [view[1] for view in views]
        cell_x = [0 for _ in views]
    canvas_width = sum(columns)
    canvas_height = sum(rows)
    canvas = Image.new("RGB", (canvas_width, canvas_height), BACKGROUND)
    cursor_y = 0
    for index, view in enumerate(views):
        if layout == "column":
            cell_y = cursor_y
            cursor_y += view[1]
        else:
            cell_y = 0
        image = yuv_to_rgb(view[2][0], view[2][1], view[2][2], view[0], view[1])
        canvas.paste(image, (cell_x[index], cell_y))
    return canvas


def difference_image(reference_views, decoded_views):
    tiles = []
    for reference, decoded in zip(reference_views, decoded_views):
        width, height = reference[0], reference[1]
        ref = yuv_to_rgb(reference[2][0], reference[2][1], reference[2][2], width, height)
        dec = yuv_to_rgb(decoded[2][0], decoded[2][1], decoded[2][2], width, height)
        diff = Image.new("RGB", (width, height))
        diff.putdata(
            [
                min(255, abs(a[0] - b[0]) * 8)
                for a, b in zip(ref.getdata(), dec.getdata())
            ]
        )
        tiles.append(diff)
    canvas = Image.new("RGB", (sum(tile.width for tile in tiles), tiles[0].height))
    cursor = 0
    for tile in tiles:
        canvas.paste(tile, (cursor, 0))
        cursor += tile.width
    return canvas


def decode_multistream(avmdec, bitstream, streams, prefix):
    run(
        [
            str(avmdec), "--codec=av2",
            f"--num-streams={streams}",
            "-o", str(prefix) + ".y4m",
            str(bitstream),
        ]
    )
    return [Path(f"{prefix}_{index}.y4m") for index in range(streams)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work-dir", default=str(REPO_ROOT / "target/demo/multiview"))
    parser.add_argument("--av2codec", default=str(REPO_ROOT / "target/release/av2codec"))
    parser.add_argument("--avmdec", default=str(REPO_ROOT.parent / "avm_build/avmdec"))
    parser.add_argument("--dav2d", default=str(REPO_ROOT.parent / "dav2d/build/tools/dav2d"))
    parser.add_argument("--frames", type=int, default=3)
    parser.add_argument("--qindex", type=int, default=150)
    parser.add_argument("--sweep", action="store_true", help="also report a qindex sweep")
    args = parser.parse_args()

    work = Path(args.work_dir).resolve()
    work.mkdir(parents=True, exist_ok=True)
    av2codec = Path(args.av2codec).resolve()
    avmdec = Path(args.avmdec).resolve()
    dav2d = Path(args.dav2d).resolve()

    if not av2codec.is_file():
        print("building av2codec (release)...", file=sys.stderr)
        run(["cargo", "build", "--release", "--bin", "av2codec"], quiet=False)
    if not avmdec.is_file():
        raise SystemExit(f"missing avmdec binary at {avmdec}")

    results = []
    artifacts = []

    # ---------------------------------------------------------------- content
    stereo_wide = work / "stereo_wide.y4m"
    left = work / "view_left.y4m"
    right = work / "view_right.y4m"
    source_clip("testsrc2", 1280 + STEREO_DISPARITY, 704, args.frames, stereo_wide)
    crop_clip(stereo_wide, 1280, 704, 0, left)
    crop_clip(stereo_wide, 1280, 704, STEREO_DISPARITY, right)

    conference_main = work / "conference_main.y4m"
    conference_small = work / "conference_small.y4m"
    conference_tiny = work / "conference_tiny.y4m"
    source_clip("testsrc2", 960, 512, args.frames, conference_main)
    source_clip("mandelbrot", 512, 256, args.frames, conference_small)
    source_clip("smptebars", 448, 192, args.frames, conference_tiny)

    # --------------------------------------------------------------- encoding
    stereo_lossless = work / "stereo_lossless.obu"
    run(
        [
            str(av2codec), str(left), str(right), str(stereo_lossless),
            "--multiview", "--qindex", "0",
        ]
    )
    stereo_lossy = work / f"stereo_q{args.qindex}.obu"
    run(
        [
            str(av2codec), str(left), str(right), str(stereo_lossy),
            "--multiview", "--qindex", str(args.qindex),
        ]
    )
    conference = work / f"conference_q{args.qindex}.obu"
    run(
        [
            str(av2codec), str(conference_main), str(conference_small),
            str(conference_tiny), str(conference),
            "--multiview", "--layout", "column", "--purpose", "multiview",
            "--qindex", str(args.qindex),
        ]
    )

    # --------------------------------------------------------------- decoding
    stereo_lossless_prefix = work / "stereo_lossless_dec"
    stereo_lossless_layers = decode_multistream(
        avmdec, stereo_lossless, 2, stereo_lossless_prefix
    )
    stereo_lossy_prefix = work / f"stereo_q{args.qindex}_dec"
    stereo_lossy_layers = decode_multistream(avmdec, stereo_lossy, 2, stereo_lossy_prefix)
    conference_prefix = work / f"conference_q{args.qindex}_dec"
    conference_layers = decode_multistream(avmdec, conference, 3, conference_prefix)

    # --------------------------------------------------------------- verific.
    left_width, left_height, left_frames = read_y4m(left)
    right_width, right_height, right_frames = read_y4m(right)
    _, _, lossless_left_frames = read_y4m(stereo_lossless_layers[0])
    _, _, lossless_right_frames = read_y4m(stereo_lossless_layers[1])

    lossless_exact = all(
        planes_of(reference) == planes_of(decoded)
        for reference, decoded in zip(
            left_frames + right_frames, lossless_left_frames + lossless_right_frames
        )
    )
    results.append(("stereo, qindex 0", "bit-exact" if lossless_exact else "MISMATCH"))

    _, _, lossy_left_frames = read_y4m(stereo_lossy_layers[0])
    _, _, lossy_right_frames = read_y4m(stereo_lossy_layers[1])
    left_psnr = psnr(planes_of(left_frames[0]), planes_of(lossy_left_frames[0]))
    right_psnr = psnr(planes_of(right_frames[0]), planes_of(lossy_right_frames[0]))
    results.append(("stereo, qindex %d" % args.qindex, "%.2f dB / %.2f dB" % (left_psnr, right_psnr)))

    conference_sources = [
        ("main", read_y4m(conference_main)),
        ("participant 1", read_y4m(conference_small)),
        ("participant 2", read_y4m(conference_tiny)),
    ]
    conference_decoded = [read_y4m(path) for path in conference_layers]
    conference_psnr = []
    for (name, (width, height, frames)), (dec_width, dec_height, dec_frames) in zip(
        conference_sources, conference_decoded
    ):
        assert (width, height) == (dec_width, dec_height)
        conference_psnr.append(
            "%.2f dB" % psnr(planes_of(frames[0]), planes_of(dec_frames[0]))
        )
    results.append(("conference, qindex %d" % args.qindex, " / ".join(conference_psnr)))
    conference_widths = [width for _, (width, _, _) in conference_sources]
    conference_heights = [height for _, (_, height, _) in conference_sources]
    results.append((
        "atlas canvases",
        f"stereo {2 * left_width}x{left_height}, "
        f"conference {max(conference_widths)}x{sum(conference_heights)}",
    ))

    # ------------------------------------------------- extracted singlestream
    extracted = work / "stereo_right_extracted.obu"
    run(
        [
            str(av2codec), str(left), str(right), str(extracted),
            "--multiview", "--qindex", str(args.qindex), "--extract-layer", "1",
        ]
    )
    extracted_decoded = work / "stereo_right_extracted_dec.y4m"
    run([str(avmdec), "--codec=av2", "-o", str(extracted_decoded), str(extracted)])
    _, _, extracted_frames = read_y4m(extracted_decoded)
    extraction_matches = (
        len(extracted_frames) == len(lossy_right_frames)
        and all(
            planes_of(extracted) == planes_of(decoded)
            for extracted, decoded in zip(extracted_frames, lossy_right_frames)
        )
    )
    results.append(
        ("extracted layer 1", "matches layer decode" if extraction_matches else "MISMATCH")
    )

    dav2d_note = "not installed"
    if dav2d.is_file():
        dav2d_output = work / "stereo_right_extracted_dav2d.yuv"
        dav2d_run = subprocess.run(
            [
                str(dav2d), "--demuxer", "section5",
                "-i", str(extracted), "-o", str(dav2d_output), "--muxer", "yuv",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        if dav2d_run.returncode == 0 and dav2d_output.is_file():
            dav2d_planes = dav2d_output.read_bytes()
            frame_size = left_width * left_height * 3 // 2
            expected = b"".join(planes_of(frame) for frame in lossy_right_frames)
            dav2d_note = (
                "decodes extracted layer bit-exactly"
                if dav2d_planes == expected
                else f"decoded {len(dav2d_planes) // frame_size} frames, differs"
            )
        else:
            dav2d_note = "failed on extracted layer"
    results.append(("dav2d (singlestream)", dav2d_note))

    # ---------------------------------------------------------------- visuals
    source_views = [(left_width, left_height, left_frames[0]),
                    (right_width, right_height, right_frames[0])]
    decoded_views = [(left_width, left_height, lossy_left_frames[0]),
                     (right_width, right_height, lossy_right_frames[0])]
    stereo_source = work / "stereo_source_atlas.png"
    compose_atlas("row", source_views).save(stereo_source)
    stereo_decoded = work / "stereo_decoded_atlas.png"
    compose_atlas("row", decoded_views).save(stereo_decoded)
    stereo_diff = work / "stereo_diff.png"
    difference_image(source_views, decoded_views).save(stereo_diff)

    parallax = Image.new("RGB", (left_width, left_height))
    left_rgb = yuv_to_rgb(left_frames[0][0], left_frames[0][1], left_frames[0][2],
                          left_width, left_height)
    right_rgb = yuv_to_rgb(right_frames[0][0], right_frames[0][1], right_frames[0][2],
                           right_width, right_height)
    parallax.putdata([
        min(255, abs(a[0] - b[0]) * 4) for a, b in zip(left_rgb.getdata(), right_rgb.getdata())
    ])
    stereo_parallax = work / "stereo_parallax.png"
    parallax.save(stereo_parallax)
    artifacts += [stereo_source, stereo_decoded, stereo_diff, stereo_parallax]

    conference_source_views = [
        (width, height, frames[0]) for _, (width, height, frames) in conference_sources
    ]
    conference_decoded_views = [
        (width, height, frames[0]) for width, height, frames in conference_decoded
    ]
    conference_source = work / "conference_source_atlas.png"
    compose_atlas("column", conference_source_views).save(conference_source)
    conference_decoded_png = work / "conference_decoded_atlas.png"
    compose_atlas("column", conference_decoded_views).save(conference_decoded_png)
    artifacts += [conference_source, conference_decoded_png]

    # ---------------------------------------------------------------- summary
    sweep_lines = []
    if args.sweep:
        for qindex in (0, 100, 150, 200, 255):
            sweep_stream = work / f"sweep_q{qindex}.obu"
            run([str(av2codec), str(left), str(sweep_stream), "--qindex", str(qindex)])
            sweep_decoded = work / f"sweep_q{qindex}.y4m"
            run([str(avmdec), "--codec=av2", "-o", str(sweep_decoded), str(sweep_stream)])
            _, _, sweep_frames = read_y4m(sweep_decoded)
            value = psnr(planes_of(left_frames[0]), planes_of(sweep_frames[0]))
            size = sweep_stream.stat().st_size
            sweep_lines.append(
                f"| {qindex} | {size} | {'lossless' if math.isinf(value) else '%.2f dB' % value} |"
            )

    bitstream_sizes = {
        "stereo, qindex 0": stereo_lossless.stat().st_size,
        f"stereo, qindex {args.qindex}": stereo_lossy.stat().st_size,
        f"conference, qindex {args.qindex}": conference.stat().st_size,
        "extracted layer 1": extracted.stat().st_size,
    }
    lines = ["# Multiview encoder demo", ""]
    lines.append(
        f"Stereo pair: two 1280x704 views, {STEREO_DISPARITY}px horizontal disparity, "
        f"{args.frames} frames, row atlas 2560x704."
    )
    lines.append(
        f"Conference: 960x512 + 512x256 + 448x192 views, {args.frames} frames, "
        f"column atlas 960x960."
    )
    lines.append("")
    lines.append("First temporal unit of the lossless stereo bitstream:")
    lines.append("")
    lines.append("| OBU | tlayer | mlayer | xlayer | bytes |")
    lines.append("|---|---|---|---|---|")
    for name, tlayer, mlayer, xlayer, size in obu_layout(stereo_lossless.read_bytes()):
        lines.append(f"| {name} | {tlayer} | {mlayer} | {xlayer} | {size} |")
        if name == "CLK":
            break
    lines.append("")
    lines.append("| Check | Result |")
    lines.append("|---|---|")
    for name, value in results:
        lines.append(f"| {name} | {value} |")
    lines.append("")
    lines.append("| Bitstream | Bytes |")
    lines.append("|---|---|")
    for name, size in bitstream_sizes.items():
        lines.append(f"| {name} | {size} |")
    lines.append("")
    if sweep_lines:
        lines.append("Quality sweep (single stream, left view, three frames):")
        lines.append("")
        lines.append("| qindex | bytes | PSNR |")
        lines.append("|---|---|---|")
        lines.extend(sweep_lines)
        lines.append("")
    lines.append("Artifacts:")
    for path in artifacts:
        lines.append(f"- `{path.relative_to(Path.cwd()) if path.is_relative_to(Path.cwd()) else path}`")
    summary = work / "summary.md"
    summary.write_text("\n".join(lines) + "\n")

    print("\n".join(lines))
    print(f"\nWork directory: {work}")


if __name__ == "__main__":
    sys.exit(main())
