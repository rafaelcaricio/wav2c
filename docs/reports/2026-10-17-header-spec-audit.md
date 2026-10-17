# Header Audit: Encoder vs AV2 Spec v1.0.0 (Final, 2026-05-28)

Local reference: `docs/spec/` (PDF + HTML + txt of the official
*AV2 Bitstream & Decoding Process Specification*, Final Deliverable v1.0.0,
28 May 2026). Cross-checked against **current AVM reference software HEAD**
(github.com/AOMediaCodec/avm, `av2/decoder/obu.c`, `av2/decoder/decodeframe.c`,
`av2/common/enums.h`) and against the reference `avmenc` fixtures in
`tests/fixtures/` (hand-parsed bit-by-bit).

## Bottom line

| Layer | Verdict |
|---|---|
| OBU framing (size, header byte, type codes) | **CORRECT** (one unused enum off-by-one) |
| Frame header / CLK payload | **CORRECT** — bit-exact conformance for the all-intra path |
| Sequence header | **NOT v1.0.0-conformant** — the project implements a pre-final draft order; a v1.0.0 decoder (incl. current avmdec HEAD) misparses it |

The project's encoder, its own decoder, and the avmenc fixtures (Feb–Mar 2026
era) all agree with each other — they implement the **public draft** syntax
(see av2.aomedia.org/v13-public syntax browser). Between that draft and the
May 2026 final v1.0.0, the sequence-header front matter was reordered and
several fields changed. Current avm HEAD (`SEQ_LEVEL_4_0 = 4` in
`av2/common/enums.h`, `read_sequence_header_obu` in `av2/decoder/obu.c`)
parses the **final** order.

## What is correct (verified field-by-field)

### OBU layer (`src/bitstream/obu.rs`)
- `[leb128 obu_size][header byte][payload]` with size preceding the header — matches `open_bitstream_unit(sz)` where `obuPayloadSize = sz - 1 - obu_header_extension_flag` (§5.2.1).
- Header byte layout `extension(1) | type(5) | tlayer(2)` — matches §5.2.2.
- Type codes vs Table 6.1: SH=1 ✓, TD=2 ✓, MFH=3 ✓, CLK=4 (=OBU_CLOSED_LOOP_KEY) ✓, OLK=5 ✓, LTG=6 ✓, RTG=7 ✓, MetadataShort=8 ✓, MetadataGroup=9 ✓, Switch=10 ✓.
- **Minor:** `ObuType::Padding = 24` (obu.rs:16) — Table 6.1: 24 = OBU_CONTENT_INTERPRETATION, **25 = OBU_PADDING**. Unused today.

### Sequence header — config groups (`src/bitstream/sequence.rs`)
- `partition` group: 5 bits (sb=64, sdp=0, ext_partitions=0, aspect flag) — matches §5.4.3 incl. Monochrome gating.
- `segment` group: 2 bits — matches §5.4.4.
- `intra` group: 4+2+2 bits — matches §5.4.5 bit-exact.
- `transform_quant_entropy` group (sequence.rs:182): 11×0 + `equal_ac_dc_q=1` + `base_uv_ac_delta_q f(5)=23` + `uv_ac_delta_q_enabled=0` — matches §5.4.8 exactly (`BaseUVAcDeltaQ = 0` via `DELTA_DCQUANT_MIN = -23`; all per-plane delta-q enables off → `quantization_params()` reduces to `base_q_idx f(8)` only, §5.18.6.1).
- Tail: `seq_tile_info_present=0`, `film_grain_params_present=0`, `obu_extension_flag=0` + `trailing_bits` — matches §5.2.1 extensible-OBU rule + §5.4.1.
- `seq_header_id uvlc(0)`, `seq_profile_idc f(5)=0`, `single_picture_header_flag=0`, `chroma_format_idc uvlc(0)` (=4:2:0, Table 6.2), `bit_depth_idc uvlc(1)` (=**8-bit**, Table 6.3: 0→10-bit, 1→8-bit) — all correct values.

### Frame header / CLK OBU payload (`src/bitstream/frame.rs`)
For the CLK key-frame path (§5.19 tile_group_obu, §5.18.2 frame_header_info):
- `is_first_tile_group f(1)=1` (first bit of CLK payload) ✓
- `cur_mfh_id uvlc(0)` (no MFH OBUs — legal, cur_mfh_id==0 path) ✓
- `seq_header_id_in_frame_header uvlc(0)` ✓
- `long_term_id_plus_1 f(3)=0` ✓ (matches `long_term_frame_id_bits=3` signaled in seq inter group)
- `immediate_output_frame f(1)=1` ✓
- `frame_size_override_flag f(1)=0` ✓ (→ `frame_size()` reads no bits, §5.18.4.1)
- `order_hint f(6)` ✓ (= `order_hint_bits_minus_1(5)+1`)
- KEY intra ⇒ `refresh_frame_flags` derived (CLK & max_mlayer_id==0 ⇒ allFrames), no bits ✓
- `screen_content_params()`: no bits (force=0 ≠ SELECT) ✓; `intrabc_params()`: `allow_intrabc f(1)=0` ✓ (the bit at frame.rs:41)
- `disable_cdf_update f(1)` ✓
- `tile_info()` → `tile_params()`: `uniform_tile_spacing_flag=1` + one terminating `0` per dimension; no `context_update_tile_id`/`tile_size_bytes_minus_1` (single tile) ✓ (§5.18.7.2/5.18.7.3) — 1 bit @64x64, 3 bits @128x128 as unit-tested
- `quantization_params()`: `base_q_idx f(8)` only ✓
- `segmentation_params f(1)=0` ✓, `setup_qm_params f(1)=0` ✓
- `delta_q_params()`: `delta_q_present f(1)=0` iff `base_q_idx>0` ✓ (§5.18.6.x)
- `deblocking_filter_params()`: `apply_deblocking_filter[0..1] f(1)=0 ×2` ✓ (§5.18.5)
- `read_tx_mode()`: `tx_mode_select f(1)=0` ✓
- `reduced_tx_set f(2)=0` ✓ (2 bits in AV2, not AV1's 1)
- `byte_alignment()` (zero padding) before tile data ✓
- Stream structure `SH CLK CLK …` matches the reference avmenc fixtures exactly (no TD/MFH — avmdec accepts; §7.3.7 strictly wants one TD per temporal unit, see open question below)

## Divergences (sequence header only)

All six are draft→final changes; encoder **and** decoder (`src/decode/sequence.rs`) share them.

| # | Where | Encoder (draft) | Spec v1.0.0 / avm HEAD |
|---|---|---|---|
| S1 | sequence.rs:20-23 vs §5.4.1 | `lcr f(3)`, `still f(1)` **before** `seq_level_idx` | `seq_level_idx f(5)` immediately after `single_picture_header_flag` |
| S2 | sequence.rs:5,25 | tier bit iff `level >= 8` | tier bit iff `seq_level_idx >= 4` (`SEQ_LEVEL_4_0=4`; `> 3 && !sph` in spec) |
| S3 | sequence.rs:64-78 `compute_level` | AV1-era indices: 3.0→4, 3.1→5, 4.0→8, 5.x→12 | Table A.7: 3.0→2, 3.1→3, 4.0→4, 4.1→5, 5.x→6…9 (pic-size bands in the table are already the v1.0.0 bands; only labels are wrong) |
| S4 | sequence.rs | `monotonic_output_order_flag` never written | required `f(1)` in the `!sph` branch, after `max_mlayer_id` block, before dimensions |
| S5 | sequence.rs:137 | `order_hint_bits_minus_1 f(3) = 5` | `order_hint_bits_minus_1 f(4)` (§5.4.6; avm HEAD `read_sequence_inter_group_tool_flags`) — every later inter-group bit shifts |
| S6 | sequence.rs:202 `write_filter_group` | 8 bits (6×0 + f(2)) | 9 bits in video mode: 7 flags + `df_par_bits_minus_2 f(2)` — `cdef_on_skip_txfm` is a **2-bit** pair (`always_on`, `disabled`) in `!sph` mode (§5.4.10; avm HEAD df.c:6210-6218) |
| — | sequence.rs:26-33 + decode/sequence.rs | chroma/bitdepth **after** crop; tlayer/mlayer **after** decoder-model flags | chroma/bitdepth right after tier; lcr/still/tlayer/mlayer/monotonic **before** dimensions (consequence of S1/S4) |
| — | obu.rs:16 | `Padding = 24` | `Padding = 25` (unused) |

Because of S1/S4/S5/S6 the bit positions of every field after bit ~10 of the
sequence header differ from v1.0.0. A v1.0.0-conformant decoder (current
avmdec) cannot parse our sequence header.

## Evidence

1. **Fixtures = draft format.** Hand-parsing all 9 `tests/fixtures/*.obu`
   sequence headers under the project order yields sane values
   (128x128/768x512/64x64, chroma=0, bitdepth_idc=1, sb 64/128); under the
   v1.0.0 order all parses produce garbage (e.g. height 62729, chroma 50).
2. **Current avm HEAD = v1.0.0 order.** `av2/decoder/obu.c`
   `read_sequence_header_obu`: profile → sph → level → tier(`>=SEQ_LEVEL_4_0`
   where `enums.h` defines `SEQ_LEVEL_4_0 = 4`) → chroma uvlc → bitdepth uvlc
   → lcr/still/tlayer/mlayer/monotonic → dims. `av2/decoder/decodeframe.c`:
   `order_hint_bits_minus_1 f(4)`; filter group reads the 2-bit
   cdef_on_skip_txfm pair.
3. **Frame header needs no change**: every field of `write_uncompressed_header`
   maps 1:1 onto §5.18.2/§5.18.5/§5.18.6/§5.18.7/§5.18.8 in order.

## Open questions

- **Temporal delimiters:** §7.3.7 lists one OBU_TEMPORAL_DELIMITER at the start
  of each temporal unit. Encoder and avmenc fixtures emit none; avmdec accepts.
  For multi-frame video, adding a TD before each frame (after the first) is the
  strictly-conformant shape — decide when migrating.
- **Which avmdec build is authoritative here?** The one referenced in AGENTS.md
  is not on this machine. If it predates the May 2026 reorder, current output
  decodes with it but not with avm HEAD; if it is at/after v1.0.0, current
  output is already broken against it. Fixtures prove only the old behavior.

## Recommended fix (S-items, encoder + decoder together)

1. Reorder sequence-header front matter to v1.0.0: level → tier(≥4) →
   chroma/bitdepth → lcr/still/tlayer/mlayer/monotonic → dims → crop →
   display/decoder-model flags → deps.
2. Fix tier gate to `seq_level_idx >= 4`, renumber `compute_level` to Table A.7
   (4.0 band → idx 4, etc.).
3. Write `monotonic_output_order_flag f(1)=0`.
4. `order_hint_bits_minus_1 f(4) = 5`; re-verify inter-group field order vs
   §5.4.6 (drl/explicit-num-refs/long_term_frame_id_bits positions).
5. Filter group: add the second `cdef_on_skip_txfm` bit (write 0,0) → 9 bits.
6. `ObuType::Padding = 25`.
7. Mirror 1–5 in `src/decode/sequence.rs`; regenerate `avmenc_*` fixtures with
   a v1.0.0-era avmenc (or retag existing ones as draft-format vectors);
   add regression test that round-trips headers and, if avmdec becomes
   available locally, an end-to-end decode test.
