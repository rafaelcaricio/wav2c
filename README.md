# av2codec

A clean-room AV2 video encoder and decoder written in pure safe Rust.

av2codec encodes raw YUV video into [AV2 (AOM Video 2)](https://aomedia.org/) bitstreams and decodes them back. Encoded bitstreams are compatible with the reference [avmdec](https://gitlab.com/AOMediaCodec/avm) decoder. No unsafe code, no C dependencies, no assembly — just Rust.

## Quick start

```sh
# Build
cargo build --release

# Encode a Y4M file to AV2
cargo run --release --bin av2codec -- input.y4m output.obu

# Decode back to Y4M (using the built-in decoder)
cargo run --release --bin av2codec_decode -- output.obu -o decoded.y4m

# Or decode with the reference decoder
avmdec --i420 -o decoded.yuv output.obu
```

## Conformance and supported tools

Bitstreams are validated against two independent reference decoders:
[AVM](https://github.com/AOMediaCodec/avm) (`avmdec`) and
[dav2d](https://code.videolan.org/videolan/dav2d) (`dav2d`). CI builds both at
pinned commits and runs the cross-decoder tests.

Supported on the intra path (profile 0, 8-bit 4:2:0):

| Area | Status |
|---|---|
| OBU framing, sequence/frame headers, temporal delimiters | Supported |
| Partition trees, SDP, rectangular/extended partitions | Supported |
| Intra modes, directional prediction, IDIF, MRLS incl. multi-line MRL | Supported |
| CfL, MHCCP, palette, CCTX, TCQ | Supported where exercised by tests |
| Quantization, quant matrices, parity hiding | Supported |
| Lossless (inverse WHT + DPCM) | Supported |
| Secondary transform (IST), DIP, frequency-selective coefficient coding | **Rejected explicitly** (fail closed) |
| CDEF, GDF, restoration, CCSO, deblocking | **Rejected explicitly** (fail closed) |
| Layer config record, atlas segment, operating point set, MSDO, metadata OBUs | Parsed and skipped (single extended layer, no composition) |
| Multiview / atlas-tiled encoding (2-4 layers, LCR + atlas + MSDO) | Supported |
| Inter prediction | Not implemented |

Unsupported tools return `DecodeError::UnsupportedFeature` instead of silently
producing incorrect pixels. Multiview and atlas-tiled content is described in
[the layer composition report](docs/reports/2026-10-18-multiview-atlas-layer-composition.md):
each view is an independent extended layer, and composition is informative
(Annex D), so the decoder accepts the metadata of such streams but does not
render them.

## Usage

### CLI

#### Encoding

```
av2codec <input.y4m> [<view.y4m> ...] <output.obu> [options]
```

| Argument | Description |
|---|---|
| `input.y4m` | Input video in Y4M format (8-bit 4:2:0 I420) |
| `view.y4m` | Additional views, one extended layer each (requires `--multiview`) |
| `output.obu` | Output AV2 bitstream (OBU format) |
| `--qindex N` | Base quantizer index, 0-255 (default: 50) |
| `--multiview` | Encode every input as an extended layer of one multistream bitstream |
| `--layout row\|column` | Atlas grid direction for multiview (default: `row`) |
| `--purpose stereo\|multiview` | Global LCR purpose id (default by view count) |
| `--extract-layer N` | Write only extended layer `N` of the encoded multistream |

Stereoscopic pair:

```sh
av2codec left.y4m right.y4m stereo.obu --multiview --qindex 0
avmdec --codec=av2 --num-streams=2 -o views.y4m stereo.obu
av2codec left.y4m right.y4m left.obu --multiview --extract-layer 0
```

A full end-to-end demo (stereo pair and three-view conference layout, encode,
per-view decode with `avmdec`, PSNR and atlas snapshots) is in
[the multiview demo report](docs/reports/2026-10-18-multiview-encoder-demo.md);
run it with `python3 tools/multiview_demo.py --sweep`.

#### Decoding

```
av2codec_decode <input.obu> [-o <output.y4m>]
```

| Argument | Description |
|---|---|
| `input.obu` | Input AV2 bitstream |
| `-o output.y4m` | Output Y4M file (default: stdout) |

### Library API

#### Encoding (streaming)

```rust
use av2codec::Encoder;

let mut enc = Encoder::new(640, 480, 50)?;
let mut output = enc.headers();

for frame in frames {
    enc.send_frame(&frame.y, &frame.u, &frame.v)?;
    let pkt = enc.receive_packet().unwrap();
    output.extend_from_slice(&pkt.data);
}

enc.flush();
std::fs::write("output.obu", &output)?;
```

#### Decoding

```rust
use av2codec::{Decoder, DecodedFrame, DecodeError};

let bitstream = std::fs::read("input.obu")?;
let frames: Vec<DecodedFrame> = Decoder::decode_all(&bitstream)?;

for frame in &frames {
    println!(
        "Frame {}: {}x{}, Y={} bytes, U={} bytes, V={} bytes",
        frame.frame_number,
        frame.width,
        frame.height,
        frame.y.len(),
        frame.u.len(),
        frame.v.len(),
    );

    // Access raw pixel planes directly
    let y_pixel = frame.y[0];      // top-left luma pixel
    let u_pixel = frame.u[0];      // top-left Cb pixel (half resolution)
    let v_pixel = frame.v[0];      // top-left Cr pixel (half resolution)
}
```

The `DecodedFrame` struct provides direct access to the decoded YUV 4:2:0 planes:

| Field | Type | Description |
|---|---|---|
| `y` | `Vec<u8>` | Luma plane (width x height) |
| `u` | `Vec<u8>` | Cb chroma plane (width/2 x height/2) |
| `v` | `Vec<u8>` | Cr chroma plane (width/2 x height/2) |
| `width` | `u32` | Frame width in pixels |
| `height` | `u32` | Frame height in pixels |
| `frame_number` | `u64` | Sequential frame index (0-based) |

#### Encode-decode roundtrip

```rust
use av2codec::{Encoder, Decoder};

// Encode
let mut enc = Encoder::new(128, 128, 50).unwrap();
let mut bitstream = enc.headers();

let y = vec![149u8; 128 * 128];
let u = vec![43u8; 64 * 64];
let v = vec![21u8; 64 * 64];
enc.send_frame(&y, &u, &v).unwrap();
bitstream.extend_from_slice(&enc.receive_packet().unwrap().data);

// Decode
let frames = Decoder::decode_all(&bitstream).unwrap();
assert_eq!(frames.len(), 1);
assert_eq!(frames[0].width, 128);
assert_eq!(frames[0].height, 128);
```

See `examples/encode.rs` for a complete working example that reads raw I420 frames from stdin:

```sh
ffmpeg -f lavfi -i testsrc2=size=64x64:duration=1:rate=25 \
  -pix_fmt yuv420p -f rawvideo - | cargo run --example encode -- output.obu

# Decode with the built-in decoder
cargo run --release --bin av2codec_decode -- output.obu -o decoded.y4m

# Or decode with the reference decoder
avmdec --i420 -o decoded.yuv output.obu
```

## What it does

av2codec reads Y4M video frames and produces an all-intra AV2 bitstream where every frame is an independent keyframe. It also includes a pure Rust decoder that can decode any AV2-compliant bitstream (not limited to av2codec-produced streams).

The encoding pipeline for each frame:

1. Read YUV 4:2:0 pixel data (Y, U, V planes)
2. Select best intra prediction mode per superblock (DC, Smooth, Smooth_V, Smooth_H, Paeth)
3. Compute residual against the selected prediction
4. Forward 2D DCT (64x64 for Y, 32x32 for U/V)
5. Quantization using AV2 dequant tables
6. Entropy-code coefficients using Daala arithmetic coding (base levels, BR, sign, adaptive HR)
7. Pack into a CLK OBU (implicit keyframe)

The decoding pipeline reverses these steps:

1. Parse OBU stream (sequence header, CLK frames)
2. Decode entropy-coded coefficients using Daala arithmetic range decoder
3. Dequantize coefficient levels
4. Inverse 2D DCT to reconstruct residuals
5. Apply intra prediction and add residuals, clamp to [0, 255]

A sequence header OBU is prepended once at the start of the bitstream. Multiview
streams wrap each coded layer in an extended-layer unit and describe the layout
with a global LCR, per-layer local LCRs and an atlas segment OBU (see the
[multiview report](docs/reports/2026-10-18-multiview-atlas-layer-composition.md)).

## Current capabilities

- Profile 0, 8-bit, 4:2:0 (YCbCr)
- All-intra encoding (every frame is a keyframe)
- Intra prediction: DC, Smooth, Smooth_V, Smooth_H, Paeth (best mode selected per superblock)
- Full DC + AC coefficient coding with forward/inverse DCT
- Multi-frame video encoding (each frame as a CLK OBU)
- Multiview encoding: 2-4 extended layers in one bitstream, with a global layer configuration record, per-layer local records (`lcr_view_type`, `lcr_layer_type`, atlas segment mapping), an enhanced atlas (row/column grid, mixed resolutions) and a multi-stream decoder operation OBU
- Per-layer sub-bitstream extraction (`extract_layer`) for singlestream playback
- **Pure Rust decoder** — decodes any AV2-compliant bitstream, not just av2codec-produced ones
- Spec-compliant sequence header parsing (both `single_picture_header_flag=0` and `=1`)
- Streaming encoder API (`send_frame` / `receive_packet`)
- Batch encoding API (`encode_video_yuv`) and decoding API (`Decoder::decode_all`)
- Y4M input parsing and output writing
- Daala-derived arithmetic range coder/decoder for entropy coding
- OBU bitstream packaging with LEB128 size framing

## Limitations

- Dimensions must be multiples of 64 (superblock aligned)
- Fixed QP only (no rate control)
- 8-bit 4:2:0 only
- No deblocking filters (CDEF/GDF) — expect visible block boundaries at higher quantization
- The decoder is single-layer aware: layer configuration and atlas metadata are accepted but not interpreted, so it cannot select or compose views
- Multiview composition, resampling and operating-point selection are not implemented

## Testing

Unit and integration tests:

```sh
cargo test
```

Integration tests that encode video, decode with avmdec, and verify pixel values:

```sh
# Requires avmdec built at ../avm_build/avmdec
cargo test --test avmdec_integration -- --ignored
```

The integration suite covers single-frame, multi-frame, multiple colors (green, red, gray), all three YUV planes, gradient/testsrc2 patterns with PSNR validation, decoder agreement between av2codec and avmdec, and CLI roundtrip encoding. `tests/multiview.rs` encodes stereoscopic, three-view, multi-frame, mixed-resolution and auxiliary-layer streams and checks that `avmdec --num-streams=N` returns every view bit-exactly, and that extracted singlestreams decode with both decoders.

Visual encoder/decoder matrix with burned-in labels:

```sh
# Requires uv, ffmpeg, avmenc, and avmdec
./scripts/compare_codec_matrix.py -s 256x256 -q 30 -o /tmp/codec_matrix.png photo.jpg
```

The script:

- Converts one input image into a single prepared Y4M frame
- Encodes that same frame with both `avmenc` and `av2codec`
- Decodes both bitstreams with both `avmdec` and `av2codec_decode`
- Produces a labeled 2x3 PNG grid with:
  - top row: `Original`, `avmenc -> avmdec`, `avmenc -> av2codec`
  - bottom row: `Original`, `av2codec -> avmdec`, `av2codec -> av2codec`

Useful flags:

- `-s WxH` resizes and center-crops the source image before encode
- `-q N` applies the same quality value to `avmenc --qp` and `av2codec --qindex`
- `--cpu-used N` controls avmenc speed
- `--profile debug|release` selects the av2codec cargo profile for the run (`debug` by default for faster iteration)
- `--cache-dir DIR` keeps prepared input, `avmenc.obu`, and `avmenc -> avmdec` artifacts stable across runs
- `--watch` reruns on source changes and keeps going after build/decode failures
- `--metrics-text PATH` and `--metrics-json PATH` write per-run numeric summaries next to the matrix PNG
- `--scale-up N` enlarges the comparison cells for easier inspection
- `--keep-workdir` preserves intermediate OBUs, Y4Ms, PNGs, and logs
- `--no-build` skips rebuilding `av2codec` binaries for the selected profile

Fast inner loop during encoder or decoder work:

```sh
./scripts/compare_codec_matrix.py \
  -s 192x192 \
  -q 30 \
  --profile debug \
  --cache-dir /tmp/av2codec_iter/jellyfish \
  --watch \
  -o /tmp/av2codec_iter/jellyfish/matrix.png \
  photo.jpg
```

That loop:

- reuses the prepared probe frame plus cached `avmenc -> avmdec` reference artifacts
- rebuilds only `av2codec` and `av2codec_decode`
- refreshes `avmenc -> av2codec`, `av2codec -> avmdec`, and `av2codec -> av2codec` on every save
- writes `matrix.png`, `matrix.txt`, and `matrix.json` so visual inspection is paired with PSNR/MAE and bitstream-size deltas

Use `--profile release` for final confirmation, then run the normal regression gate:

```sh
cargo test
cargo clippy --tests
cargo test --test avmdec_integration -- --ignored
```

See:

```sh
./scripts/compare_codec_matrix.py --help
```

## Project structure

```
src/
  lib.rs              Crate root, module declarations, re-exports
  encode.rs           Streaming Encoder, batch API, quantization math
  dct.rs              Forward and inverse 2D DCT (32x32, 64x64)
  tile.rs             Tile data: partition, intra mode, coefficient coding
  entropy.rs          Daala arithmetic range coder (od_ec_enc port)
  scan.rs             Coefficient scan order tables
  cdf_tables.rs       Initial CDF tables for entropy coding
  y4m.rs              Y4M file parser
  bin/
    av2codec.rs           Encoder CLI binary
    av2codec_decode.rs    Decoder CLI binary
  decode/
    mod.rs            Decoder orchestrator, frame reconstruction
    bitreader.rs      Bit-level reader (MSB-first, UVLC, LEB128)
    entropy.rs        Daala arithmetic range decoder
    obu.rs            OBU stream parser
    sequence.rs       Sequence header decoder
    frame.rs          Frame header decoder
    tile.rs           Tile coefficient decoder (base, BR, sign, HR)
    transform.rs      Dequantization and inverse DCT
  bitstream/
    mod.rs            Bitstream module re-exports
    writer.rs         Bit-level writer (literals, UVLC, LEB128)
    obu.rs            OBU header and LEB128 size framing
    sequence.rs       Sequence header payload
    frame.rs          Frame header (CLK OBU) payload
examples/
  encode.rs           Streaming API example (solid-color frames)
tests/
  avmdec_integration.rs   Encode-decode roundtrip tests
  fixtures/               Y4M test inputs (solid colors, gradients, patterns)
docs/
  plans/                  Design documents and implementation plans
```

## Building the reference decoders

```sh
git clone --depth 1 https://github.com/AOMediaCodec/avm.git ../avm_ref
cmake -S ../avm_ref -B ../avm_build -G Ninja \
  -DAOM_TARGET_CPU=generic -DCMAKE_BUILD_TYPE=Release
ninja -C ../avm_build avmdec avmenc

git clone --depth 1 https://code.videolan.org/videolan/dav2d.git ../dav2d
meson setup ../dav2d/build ../dav2d --buildtype release
ninja -C ../dav2d/build
```

## Roadmap

| Phase | Description | Status |
|---|---|---|
| 0 | Infrastructure (bitwriter, OBU, headers, entropy coder) | Done |
| 1 | Multi-frame all-intra with DC coefficients | Done |
| 2 | AC coefficient support | Done |
| 3 | Pure Rust decoder | Done |
| — | Spec-compliant sequence header parsing | Done |
| — | Intra prediction modes (DC, Smooth, Paeth) | Done |

## License

MIT
