# Architecture

This repository has two clearly separated parts:

- **the codec** (`src/`, the `av2codec` crate): bitstream, encoder, decoder,
  sequence/frame header handling, entropy coding, transforms, and the two core
  command-line tools (`av2codec`, `av2codec_decode`);
- **the demos** (`demos/`, workspace members and scripts): capture, playback,
  composition, hosting and presentation built on top of the public API.

Nothing in `demos/` is required to encode or decode AV2; nothing in the codec
depends on the demos. The same library powers the CLIs, the native demos and the
WASM build.

## Codec overview

```
src/
  lib.rs            public surface: Encoder, Decoder, EncodeError, DecodeError
  encode.rs         intra encoder: partition search, prediction, quantisation
  tile.rs           tile payload: partition tree, intra modes, coefficients
  entropy.rs        Daala range coder
  dct.rs            forward/inverse DCT (4x4 … 64x64)
  scan.rs           coefficient scan orders
  cdf_tables.rs     initial CDF tables
  y4m.rs            Y4M I/O for the CLI and tests
  bin/
    av2codec.rs        encoder CLI
    av2codec_decode.rs decoder CLI
  bitstream/
    writer.rs       bit writer (literals, UVLC, LEB128, trailing bits)
    obu.rs          OBU types, headers, layer scoping, LEB128 framing
    sequence.rs     sequence header payload
    frame.rs        frame header (CLK) payload
    lcr.rs          layer configuration record (global and local)
    atlas.rs        enhanced atlas segment info
    msdo.rs         multi-stream decoder operation
  decode/
    mod.rs          orchestration, reconstruction, fail-closed tool checks
    obu.rs          OBU stream parser (layer-aware)
    sequence.rs     sequence header parsing
    frame.rs        frame header parsing
    tile.rs         coefficient decoding, CDF adaptation
    transform.rs    dequantisation, quant matrices, inverse transforms
    predict.rs      intra prediction
    qm.rs           predefined quantisation matrices
```

### Encoding

1. Frames arrive as YUV420 planes (CLI or library), padded to superblock
   multiples (64).
2. A rate-distortion search picks partitions and intra modes per 64×64
   superblock; residuals go through the integer DCT, quantisation (including
   parity hiding, quantisation matrices and TCQ) and the range coder.
3. Each frame becomes a CLK OBU; a sequence header is emitted first.
4. Multiview input repeats step 3 per extended layer, then wraps the layers in
   one temporal unit: temporal delimiter, MSDO, global LCR, atlas, then each
   layer's local LCR, sequence header and CLK in ascending `obu_xlayer_id`
   order (`src/multiview.rs`).

### Decoding

1. `parse_obus` reads LEB128-sized OBUs, decodes the header extension
   (`obu_tlayer_id`/`obu_mlayer_id`/`obu_xlayer_id`) and infers the global layer
   for temporal delimiters and MSDO.
2. The decoder uses the sequence header and frame headers, then reconstructs
   partitions, coefficients, prediction and transforms.
3. Tools that are not implemented are rejected with
   `DecodeError::UnsupportedFeature` instead of producing wrong pixels
   (in-loop filters, IST, FSC, inter prediction).
4. Metadata OBUs (LCR, atlas, MSDO, OPS, quantisation matrices, padding) are
   accepted and skipped; the decoder is single-layer aware today.

### Multiview model

AV2 describes multiview as independent extended layers plus metadata: the LCR
says what each layer is (view, layer type, atlas segment, priority), the atlas
says where it lands on a virtual canvas, and MSDO ties the streams together.
Composition is informative in the specification, so the codec does not compose;
the demos do it from the layout the encoder declared. See
[docs/reports/2026-10-18-multiview-atlas-layer-composition.md](docs/reports/2026-10-18-multiview-atlas-layer-composition.md).

## Demo architectures

All demos produce one AV2 multistream with a 2×2 atlas (four extended layers)
and compose the decoded layers into a single canvas in row-major order.

### Native multiview and sports demos (`demos/native/`)

```
FFmpeg clips ─► av2codec CLI (--multiview, grid columns) ─► .obu
   .obu ─► avmdec --num-streams=N ─► one Y4M per layer ─► PIL composition ─► PNG/MP4
                                     └─ per-view PSNR against the sources
```

`multiview_demo.py` generates a stereoscopic pair (64 px disparity) and a
three-view conference layout, checks lossless bit-exactness at qindex 0 and
reports PSNR at qindex 150. `sports_multiview_demo.py` prepares four sports
clips (Mixkit Free License) at 640×384/15 fps, encodes a 2×2 wall, decodes with
`avmdec`, writes a composed MP4/GIF, and measures encode throughput (9.9 fps on
one thread, 35.6 fps with one thread per view on a Ryzen 9 7900X).

### Live camera server (`demos/live-camera/`)

```
browser getUserMedia ─► 640×384 RGBA ─► POST /frame
                                        server: RGBA→YUV420, AV2 encode (4 layers),
                                        in-process decode, compose 1280×768
browser <img src="/canvas.mjpg"> ◄──── MJPEG (ffmpeg) ◄── canvas frames
browser /stats ◄────────────────────── fps, encode/decode ms, TU bytes, bitrate
```

The server keeps one temporal unit in flight per frame, paces to
`--fps` (default 15) and binds `127.0.0.1` unless `--bind 0.0.0.0` is given.
It needs FFmpeg for the MJPEG preview and for generating fallback background
clips; sports clips from the sports demo are reused when present. Camera capture
requires `localhost` or HTTPS — see the report for the TLS helper.

### WASM multiview (`demos/wasm-multiview/`)

A separate workspace crate (`av2codec-wasm`) exposes a small C ABI over the
library:

```rust
av2_alloc / av2_free                 // linear-memory buffers
av2_convert_rgba                     // RGBA → YUV420
av2_encode_multiview                 // one temporal unit from N view frames
av2_result_ptr / av2_result_len      // last produced bitstream or decode result
av2_decode                           // bitstream → frames (header + Y/U/V planes)
```

```
canvas feeds + camera ─► RGBA ─► [wasm] YUV420 ─► [wasm] AV2 encode ─► 3 KB TU
        ▲                                                │                  │
        │                                                └─► recording buffer│
        └── JS composition ◄── 4 frames ◄── [wasm] AV2 decode ◄─────────────┘

The recording buffer keeps up to 120 s of temporal units; Download .obu
concatenates them into a normal AV2 multistream file.
```

Views are 320×192 (canvas 640×384), qindex 150. Measured in Chrome: 15 fps,
~37 ms encode, ~3.6 ms decode, 717 KB module, ~330 kb/s. The browser module is
single-threaded; the native demos use one thread per view.

The page composes layers by index. dav2d knows the LCR/atlas/MSDO OBU types but
does not implement them, so it can only emit the layers sequentially; composition
belongs to the player regardless of decoder.

### Slides (`demos/slides/`)

Static four-slide deck (keyboard navigation) with inline SVG QR codes pointing
at the hosted WASM demo and at the source branch, plus a closing slide with the
API cost of the project. The layout targets a 16:9 projector; the
checked-in `av2codec-multiview-slides.pdf` is produced by `print_pdf.mjs`,
which drives Chrome over CDP to fix the 1280x720 layout viewport and prints one
13.333in x 7.5in page per slide. `demos/serve_tls.sh` serves the WASM demo over
HTTPS so remote devices can use their camera.

## Conformance and testing

- `cargo test` runs unit and integration tests; `tests/avmdec_integration.rs`
  and `tests/dav2d_integration.rs` cross-check bitstreams against the reference
  decoders; `tests/multiview.rs` checks per-view bit-exactness and grid layouts
  against `avmdec`.
- Fail-closed behaviour is part of the contract: unsupported tools raise
  `UnsupportedFeature` rather than silently decoding.
- The demos are smoke-tested separately (`demos/wasm-multiview/build.sh`,
  `cargo build -p av2codec-live`, the Python scripts) and are not part of the
  codec's conformance claims.
