# av2codec demos

Runnable examples built on top of the av2codec crate. They are separate from the
codec itself: the library and the two core CLIs live in `src/`, while everything
in this directory is a demo, a hosting helper or a presentation. The technical
background for each one is in [ARCHITECTURE.md](../ARCHITECTURE.md).

| Demo | What it shows | Needs |
|---|---|---|
| `wasm-multiview/` | encoder and decoder compiled to wasm32, running entirely in a browser page | Rust, a local HTTPS server for camera |
| `live-camera/` | native server that encodes your camera as layer 3 of a 2×2 multistream and streams the composed canvas back | Rust, FFmpeg |
| `native/multiview_demo.py` | stereo pair and three-view conference layout, per-view PSNR and atlas images | Python, FFmpeg, `avmdec` |
| `native/sports_multiview_demo.py` | four sports clips in one 2×2 multistream, composed canvas MP4, realtime measurements | Python + Pillow, FFmpeg, `avmdec` |
| `slides/` | three-slide deck with a QR code to the hosted demo | any static server |
| `serve_tls.sh` | serves `wasm-multiview/` over HTTPS on port 9090 (camera needs a secure origin) | Python, OpenSSL |

Build the codec first:

```sh
cargo build --release
```

## WASM multiview (browser only)

```sh
demos/wasm-multiview/build.sh       # cargo build -p av2codec-wasm --target wasm32-unknown-unknown
demos/serve_tls.sh                  # https://<lan-ip>:9090/ (PORT=9443 to change)
```

Open the HTTPS URL, accept the self-signed certificate, press *Enable camera*.
The page captures the camera plus three canvas-rendered sports feeds, encodes
them as one AV2 multistream inside the tab, decodes it and paints the 640×384
canvas. Without a camera it keeps running with a placeholder layer.

Every temporal unit is also kept in the page (up to 120 s) as a session
recording; **Download .obu** writes the concatenated multistream to an
`av2-multiview-<timestamp>.obu` file, which is a normal AV2 bitstream (four
extended layers, LCR and atlas included) that `avmdec`, `dav2d` or
`av2codec_decode` can open.

Camera capture requires a secure origin (`https://` or `http://localhost`); over
plain HTTP the page shows a warning and disables the camera button.

Measured in Chrome: 15 fps, ~37 ms encode, ~4 ms decode, ~3 KB per temporal unit.

## Live camera (native server)

```sh
cargo run -p av2codec-live --release -- --port 8090
# or expose it:  cargo run -p av2codec-live --release -- --port 8090 --bind 0.0.0.0
```

Open <http://localhost:8090/> and press *Enable camera*. The server receives
RGBA frames on `POST /frame`, encodes one temporal unit per frame, decodes it,
composes the 1280×768 canvas and serves it as MJPEG on `GET /canvas.mjpg`;
`GET /stats` exposes timings and bitrate. FFmpeg is required for the MJPEG
preview and for generating fallback background clips. If the sports demo has
been run, its cached clips are used as the three backgrounds.

## Native multiview demo

```sh
AVMDEC=../avm_build/avmdec python3 demos/native/multiview_demo.py --sweep
```

Synthesizes a stereoscopic pair and a three-view conference layout, encodes
them with `av2codec --multiview`, decodes every layer with `avmdec
--num-streams=N`, checks lossless bit-exactness at qindex 0, reports PSNR at
qindex 150, and writes atlas and diff images plus a `summary.md` into
`target/demo/multiview`. See
[the demo report](../docs/reports/2026-10-18-multiview-encoder-demo.md).

## Sports multiview wall

```sh
python3 demos/native/sports_multiview_demo.py
```

Downloads four short sports clips (Mixkit Free License), letterboxes them to
640×384 at 15 fps, encodes a 2×2 multistream, decodes it with `avmdec`, composes
the declared 1280×768 canvas and writes `sports_multiview.mp4`, a GIF preview
and PNG stills to `target/demo/sports`, together with encode/decode timings and
per-view PSNR. See
[the sports report](../docs/reports/2026-10-18-sports-multiview-demo.md).

## Slides

`demos/slides/index.html` is a three-slide deck (arrow keys/space) whose last
slide carries the QR code for the hosted demo. Serve the directory, e.g.:

```sh
python3 -m http.server 8081 --directory demos/slides
```

The hosted copy lives at <https://static.caric.io/vdd26/av2demo>; the QR code on
slide 3 points there. To publish, upload `demos/wasm-multiview/index.html` and
`demos/wasm-multiview/av2codec.wasm` next to each other (the page fetches the
module with a relative path) and serve them over HTTPS so the camera works.
Update the URL in `index.html` and regenerate `qr.png` when hosting elsewhere.

## Notes

- The core encoder and decoder are in `src/`; this directory never contains
  bitstream logic, only orchestration, capture, composition and hosting.
- The WASM module is a thin FFI crate (`demos/wasm-multiview/src/lib.rs`) over
  the same library the native demos use.
- The in-browser and native demos can run without `avmdec`; only the native
  validation scripts cross-check against it.
