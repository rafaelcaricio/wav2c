#!/usr/bin/env python3
"""Sports multiview demo: four sports clips in one AV2 multistream.

Downloads four short royalty-free sports clips, letterboxes them to 640x384 at
15 fps, encodes them as four extended layers of one AV2 multistream with a 2x2
atlas, decodes all layers with avmdec, composes them back into the declared
1280x768 frame canvas, and writes a playable MP4 plus stills. It also measures
encode throughput (sequential and multithreaded) and the decode+compose rate.
"""

import argparse
import json
import math
import shutil
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

from PIL import Image

REPO_ROOT = Path(__file__).resolve().parents[1]
VIEW_WIDTH = 640
VIEW_HEIGHT = 384
COLUMNS = 2
FRAME_RATE = 15
SECONDS = 5
CLIP_LICENSE = "Mixkit Free License (https://mixkit.co/license/)"
CLIPS = [
    {
        "name": "soccer",
        "title": "Goal play in a semi-professional soccer game",
        "url": "https://assets.mixkit.co/videos/43499/43499-360.mp4",
        "start": 0.5,
    },
    {
        "name": "basketball",
        "title": "Skilled basketball player shooting baskets, training alone",
        "url": "https://assets.mixkit.co/videos/44448/44448-360.mp4",
        "start": 0.0,
    },
    {
        "name": "boxing",
        "title": "Intense semi-pro boxing match",
        "url": "https://assets.mixkit.co/videos/40957/40957-360.mp4",
        "start": 0.0,
    },
    {
        "name": "table_tennis",
        "title": "Adult man playing table tennis",
        "url": "https://assets.mixkit.co/videos/17109/17109-360.mp4",
        "start": 3.0,
    },
]
USER_AGENT = "av2codec-sports-demo/0.1 (+https://github.com/rafaelcaricio/wav2c)"


def run(command, quiet=True):
    started = time.monotonic()
    result = subprocess.run(
        command,
        stdout=subprocess.PIPE if quiet else None,
        stderr=subprocess.PIPE if quiet else None,
        text=True,
    )
    elapsed = time.monotonic() - started
    if result.returncode != 0:
        raise SystemExit(
            f"command failed: {' '.join(str(part) for part in command)}\n"
            f"{result.stderr or ''}"
        )
    return elapsed


def download(url, target):
    if target.is_file() and target.stat().st_size > 100_000:
        return
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(request, timeout=120) as response:
        target.write_bytes(response.read())


def probe_size(path):
    result = subprocess.run(
        [
            "ffprobe", "-v", "error",
            "-show_entries", "stream=width,height",
            "-of", "json", str(path),
        ],
        stdout=subprocess.PIPE,
        text=True,
        check=True,
    )
    stream = json.loads(result.stdout)["streams"][0]
    return stream["width"], stream["height"]


def prepare_view(clip, work):
    source = work / f"{clip['name']}_source.mp4"
    download(clip["url"], source)
    view = work / f"view_{clip['name']}.y4m"
    run(
        [
            "ffmpeg", "-v", "error", "-y",
            "-ss", str(clip["start"]), "-i", str(source),
            "-t", str(SECONDS),
            "-frames:v", str(int(SECONDS * FRAME_RATE)),
            "-vf",
            (
                f"scale={VIEW_WIDTH}:{VIEW_HEIGHT}:force_original_aspect_ratio=decrease,"
                f"pad={VIEW_WIDTH}:{VIEW_HEIGHT}:(ow-iw)/2:(oh-ih)/2:color=black,"
                f"fps={FRAME_RATE}"
            ),
            "-pix_fmt", "yuv420p",
            str(view),
        ]
    )
    return view


def read_y4m(path):
    data = Path(path).read_bytes()
    header_end = data.index(b"\n") + 1
    header = data[:header_end].decode("ascii").split()
    width = int(header[1][1:])
    height = int(header[2][1:])
    luma_size = width * height
    chroma_size = (width // 2) * (height // 2)
    frames = []
    offset = header_end
    while offset < len(data):
        offset += len(b"FRAME\n")
        y = data[offset:offset + luma_size]
        offset += luma_size
        u = data[offset:offset + chroma_size]
        offset += chroma_size
        v = data[offset:offset + chroma_size]
        offset += chroma_size
        frames.append((y, u, v))
    return width, height, frames


def write_y4m(path, width, height, frames, rate):
    with open(path, "wb") as handle:
        handle.write(
            f"YUV4MPEG2 W{width} H{height} F{rate}:1 Ip A1:1 C420jpeg\n".encode()
        )
        for y, u, v in frames:
            handle.write(b"FRAME\n")
            handle.write(y)
            handle.write(u)
            handle.write(v)


def compose_canvas(views, columns):
    """views: list of (width, height, frames); row-major grid placement."""
    rows = len(views) // columns
    column_widths = [
        max(views[row * columns + column][0] for row in range(rows))
        for column in range(columns)
    ]
    row_heights = [
        max(views[row * columns + column][1] for column in range(columns))
        for row in range(rows)
    ]
    canvas_width = sum(column_widths)
    canvas_height = sum(row_heights)
    column_offsets = [sum(column_widths[:column]) for column in range(columns)]
    row_offsets = [sum(row_heights[:row]) for row in range(rows)]
    frame_count = len(views[0][2])

    canvas_frames = []
    for index in range(frame_count):
        canvas_y = bytearray(canvas_width * canvas_height)
        canvas_u = bytearray((canvas_width // 2) * (canvas_height // 2))
        canvas_v = bytearray((canvas_width // 2) * (canvas_height // 2))
        for view_index, (width, height, frames) in enumerate(views):
            column = view_index % columns
            row = view_index // columns
            left = column_offsets[column]
            top = row_offsets[row]
            y, u, v = frames[index]
            for line in range(height):
                source = line * width
                target = (top + line) * canvas_width + left
                canvas_y[target:target + width] = y[source:source + width]
            chroma_width = width // 2
            chroma_height = height // 2
            for line in range(chroma_height):
                source = line * chroma_width
                target = (top // 2 + line) * (canvas_width // 2) + left // 2
                canvas_u[target:target + chroma_width] = u[source:source + chroma_width]
                canvas_v[target:target + chroma_width] = v[source:source + chroma_width]
        canvas_frames.append((bytes(canvas_y), bytes(canvas_u), bytes(canvas_v)))
    return canvas_width, canvas_height, canvas_frames


def planes_of(frame):
    return b"".join(frame)


def psnr(reference, decoded):
    squared_error = 0
    for ref_byte, dec_byte in zip(reference, decoded):
        difference = ref_byte - dec_byte
        squared_error += difference * difference
    if squared_error == 0:
        return math.inf
    mse = squared_error / len(reference)
    return 10.0 * math.log10(255.0 * 255.0 / mse)


def yuv_to_rgb(frame, width, height):
    y, u, v = frame
    luma = Image.frombytes("L", (width, height), y)
    chroma_width, chroma_height = width // 2, height // 2
    u_image = Image.frombytes("L", (chroma_width, chroma_height), u).resize(
        (width, height), Image.BICUBIC
    )
    v_image = Image.frombytes("L", (chroma_width, chroma_height), v).resize(
        (width, height), Image.BICUBIC
    )
    return Image.merge("YCbCr", (luma, u_image, v_image)).convert("RGB")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work-dir", default=str(REPO_ROOT / "target/demo/sports"))
    parser.add_argument("--av2codec", default=str(REPO_ROOT / "target/release/av2codec"))
    parser.add_argument("--avmdec", default=str(REPO_ROOT.parent / "avm_build/avmdec"))
    parser.add_argument("--qindex", type=int, default=150)
    parser.add_argument("--threads", type=int, default=4)
    parser.add_argument("--skip-download", action="store_true")
    args = parser.parse_args()

    work = Path(args.work_dir).resolve()
    work.mkdir(parents=True, exist_ok=True)
    clips_dir = work / "clips"
    clips_dir.mkdir(exist_ok=True)
    av2codec = Path(args.av2codec).resolve()
    avmdec = Path(args.avmdec).resolve()
    if not av2codec.is_file():
        print("building av2codec (release)...", file=sys.stderr)
        run(["cargo", "build", "--release", "--bin", "av2codec"], quiet=False)
    if not avmdec.is_file():
        raise SystemExit(f"missing avmdec binary at {avmdec}")

    views = []
    for clip in CLIPS:
        if args.skip_download:
            source = clips_dir / f"{clip['name']}_source.mp4"
            if not source.is_file():
                raise SystemExit(f"missing clip {source}")
            view = work / f"view_{clip['name']}.y4m"
            if not view.is_file():
                view = prepare_view(clip, clips_dir)
        else:
            view = prepare_view(clip, clips_dir)
        views.append((clip, view))

    inputs = [str(view) for _, view in views]
    bitstream = work / f"sports_q{args.qindex}.obu"

    sequential_time = run(
        [
            str(av2codec), *inputs, str(bitstream),
            "--multiview", "--layout", "grid", "--columns", str(COLUMNS),
            "--purpose", "multiview", "--qindex", str(args.qindex),
            "--threads", "1",
        ]
    )
    parallel_time = run(
        [
            str(av2codec), *inputs, str(bitstream),
            "--multiview", "--layout", "grid", "--columns", str(COLUMNS),
            "--purpose", "multiview", "--qindex", str(args.qindex),
            "--threads", str(args.threads),
        ]
    )

    source_views = []
    for _, view in views:
        width, height, frames = read_y4m(view)
        source_views.append((width, height, frames))
    temporal_units = len(source_views[0][2])

    decode_prefix = work / "decoded"
    decode_time = run(
        [
            str(avmdec), "--codec=av2",
            f"--num-streams={len(views)}",
            "-o", str(decode_prefix) + ".y4m",
            str(bitstream),
        ]
    )
    decoded_views = []
    for index in range(len(views)):
        width, height, frames = read_y4m(f"{decode_prefix}_{index}.y4m")
        decoded_views.append((width, height, frames))

    started = time.monotonic()
    canvas_width, canvas_height, canvas_frames = compose_canvas(decoded_views, COLUMNS)
    compose_time = time.monotonic() - started
    canvas_path = work / "sports_multiview.y4m"
    write_y4m(canvas_path, canvas_width, canvas_height, canvas_frames, FRAME_RATE)
    video_path = work / "sports_multiview.mp4"
    run(
        [
            "ffmpeg", "-v", "error", "-y",
            "-i", str(canvas_path),
            "-c:v", "libx264", "-preset", "medium", "-crf", "20",
            "-pix_fmt", "yuv420p", str(video_path),
        ]
    )
    preview_path = work / "sports_multiview_preview.gif"
    run(
        [
            "ffmpeg", "-v", "error", "-y",
            "-i", str(video_path), "-t", "3",
            "-vf", "fps=12,scale=640:-1:flags=lanczos",
            str(preview_path),
        ]
    )

    stills = []
    for second in (0.5, 2.5, 5.0):
        frame_index = min(int(second * FRAME_RATE), len(canvas_frames) - 1)
        still = work / f"sports_canvas_{second:0.1f}s.png"
        yuv_to_rgb(canvas_frames[frame_index], canvas_width, canvas_height).save(still)
        stills.append(still)

    view_stills = []
    for index, (width, height, frames) in enumerate(decoded_views):
        still = work / f"sports_view_{index}.png"
        yuv_to_rgb(frames[min(30, len(frames) - 1)], width, height).save(still)
        view_stills.append(still)

    psnr_values = []
    for (clip, view), (width, height, frames), (_, _, decoded) in zip(
        views, source_views, decoded_views
    ):
        psnr_values.append(
            (clip["name"], psnr(planes_of(frames[0]), planes_of(decoded[0])))
        )

    results = {
        "temporal_units": temporal_units,
        "views": len(views),
        "view_size": f"{VIEW_WIDTH}x{VIEW_HEIGHT}",
        "canvas": f"{canvas_width}x{canvas_height}",
        "bitstream_bytes": bitstream.stat().st_size,
        "encode_sequential_fps": temporal_units / sequential_time,
        "encode_parallel_fps": temporal_units / parallel_time,
        "threads": args.threads,
        "decode_seconds": decode_time,
        "compose_seconds": compose_time,
        "decode_compose_fps": temporal_units / (decode_time + compose_time),
        "psnr": psnr_values,
    }

    summary = ["# Sports multiview demo", ""]
    summary.append(
        f"{len(views)} views of {VIEW_WIDTH}x{VIEW_HEIGHT} at {FRAME_RATE} fps, "
        f"{temporal_units} temporal units, {canvas_width}x{canvas_height} atlas "
        f"({COLUMNS} columns), qindex {args.qindex}."
    )
    summary.append("")
    summary.append("| Metric | Value |")
    summary.append("|---|---|")
    summary.append(f"| bitstream | {results['bitstream_bytes']} bytes |")
    summary.append(
        f"| encode, 1 thread | {results['encode_sequential_fps']:.1f} multiview fps |"
    )
    summary.append(
        f"| encode, {args.threads} threads | {results['encode_parallel_fps']:.1f} multiview fps |"
    )
    summary.append(f"| avmdec decode | {decode_time:.2f} s |")
    summary.append(f"| canvas composition | {compose_time:.2f} s |")
    summary.append(
        f"| decode + compose | {results['decode_compose_fps']:.1f} canvas fps |"
    )
    summary.append("")
    summary.append("| View | PSNR |")
    summary.append("|---|---|")
    for name, value in psnr_values:
        summary.append(f"| {name} | {value:.2f} dB |")
    summary.append("")
    summary.append("Clips:")
    for clip in CLIPS:
        summary.append(f"- {clip['name']}: {clip['title']} ({CLIP_LICENSE})")
        summary.append(f"  {clip['url']}")
    summary.append("")
    summary.append("Artifacts:")
    for path in [video_path, preview_path, canvas_path, *stills, *view_stills]:
        summary.append(f"- `{path}`")
    summary_path = work / "results.md"
    summary_path.write_text("\n".join(summary) + "\n")

    print("\n".join(summary))


if __name__ == "__main__":
    sys.exit(main())
