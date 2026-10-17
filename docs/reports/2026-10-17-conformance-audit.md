# Conformance Audit: av2codec vs AV2 Spec v1.0.0

Reference: `docs/spec/AV2_Spec_v1.0.0.txt` (Final Deliverable v1.0.0, 28 May 2026).
Ground truth decoder/encoder: AVM reference software HEAD
(`github.com/AOMediaCodec/avm`, commit `ce1ec108a82b`, 16 Oct 2026), built
locally with `cmake -DAOM_TARGET_CPU=generic` (no assembly) — `avmdec` and
`avmenc`. Rust 1.99.1 installed; `cargo test`: **330 passed, 4 failed**
(the 4 failures are PSNR tests that hard-code a macOS `avmdec` path).

This audit is empirical where possible: bitstreams were produced by av2codec
and fed to the reference `avmdec`, and bitstreams produced by the reference
`avmenc` were fed to the av2codec decoder. Existing report
`docs/reports/2026-10-17-header-spec-audit.md` was independently re-checked;
several of its claims need correction (see §5).

## 0. Resolution status (2026-10-18, branch `av2-v1.0.0-conformance`)

The findings below are the state at the time of the audit. All blockers in
§1 have since been fixed on the conformance branch and verified against the
same AVM HEAD (`ce1ec108a82b`):

| Item | Resolution |
|---|---|
| Sequence header (§3 E1–E3) | Reordered to §5.4.1, `monotonic_output_order_flag` emitted, tier gated on `seq_level_idx > 3`, Table A.8 levels, `order_hint_bits_minus_1 f(4)`. Encoder and decoder agree; `avmdec` prescan accepts the streams |
| OBU framing (§3 E4–E5) | `ObuType::Padding = 25`; every temporal unit starts with `OBU_TEMPORAL_DELIMITER` (`TD SH CLK [TD CLK]*`) |
| Lossless (qindex 0) | 4x4 TUs and no tx-type bits; per-TU prediction uses the same availability mask as the decoder; inverse WHT for luma **and chroma**; DPCM vertical/horizontal cumulative residuals. `--qindex 0` encodes decode **bit-exactly** via `avmdec` and the internal decoder on solid/gradient/testsrc2/noise and multi-frame inputs |
| Decoder OBU handling (§4 D1) | Scans for the sequence-header OBU; accepts TD-prefixed streams |
| Decoder flat-SB shortcut (§4 D2) | Removed; all frames use the partitioned decoder and failures are explicit errors |
| Decoder lossless (§4 D4) | Reference lossless streams (`avmenc --qp=0`) decode **bit-exactly**, including DPCM blocks |
| Decoder luma rounding (§4 D3) | Caused by applying the directional edge filter when `enable_intra_edge_filter = 0`; the flag is now parsed and honored. Reference qindex>0 streams decode bit-exactly (96×128, 128×96, 128×128, 192×192, 64×64) |
| Tests | Draft-era avmenc fixtures regenerated with current v1.0.0 `avmenc`; `cargo test`: 328 passed / 0 failed (18 ignored integration tests pass with `--ignored`); `cargo clippy --tests`: no errors |

Validation matrix (av2codec encoder, stock `avmdec`):

| Input | q0 | q50/q200 |
|---|---|---|
| solid red 64×64 | bit-exact vs source | bit-exact |
| gradient 64×64 | bit-exact | decodes; avmdec/internal decoder differ by ≤1 LSB on 6 samples |
| testsrc2 64×64 | bit-exact | ≤1 LSB on 21 samples |
| multi-color / multi-gray 3 frames | bit-exact | bit-exact |
| 128×128 noise, 192×192 gradient | bit-exact | luma bit-exact; small chroma ≤1 LSB deltas on noise |

Note on AVM CLI semantics: in this build `--qp=N` is an absolute qindex
(logged as `Q: N`), so a true lossless reference stream is `--qp=0`;
`--qp=120` is a lossy qindex-120 stream, not lossless.

Independent corrections to the header audit (§5) remain valid: the filter
group is already **9 bits**, and the transform/quant group has **12 flags
before `equal_ac_dc_q`**, so neither is a defect.

## 1. Bottom line

| Layer | Verdict |
|---|---|
| OBU framing (`src/bitstream/obu.rs`) | **Correct** (size includes header; leb128; type/extension/tlayer layout). One unused enum value wrong (`Padding = 24`, spec 25) |
| Sequence header | **NOT v1.0.0-conformant** — field order, tier gate, level indices, missing `monotonic_output_order_flag`. The reference decoder rejects the stream at the prescan stage |
| Frame header / CLK payload | **Correct** on the all-intra path — verified byte-for-byte against `avmdec` |
| Tile data, entropy coder, CDF init/adaptation, transform, quantization, intra prediction (encoder) | **Correct for qindex > 0** — after fixing only the sequence header, `avmdec` decodes solid/gradient/noise/multi-frame streams, bit-exact on solid/multi-color frames |
| Lossless encoding (base_q_idx = 0) | **NOT conformant** — `read_tx_size` must force `TX_4X4` for intra lossless blocks; the encoder keeps large transforms and signals tx types, so `avmdec` reports `Failed to decode tile data` |
| Decoder: OBU handling | **NOT conformant** — assumes `obus[0]` is the sequence header; fails on valid streams that start with `OBU_TEMPORAL_DELIMITER` (every reference-encoder stream) |
| Decoder: tile data | **Partially conformant** — a "flat-SB" shortcut hard-codes 32×32 luma / 16×16 chroma transform geometry and silently produces wrong pixels on general streams; the general partitioned path is close (luma off by ≤1–2 on a few percent of samples, chroma bit-exact) |

## 2. What is demonstrably correct (encoder)

Encoded with the *unmodified* repo except a scratch copy whose sequence-header
front matter was reordered to v1.0.0 (§5.4.1), then decoded with stock
`avmdec`:

| Input | Result |
|---|---|
| `solid_red_64x64.y4m`, q50 | decodes; output **bit-identical to source** (Y/U/V) |
| `gradient_64x64.y4m`, q50/q100 | decodes; Y-PSNR 57.2 / 50.4 dB |
| `testsrc2_64x64.y4m`, q50/q100 | decodes; Y-PSNR 49.0 / 41.2 dB |
| `multi_color_64x64_3f.y4m`, q50 | 3 frames decode; **bit-identical to source** |
| `multi_gray_64x64_3f.y4m`, q50 | 3 frames decode; bit-identical |
| 128×128 random noise, q50/q200 | decodes; 47.7 / 14.2 dB Y-PSNR |
| 192×192 gradient, q50/q200 | decodes; 58.6 / 38.7 dB |

This validates, on the all-intra path: frame header field order, tile/partition
syntax as emitted, coefficient syntax, CDF initialization (including
qindex-dependent coefficient CDF banks), CDF adaptation across frames, inverse
transform and dequantization values used by `avmdec`.

`src/encode.rs::ac_qlookup` matches the spec exactly (`Ac_Qlookup` and the
`q >= 25` shift rule), so qindex→quant is spec-conformant.

## 3. Encoder divergences

### E1. Sequence header order is pre-v1.0.0 (BLOCKER)

Spec §5.4.1 order:

`seq_header_id uvlc, seq_profile_idc f(5), single_picture_header_flag f(1),
seq_level_idx f(5), seq_tier f(1) iff (!sph && seq_level_idx > 3),
chroma_format_idc uvlc, bit_depth_idc uvlc, then (if !sph) seq_lcr_id f(3),
still_picture f(1), max_tlayer_id f(2), max_mlayer_id f(3),
monotonic_output_order_flag f(1), then dimensions/crop/display/model flags`.

`src/bitstream/sequence.rs:18-58` instead writes:

`id, profile, sph, seq_lcr_id f(3), still_picture f(1), seq_level_idx,
[tier], dimensions, crop, chroma uvlc, bit_depth uvlc, display flags,
tlayer, mlayer` — and never writes `monotonic_output_order_flag`.

Evidence (reference decoder, current HEAD):

```
$ avmdec --codec=av2 --i420 -o out.yuv /tmp/ours_red.obu
[OBUDBG] type=1 ...
[DXDBG] prescan SH: profile=0 sph=0 level=0 chroma/bitdepth parse FAILED
[DXDBG] frame_unit_size=0 -> error
Warning: Failed to decode frame 1: Unspecified internal error
```

The reference decoder rejects the sequence header before it ever reaches
frame decoding. Patching only the front-matter order (and level index table)
in a scratch copy made every test stream above decode successfully.

### E2. Tier bit gate and level indices (BLOCKER on some sizes)

- `src/bitstream/sequence.rs:5` defines `SEQ_LEVEL_4_0 = 8`; the spec gates the
  tier bit on `seq_level_idx > 3` (§5.4.1) and AVM's `SEQ_LEVEL_4_0` is `4`.
  For frames in the 1360×1360 / 1720×1720 bands (e.g. 1366×768) the encoder
  writes level 4–5 but no tier bit, so the spec decoder reads the next syntax
  element one bit early.
- `compute_level` (`sequence.rs:64-78`) returns AV1-era indices
  (`1360 → 4`, `1720 → 5`, `2560 → 8`, `4975 → 12`). Table A.7/A.8 assign
  `1360 → 2` (3.0), `1720 → 3` (3.1), `2560 → 4` (4.0), `4975 → 6` (5.0),
  `9951 → 10`, `19902 → 14`, `38400 → 18`. `avmdec` validates
  `seq_level_idx` against Annex A (`is_valid_seq_level_idx`,
  `av2_max_level_bitrate`), so wrong levels change conformance semantics for
  large frames.

### E3. `order_hint_bits_minus_1` is written with 3 bits (latent)

`src/bitstream/sequence.rs` writes `write_literal(5, 3)`; spec §5.4.6 requires
`f(4)` (AVM: `avm_rb_read_literal(rb, 4)`). For the current value 5 the 3 bits
`101` plus the following `0` form the correct 4-bit value `0101`, so bit
alignment is accidentally preserved. It breaks for any
`order_hint_bits_minus_1 > 7`.

### E4. `ObuType::Padding = 24` (minor, unused)

`src/bitstream/obu.rs:16`. Table 6.1: 24 = `OBU_CONTENT_INTERPRETATION`,
25 = `OBU_PADDING`.

### E5. Missing temporal delimiter OBUs (minor stream-structure issue)

§7.3.7 requires exactly one `OBU_TEMPORAL_DELIMITER` at the start of every
temporal unit. The encoder emits `SH CLK CLK …` with no TD. `avmdec` tolerates
it, but a strict decoder need not. Adding one TD before the first frame unit
(and before each subsequent temporal unit) is the conformant shape.

### E6. Lossless encoding is not conformant (BLOCKER for qindex 0)

With `base_q_idx = 0`, `CodedLossless = 1`, and the spec forces intra blocks to
`TX_4X4` with no tx-size/tx-type signaling (§5.20.6.1 `read_tx_size`,
§5.20.8.2 `transform_type` → `DCT_DCT`). The inverse transform is the inverse
Walsh–Hadamard transform (§7.15.2.2). `src/tile.rs`/`src/encode.rs` ignore
lossless: they keep `tx_size_sqr_for_bsize(bsize)` and write tx types, so the
tile payload desynchronizes.

Evidence: every `--qindex 0` encode (solid red, gradient, testsrc2, 128×128
noise, 192×192 gradient, multi-frame) is rejected by the reference decoder:

```
Warning: Failed to decode frame 1: Corrupt frame detected
Warning: Additional information: Failed to decode tile data
```

At qindex ≥ 1 the same inputs decode cleanly with `avmdec`. Note
`src/bitstream/frame.rs:57-64` already models `coded_lossless` for the frame
header, but the tile encoder has no corresponding behavior.

## 4. Decoder divergences

Tested by decoding reference-encoder (`avmenc`, built from the same AVM HEAD)
streams with `src/decode`.

### D1. Decoder assumes the sequence header is the first OBU (BLOCKER)

`src/decode/mod.rs:96` calls `parse_sequence_header(obus[0].payload)`.
Reference streams start with `OBU_TEMPORAL_DELIMITER` (type 2, empty payload),
so this panics with an empty-payload index error. §7.3.7 permits/requires TD;
the decoder must scan for `obu_type == 1` instead of assuming position 0.

### D2. "Flat-SB" shortcut decodes general streams incorrectly (BLOCKER)

`src/decode/mod.rs:148-196` selects a shortcut whenever the frame is
64-aligned and `tx_mode_select == 0`, `enable_sdp == 0`, `enable_ibp == 0`.
That path (`src/decode/tile.rs:1393` and `decode_plane_full` callers at
`1455-1520`) hard-codes `bwl: 5`, `height: 32` (luma 32×32) and 16×16 chroma
instead of following the signaled `tx_size`, and asserts `do_split == 0`.
When it panics, the fallback is `catch_unwind` → the general partitioned path,
and if that fails too the decoder silently returns `gray_frame(...)` rather
than an error (`mod.rs:196,223`).

Evidence: a 32-byte reference stream encoding a single 64×64 solid-red block
(q120) decodes to prediction-only values in the 127–129 range (DC from
unavailable neighbours, no residual applied) instead of 81, with exit status 0
and no warning. The same stream decodes correctly with `avmdec`.

### D3. General partitioned path is close to spec but has luma rounding differences (MINOR/major depending on severity policy)

For sizes that force the general path (not 64-aligned), decoding reference
streams gives:

| Stream | Luma vs `avmdec` | Chroma |
|---|---|---|
| 96×128 gradient, q120 | max diff 1, 0.1 % samples differ | bit-exact |
| 128×96 gradient, q120 | max diff 2, 13.9 % samples differ | bit-exact |

Differences start at the second superblock row of the 128×96 frame and are
±1/±2. This is not an entropy-syntax desync (all symbols parsed); it is a
numeric difference in intra prediction or inverse-transform rounding, likely
compounded across rows. Worth a focused numeric audit of `src/decode/predict.rs`
and `src/decode/transform.rs` against §7.13.2 and §7.15.

### D4. Lossless decoding path is not spec-compliant

The decoder shares the encoder's non-spec lossless handling (large transforms
instead of forced `TX_4X4` + Inverse WHT). It round-trips av2codec's own
lossless streams but is not a conformant decoder for lossless AV2 streams.

## 5. Verification of the existing header report

`docs/reports/2026-10-17-header-spec-audit.md` — each claim re-derived from the
spec text and the current code:

| Claim | Verdict |
|---|---|
| OBU framing size-precedes-header, header layout, type codes 1–10 | **CONFIRMED** |
| `ObuType::Padding = 24` should be 25 | **CONFIRMED** (Table 6.1) |
| S1: `seq_lcr_id`/`still_picture` written before `seq_level_idx` | **CONFIRMED** (§5.4.1) |
| S2: tier gated at `level >= 8`, spec wants `> 3` | **CONFIRMED** (AVM `SEQ_LEVEL_4_0 = 4`) |
| S3: `compute_level` uses AV1-era level indices | **CONFIRMED** (Table A.7/A.8) |
| S4: `monotonic_output_order_flag` never written | **CONFIRMED** (§5.4.1) |
| S5: `order_hint_bits_minus_1 f(3)`, spec `f(4)` | **CONFIRMED as a width defect**, but the claimed consequence ("every later inter-group bit shifts") is **REFUTED for the current value 5** — the following 0 bit completes `0101`, so net alignment matches. It only breaks for values > 7 |
| S6: filter group is 8 bits, spec wants 9 | **REFUTED** — current `write_filter_group` writes 7 flags + `df_par_bits_minus_2 f(2)` = 9 bits, matching §5.4.10 for `enable_* = 0`. AVM's `read_sequence_filter_group_tool_flags` consumes exactly 9 here |
| `transform_quant_entropy` group: "11×0 + equal_ac_dc_q + …" | **REFUTED** — §5.4.8 has **12** flags before `equal_ac_dc_q` for 4:2:0/!sph, and the code writes 12 + 1 + 5 + 1 = 19 bits, which AVM also consumes (verified with an instrumented `avmdec`: group ends at the same offset) |
| Chroma/bitdepth after crop; tlayer/mlayer after display flags | **CONFIRMED** |
| Frame header needs no change; every field maps onto §5.18.x | **CONFIRMED** (whole CLK payload decodes on `avmdec` after the SH fix) |
| TD open question | **CONFIRMED as a real conformance gap**, not merely an open question: §7.3.7 requires exactly one TD per temporal unit |

The report's conclusion ("the sequence header is the only blocker") is
**correct for qindex > 0 encoding**, and this audit adds the lossless-encoder
and decoder-side blockers that the report did not cover.

## 6. Recommended fix order

1. **Sequence header** (`src/bitstream/sequence.rs` and
   `src/decode/sequence.rs` together): reorder front matter to §5.4.1; add
   `monotonic_output_order_flag f(1)`; tier gate `seq_level_idx > 3`;
   `order_hint_bits_minus_1 f(4)`; recompute `compute_level` per Table A.8
   (`640→0, 880→1, 1360→2, 1720→3, 2560→4, 4975→6, 9951→10, 19902→14,
   38400→18`); change `SEQ_LEVEL_4_0` to 4.
2. **Decoder OBU scan** (`src/decode/mod.rs`): find the first
   `obu_type == 1` instead of `obus[0]`; ignore `OBU_TEMPORAL_DELIMITER`.
3. **Lossless** (encoder + decoder tile path): force `TxSize = TX_4X4` for
   intra lossless blocks, suppress tx-type signaling, use Inverse WHT, and
   honor the `lossless_tx_size` syntax path when `enable_fsc` / non-intra
   allows it.
4. **Remove the flat-SB shortcut** (`src/decode/mod.rs:148-196`,
   `src/decode/tile.rs:1393-1520`): route 64-aligned frames through the
   general partitioned decoder, and make unsupported syntax an explicit
   `DecodeError` — never `gray_frame` on a valid bitstream.
5. **Decoder numeric audit**: reconcile the ±1/±2 luma differences in
   `src/decode/predict.rs` / `src/decode/transform.rs` against §7.13.2/§7.15,
   using the reference streams generated in this audit.
6. **Minor**: `ObuType::Padding = 25`; emit `OBU_TEMPORAL_DELIMITER` at the
   start of each temporal unit; fix the hard-coded macOS `avmdec` paths in
   `src/decode/mod.rs` tests and `tests/avmdec_integration.rs` so the
   integration tests run on this machine.

## 7. Reproducing the audit

Reference tools used (kept out of the repo):

```sh
git clone --depth 1 https://github.com/AOMediaCodec/avm.git /tmp/avm
cmake -S /tmp/avm -B /tmp/avm/build -G Ninja -DAOM_TARGET_CPU=generic \
      -DCMAKE_BUILD_TYPE=Release
ninja -C /tmp/avm/build avmdec avmenc
/tmp/avm/build/avmdec --codec=av2 --i420 --framestats=/tmp/fs.csv \
      -o /tmp/ref.yuv /tmp/our.obu
/tmp/avm/build/avmenc --codec=av2 --obu -w 96 -h 128 --i420 --limit=1 \
      --end-usage=q --cq-level=30 --enable-cdef=0 --enable-gdf=0 \
      --enable-restoration=0 --enable-ccso=0 --enable-intrabc=0 \
      --enable-palette=0 --enable-tcq=0 --enable-sdp=0 --enable-tx-partition=0 \
      -o /tmp/ref.obu /tmp/grad96x128.y4m
```
