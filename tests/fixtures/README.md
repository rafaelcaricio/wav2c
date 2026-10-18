# Test fixtures

## Input clips

| File | Size | Contents |
|---|---|---|
| `solid_red_64x64.y4m` | 64x64 | constant red |
| `solid_green_64x64.y4m` | 64x64 | constant green |
| `solid_gray_64x64.y4m` | 64x64 | constant gray |
| `gradient_64x64.y4m` | 64x64 | 2D gradient |
| `testsrc2_64x64.y4m` | 64x64 | FFmpeg `testsrc2` pattern |
| `multi_color_64x64_3f.y4m` | 64x64, 3 frames | distinct solid colors |
| `multi_gray_64x64_3f.y4m` | 64x64, 3 frames | distinct gray levels |

## Reference bitstreams

`avmenc_*.obu` files are produced by the AVM reference encoder at commit
`ce1ec108a82b` with the all-intra profile used by the decoder tests.  Their
decoded output is asserted bit-exactly in the unit tests:

| Fixture | Tools |
|---|---|
| `avmenc_minimal.obu` | 256x128, qindex 100, single-picture header |
| `avmenc_128x128_q30.obu` | 128x128, qindex 30, tools off |
| `avmenc_128x128_q30_nofilter.obu` | same, IBP enabled |
| `avmenc_128x128_q30_noibc.obu` | same, intrabc disabled |
| `avmenc_lossless_128x128.obu` | qindex 0 (inverse WHT + DPCM) |
| `avmenc_parity_128x128.obu` | parity hiding |
| `avmenc_qm_128x128.obu` | quant matrices |
| `avmenc_mrls_128x128.obu` | multi-line MRL |

Regenerate the reference bitstreams with
`tools/regenerate_avmenc_fixtures.sh` (set `AVMENC` to the reference encoder
binary). The script also synthesizes the 128x128 and 256x128 Y4M inputs it
needs, so it does not depend on files outside this directory.
