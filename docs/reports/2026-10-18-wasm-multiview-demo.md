# In-browser AV2 multiview (WASM) and the three demo slides

Hosted single-page demo: <https://static.caric.io/vdd26/av2demo>. The slides
live in `demos/slides/` and are served separately.

## What runs in the page

`demos/wasm-multiview/index.html` plus `av2codec.wasm` (717 KB, built from this
crate with `demos/wasm-multiview/build.sh`). No server is involved in the codec path:

1. Three canvas-rendered sports feeds (pitch, court, ring) and the camera are
   cropped to 320x192 RGBA.
2. `av2_convert_rgba` turns each into YUV420 inside the WASM module.
3. `av2_encode_multiview` builds one AV2 temporal unit: TD, MSDO, global LCR,
   2x2 enhanced atlas, then four extended layer units (local LCR, sequence
   header, CLK) at qindex 150.
4. `av2_decode` decodes that temporal unit back to four frames.
5. The page composes them row-major into the 640x384 canvas and paints it.
6. Each temporal unit is kept for up to 120 s; the *Download .obu* button writes
   the concatenated units as `av2-multiview-<timestamp>.obu`. The recorded file
   is a valid AV2 multistream: decoding 80 recorded units (5 s) yields 320
   frames of 320x192, i.e. four layers per temporal unit.

Measured in Chrome on an AMD Ryzen 9 7900X: **15 fps** loop, ~37 ms encode,
~3.6 ms decode, ~3 KB per temporal unit (~330 kb/s), 717 KB wasm module
(single-threaded; the native multiview demo is ~10x faster because it can use
one thread per view).

## Is dav2d able to decode multiview/atlas frames?

No, not semantically. dav2d's OBU parser knows the object types
(`DAV2D_OBU_LAYER_CFG_REC = 16`, `DAV2D_OBU_ATLAS_SEG = 17`,
`DAV2D_OBU_MSDO = 20`) but its switch has no cases for them, so it logs
"Unknown OBU type" and skips them; `xlayer_id`/`mlayer_id` are read from the OBU
header into the frame header, but there is no layer selection, no atlas parsing
and no composition. In practice dav2d decodes our multistream as a sequence of
ordinary frames in bitstream order (one frame per layer per temporal unit), as
observed in the earlier demos (`Decoded 2/1 frames`). Composition therefore has
to happen in the player, exactly as this page does it by index.

## dav2d to WASM

Not built here: this machine has no emscripten. A zig-based wasm32-wasi build of
dav2d was attempted and stops at configure time because wasi-libc lacks
`clock_gettime` (dav2d requires it) and the build pulls in a threads dependency;
it would need `-D_WASI_EMULATED_PROCESS_CLOCKS -lwasi-emulated-process-clocks`,
thread support removal and a WASI-import shim in JS. Emscripten would be the
shorter path: `emsdk` + a meson cross file, then a small C wrapper exposing
`dav2d_send_data`/`dav2d_get_picture` over linear memory. The page keeps the
decoder behind `av2_decode`, so swapping in a dav2d wasm module later only
replaces that call (and composition stays in JS either way).

## Slides

`demos/slides/index.html`, four slides, arrow keys/space to navigate, QR code on
the third slide pointing at <https://static.caric.io/vdd26/av2demo>; the last
slide shows the API cost of the work. The deck is
self-contained: the QR code is inline SVG, so no companion image is needed; use
`demos/slides/make_qr.py` to regenerate it for another URL. The hosted
URL is HTTPS, so remote devices can use their camera; serving the files locally
also works on `http://localhost`, while plain HTTP on a LAN address blocks
`getUserMedia`.

## Camera on a LAN (HTTP) origin

Browsers only expose `getUserMedia` in a **secure context**: `https://` or
`http://localhost`. Serving the page over plain HTTP on a LAN address such as
`http://192.168.1.94:9090` leaves the encoder loop working (placeholder camera
layer) but blocks camera access; the page now detects this, shows a warning and
disables the camera button instead of failing silently.

The `Error with Permissions-Policy header: Unrecognized feature: …` messages in
the console are unrelated Chrome noise about feature names in the server's
headers; they do not affect camera access.

Fix it by serving the demo over TLS:

```sh
demos/serve_tls.sh            # PORT=9090 by default
# then open https://192.168.1.94:9090/ and accept the self-signed certificate
```

The script creates a self-signed certificate whose SANs cover `localhost`, every
local IPv4 address and the machine's other addresses, and serves
`demos/wasm-multiview`. After accepting the certificate warning the origin is
secure and the camera works. Alternatives: forward the port over SSH so the
origin is `http://localhost:9090`, or start Chrome with
`--unsafely-treat-insecure-origin-as-secure=http://192.168.1.94:9090` (desktop
testing only).
