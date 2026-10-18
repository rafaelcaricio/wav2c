# Multiview, layer configuration and atlas composition in AV2 v1.0.0

Reference: `docs/spec/AV2_Spec_v1.0.0.md` (Final Deliverable v1.0.0, 28 May
2026). Reference implementation: AVM HEAD `ce1ec108a82b`
(`github.com/AOMediaCodec/avm`).

## 1. Summary

AV2 has no dedicated multiview annex. Multiview is expressed entirely through
the normative high-level syntax that describes *layers* and *spatial layout*:

| Topic | Normative syntax | Normative semantics | Informative process |
|---|---|---|---|
| Streams and layers | §5.8 Layer config record OBU | §6.8 | Annex G |
| Spatial layout ("tiling") | §5.9 Atlas segment info OBU | §6.9 | Annex D, Annex G |
| Operating points | §5.10, §5.11 | §6.10 | — |
| Multi-stream operation | §5.6 MSDO OBU | §6.6 | Annex F.2.2 |
| Extended layer units | §7.3.2, §7.3.6, §7.6 | — | — |

The multi-view/tiled *picture* itself is never present in one coded frame. Each
view or subpicture is an independent extended layer (`obu_xlayer_id`) with its
own coded video sequence, and the Layer Configuration Record (LCR) plus Atlas
Segment Info OBUs say what each layer is and where it belongs. Composition of
decoded layers into a single output frame is described in Annex D and is
explicitly informative:

> "It is recommended that decoders support the process … However, this annex is
> marked as informative because supporting the composition process or
> implementing it according to the description is not mandatory for a conformant
> decoder." — Annex D.1

Annex G supplies worked examples: 360-degree viewport streaming over a 3×3
grid (G.2), a three-participant conferencing layout with resampling (G.3), and
region-of-interest scalability with encoder padding and normative cropping
(G.4).

Status: av2codec's encoder now emits multiview bitstreams (global LCR, per-layer
local LCRs, enhanced atlas, MSDO) that `avmdec` decodes per view bit-exactly;
§8.2 describes the layout and §9 lists the remaining decoder and encoder work.

## 2. Layer vocabulary

| Term | Syntax element / scope | Meaning |
|---|---|---|
| Temporal layer | `obu_tlayer_id` (2 bits) | Frame-rate scalability inside a layer |
| Embedded layer | `obu_mlayer_id` (3 bits), `lcr_mlayer_map` | An auxiliary or sub-stream inside an extended layer (e.g. alpha, depth) |
| Extended layer | `obu_xlayer_id` (5 bits), `lcr_xlayer_map` | One independent coded video sequence; one view, subpicture or spatial scalability level |
| Global layer context | `GLOBAL_XLAYER_ID` = 31 | Carries OBUs that describe the whole multistream (MSDO, global LCR, multistream atlas) |
| Atlas | `atlas_segment_id` (3 bits) per xlayer | Virtual canvas plus segment layout |
| Atlas region | `ats_region_info` | One cell of the atlas grid (geometry only) |
| Atlas segment | `ats_region_to_segment_mapping`, `ats_label_segment_info` | A rectangle of the canvas that one layer fills; may span several grid regions |
| Representation | `lcr_rep_info` | Decoded (pre-crop) size, format, cropping window |
| Operating point | OPS OBU | Selectable subset of layers/temporal layers |

OBU layer scoping follows §5.2.2: `obu_header_extension_flag` adds one header
byte carrying `obu_mlayer_id` (3 bits) and `obu_xlayer_id` (5 bits). OBU types
that cannot be layer-scoped (`OBU_TEMPORAL_DELIMITER`, `OBU_MSDO`) are always
in the global layer context. All other OBUs without the extension header are
inferred to belong to embedded layer 0 of extended layer 0.

Stream structure per §7.3.2 and §7.3.6: a *coded multistream video sequence*
is the set of coded extended layer units for the same time instant; a *coded
extended layer unit* is a temporal delimiter followed by the sequence header,
metadata, frame and tile group OBUs of one extended layer. A "multiview
bitstream" is therefore N such streams (each a normal AV2 bitstream) plus the
global-context description.

## 3. Layer Configuration Record (§5.8, §6.8)

Two record flavours are distinguished by the OBU's `obu_xlayer_id`.

### 3.1 Global LCR (`obu_xlayer_id == 31`), §5.8.1

```
lcr_global_info() {
    lcr_global_config_record_id      // 3 bits, 0 reserved
    lcr_xlayer_map                   // 31 bits: which extended layers exist
    lcr_aggregate_info_present_flag
    lcr_seq_profile_tier_level_info_present_flag
    lcr_global_payload_present_flag
    lcr_dependent_xlayers_flag
    lcr_global_atlas_id_present_flag
    lcr_global_purpose_id            // 7 bits, GLOBAL_PURPOSE_ID_TYPES
    lcr_doh_constraint_flag
    lcr_enforce_tile_alignment_flag
    lcr_global_atlas_id | lcr_global_reserved_zero_3bits
    lcr_global_reserved_zero_5bits
    ...
}
```

Key elements:

- `lcr_xlayer_map` enumerates the extended layers; `LcrXLayerID[]` is derived
  from the set bits, so layer IDs need not be contiguous.
- `lcr_aggregate_info_present_flag` carries one profile/level/tier/interop for
  the whole multistream (`lcr_aggregate_level_idx`, `lcr_max_tier_flag`,
  `lcr_max_interop`).
- `lcr_seq_profile_tier_level_info_present_flag` carries per-layer
  `lcr_seq_profile_idc`, `lcr_max_level_idx`, `lcr_tier_flag` and
  `lcr_max_mlayer_count`.
- `lcr_global_purpose_id` classifies the use case. `GLOBAL_PURPOSE_ID_TYPES`
  includes `UNSPECIFIED_MULTIVIEW`, `STEREOSCOPIC_VIEWPORTS`,
  `IMMERSIVE_MULTIPLE_VIEWPORTS` (+ alpha/depth variants), `MULTIVIEW_PLAYBACK`
  and `SUBREGION_PLAYBACK`.
- `lcr_dependent_xlayers_flag` + per-layer `lcr_num_dependent_xlayer_map`
  declare inter-layer dependencies (needed for scalability, not for
  independent views).
- `lcr_enforce_tile_alignment_flag` is the normative link between atlas layout
  and frame tiling:
  > "equal to 1 specifies that all extended layer sub-bitstreams associated
  > with this global LCR shall use the same tile structure … all extended
  > layers use the same values of TileCols, TileRows, and the same tile column
  > and row start positions." — §6.8
- The global payload is a per-layer byte string (`lcr_data_size` + payload)
  whose syntax depends on `lcr_dependent_xlayers_flag`.

### 3.2 Local LCR (any other `obu_xlayer_id`), §5.8.2

```
lcr_local_info(obu_xlayer_id) {
    lcr_global_id                    // 3 bits, may be 0 (none)
    lcr_local_id                     // 3 bits, 0 reserved
    lcr_profile_tier_level_info_present_flag
    lcr_local_atlas_id_present_flag
    lcr_local_atlas_id | lcr_reserved_zero_3bits
    lcr_reserved_zero_5bits
    lcr_xlayer_info(isGlobal = 0)
}
```

`lcr_xlayer_info` (§5.8.6) is the heart of the description:

```
lcr_xlayer_info() {
    lcr_rep_info_present_flag
    lcr_xlayer_purpose_present_flag
    lcr_xlayer_color_info_present_flag
    lcr_embedded_layer_info_present_flag
    if (lcr_rep_info_present_flag) {
        lcr_max_pic_width, lcr_max_pic_height          // uvlc
        lcr_format_info_present_flag
        lcr_crop_window_present_flag
        lcr_bit_depth_idc, lcr_chroma_format_idc       // if format info
        crop_win_left/right/top/bottom_offset          // if crop info
    }
    lcr_xlayer_purpose_id                              // 7 bits
    lcr_xlayer_color_info()                            // primaries, transfer, matrix, full range
    byte_alignment()
    lcr_embedded_layer_info()                          // per embedded layer
}
```

`lcr_embedded_layer_info` (§5.8.8) is emitted whenever the layer carries its
own embedded layers; otherwise a global LCR can attach a layer directly to an
atlas:

```
lcr_embedded_layer_info() {
    lcr_mlayer_map                                     // which embedded layers present
    for each embedded layer j:
        lcr_tlayer_map[j]
        lcr_layer_atlas_segment_id[j]                  // atlas segment this layer fills
        lcr_priority_order[j]                          // rendering order within the segment
        lcr_rendering_method[j]                        // e.g. overwrite / blend
        lcr_layer_type[j]                              // TEXTURE_LAYER, AUX_LAYER, STEREO_LAYER, DEPENDENT_LAYER
        if (AUX_LAYER) lcr_auxiliary_type[j]           // LCR_ALPHA_AUX, LCR_DEPTH_AUX, LCR_SEGMENTATION_AUX, LCR_GAIN_MAP_AUX
        lcr_view_type[j]                               // VIEW_UNSPECIFIED, VIEW_CENTER, VIEW_LEFT, VIEW_RIGHT, VIEW_EXPLICIT
        if (VIEW_EXPLICIT) lcr_view_id[j]
        if (j > 0) lcr_dependent_layer_map[j]
        lcr_same_sh_max_resolution_flag[j]
        if (!same) lcr_max_expected_width/height[j]
        byte_alignment()
}
```

This is the multiview signalling: `lcr_view_type`/`lcr_view_id` assign a view
to each layer, `lcr_layer_type` distinguishes texture from depth/alpha, and
`lcr_layer_atlas_segment_id` places the layer on the canvas. Nothing in the
coded frame data is view-specific — the same intra/inter tools code every
view.

## 4. Atlas segment info OBU (§5.9, §6.9)

`atlas_segment_id` (3 bits) plus `ats_atlas_segment_mode_idc` select one of
five layout models:

| Mode | Name | Layout description |
|---|---|---|
| 0 | `ENHANCED_ATLAS` | Region grid + region→segment mapping; no stream IDs, LCR maps layers to segments |
| 1 | `BASIC_ATLAS` | Explicit segment rectangles with optional `ats_input_stream_id` (5-bit xlayer id) |
| 2 | `SINGLE_ATLAS` | One segment, canvas size only |
| 3 | `MULTISTREAM_ATLAS` | Global-context atlas with `ats_msi_input_stream_id` per segment (composition in Annex D) |
| 4 | `MULTISTREAM_ALPHA_ATLAS` | As above plus per-segment alpha flag and background colour |

Enhanced atlas syntax (§5.9.2):

```
ats_region_info(xAId) {
    ats_num_region_columns_minus_1      // ≤ 63 (MAX_ATLAS_COLS)
    ats_num_region_rows_minus_1         // ≤ 63 (MAX_ATLAS_ROWS)
    ats_uniform_spacing_flag
    if (!uniform) { per-column widths; per-row heights }
    else          { ats_region_width_minus_1; ats_region_height_minus_1 }
}
ats_region_to_segment_mapping(xAId) {
    ats_single_region_per_atlas_segment_flag
    if (!one-to-one) {
        ats_num_atlas_segments_minus_1
        per segment: ats_top_left_region_column, ats_top_left_region_row,
                     ats_bottom_right_region_column_off, ats_bottom_right_region_row_off
    } else { numSegments = NumRegionsInAtlas }
}
ats_label_segment_info(xAId, numSegments) {
    ats_signaled_atlas_segment_ids_flag
    optional 8-bit ats_atlas_segment_id[] per segment
}
```

Consequences:

- The grid defines canvas geometry; a segment is the union of a rectangular
  block of grid cells, so segments can span rows (the "main speaker occupies
  the whole left column" case in G.3.2).
- Segments overlap freely; `lcr_priority_order` and `lcr_rendering_method`
  decide which layer wins, which is exactly how a high-resolution region-of
  interest overlays a lower-resolution background (G.4.2).
- Segment dimensions need not match the decoded layer size.
  `lcr_rep_info` gives the intended size and a normative cropping window;
  resampling is applied when the cropped size still differs, and the
  resampling filter is explicitly implementation-defined (G.3.3, G.4.3).
- Segment IDs are the join key: LCR says `lcr_layer_atlas_segment_id = 4`,
  the atlas says segment 4 covers row 1, column 1 — together they place the
  layer.

## 5. Multi-stream decoder operation and operating points

`multistream_decoder_operation_obu()` (§5.6) is signalled at
`GLOBAL_XLAYER_ID` and declares the multistream configuration:

```
num_streams_minus_2      // 1 .. 3 → 2 to 4 streams
multistream_profile_idc, multistream_level_idx, multistream_tier
multistream_even_allocation_flag
if (!even) multistream_large_picture_idc
for each stream i:
    sub_xlayer_id[i]
    sub_stream_max_profile[i], sub_stream_max_level[i], sub_stream_max_tier[i]
multistream_doh_constraint_flag
```

The 4-stream cap is why Annex G.2 notes that a 9-layer 360 layout "requires LCR
(not MSDO)". Operating point sets (§5.10/§5.11) then define selectable subsets
with their own aggregate PTL, decoder model, colour info and mlayer info, so a
client can decode, say, only the centre viewport layer.

## 6. Composition processes (informative)

Annex D.1–D.5 describes composition for `MULTISTREAM_ATLAS` /
`MULTISTREAM_ALPHA_ATLAS`:

1. Chroma format determination → `subX`, `subY`.
2. Array initialization: allocate the atlas-sized composite planes and fill
   them with the background colour (default 16,16,16 in BT.709 when absent).
3. For each segment: map the decoded frame associated with
   `ats_msi_input_stream_id[i]` into the rectangle
   (`ats_msi_segment_top_left_pos_x/y`, `width`, `height`).
4. Alpha segments are always followed by their paired texture segment
   (`ats_msi_alpha_segment_flag` is 0 for the last segment by conformance),
   and the pair is blended by the "spatial mapping with alpha" process.

Annex G covers the enhanced-atlas path, where no stream IDs exist and the
mapping is purely LCR-driven, plus the parts Annex D does not address:
resampling to segment size and normative cropping
(`lcr_max_pic_width/height` minus crop offsets) before placement.

## 7. What the AVM reference implementation actually does

Encoder (`avmenc`):

- `--enable-lcr=1` makes the encoder emit an `OBU_LAYER_CONFIGURATION_RECORD`
  before the first shown keyframe and set `seq_lcr_id = 1` in the sequence
  header. The record written by a stock single-layer encode is a *local* LCR
  for xlayer 0, not a global record.
- `--enable-atlas=1` (only effective together with `--enable-lcr=1`) emits an
  `OBU_ATLAS_SEGMENT` at xlayer 0 built from `cpi->atlas_list`, which is
  zero-initialised unless the application fills it, i.e. a degenerate enhanced
  atlas (1×1 grid, one segment).
- `--enable-ops=1` emits OPS OBUs.
- There is no multiview encoder: `avmenc` codes one extended layer. Spatial
  scalability with multiple *embedded* layers exists only in the
  `examples/scalable_encoder.c` example (via `AVME_SET_NUMBER_MLAYERS`), which
  still produces a single xlayer.
- `av2/encoder/bitstream_atlas.c` can serialise every atlas mode and
  `av2/encoder/bitstream_lcr.c` every LCR field, but nothing in the encoder
  populates a real multi-view layout.

Decoder (`avmdec`):

- Parses LCR (`av2/decoder/obu_lcr.c`), atlas (`av2/decoder/obu_atlas.c`) and
  MSDO (`av2/decoder/obu.c`), maintaining `lcr_list[xlayer_id][lcr_id]`,
  `atlas_list[xlayer_id][atlas_id]` and per-xlayer `num_streams`.
- Enforces conformance: `lcr_global_config_record_id == 0` and
  `lcr_local_id == 0` are invalid; a repeated record with the same identity
  must be bit-identical; `MULTISTREAM_ATLAS`/`MULTISTREAM_ALPHA_ATLAS` must be
  carried at `GLOBAL_XLAYER_ID`; `ats_num_region_*` and segment counts are
  range-checked; `lcr_auxiliary_type` reserved ranges are rejected.
- Accepts N input streams (`--num-streams=N`, one file per xlayer) and selects
  operating points per layer (`--select-local-ops=xlayer_id,ops_id,op_index`),
  writing one output per stream. It does **not** implement Annex D or G: no
  composition, no resampling, no atlas output frame.

Empirical check performed for this report: a stock `avmenc` encode of a 128×128
gradient with `--enable-lcr=1 --enable-atlas=1` yields the OBU sequence

```
TD(2) LCR(16) ATLAS(17) SEQ(1) CLK(4)
```

with `obu_xlayer_id = 0` and `obu_mlayer_id = 0` on every OBU. `avmdec`,
`dav2d` and av2codec all decode it to identical planes (FNV-1a of the frame
planes `0x908d9dfbe2e6bb67`); the fixture is checked in as
`tests/fixtures/avmenc_lcr_atlas_128x128.obu`. dav2d needs
`--demuxer section5` because its content probe expects the first OBU to be one
it recognises.

## 8. av2codec status

### 8.1 Decoder

Supported today:

- Layer-scoped OBU headers: `parse_obus` (`src/decode/obu.rs`) decodes
  `obu_header_extension_flag`, `obu_tlayer_id`, `obu_mlayer_id` and
  `obu_xlayer_id`, starts the payload after the extension byte, and applies the
  §5.2.2 inference (no extension header means extended layer 0, except for
  temporal delimiter and MSDO which are global).
- OBU types without a coded-picture role (`LCR` 16, `ATLAS` 17, `OPS` 18,
  `MSDO` 20, `QM` 22, metadata 8/9, padding 25, …) are skipped by the frame
  decoder, which looks only for the sequence header and CLK frame OBUs.
- A stream carrying LCR and atlas metadata decodes bit-exactly (fixture
  `avmenc_lcr_atlas_128x128.obu`, asserted in `src/decode/mod.rs` and
  `tests/dav2d_integration.rs`), so the decoder does not reject compliant
  metadata.

Not supported:

- No typed parsing of LCR/atlas/OPS/MSDO: their contents are never interpreted,
  so no layer catalogue, no view identification and no atlas geometry are
  exposed through the API.
- The decoder is single-layer aware. It decodes every CLK OBU of a multistream
  in bitstream order using the first sequence header, which is only correct when
  all layers share the same sequence parameters; it does not select a layer and
  does not accept N separate input files.
- No composition (Annex D), no cropping/resampling (Annex G.4), no operating
  point selection.

### 8.2 Encoder (implemented)

The encoder now produces real multiview bitstreams. `src/multiview.rs` exposes

```rust
pub fn encode_multiview(config: &MultiviewConfig) -> Result<Vec<u8>, EncodeError>;
pub fn extract_layer(bitstream: &[u8], xlayer_id: u8) -> Vec<u8>;
```

with `MultiviewConfig { base_qindex, purpose_id, layout, views }`,
`MultiviewView { width, height, view_type, view_id, layer_type,
auxiliary_type, frames }` and `AtlasLayout::{Row, Column}`. Two to four views
are supported (`num_streams_minus_2` allows at most four streams), each view may
have its own resolution, and all views must provide the same number of frames.

Output layout for every temporal unit (all views share one output time):

```
TD (global)                                  §7.3.7
MSDO (global)                                §5.6, required at IP0 when layers > 1
global LCR (obu_xlayer_id = 31)              §5.8.1, carries per-layer xlayer_info
atlas segment info (obu_xlayer_id = 31)      §5.9.2, enhanced atlas
for each view in ascending obu_xlayer_id:    §7.3.6, §7.3.1 requirement 6
    local LCR (obu_xlayer_id = view)         §5.8.2
    sequence header (seq_lcr_id = 1)         §5.4.1, activates the LCR
    CLK frame
```

Details that matter for conformance:

- **Profile 0 forces MSDO.** Table A.4 requires `OBU_MSDO` whenever a bitstream
  at interoperability point 0 has more than one extended layer, so the encoder
  always writes an MSDO with `num_streams_minus_2 + 2` equal to the view count,
  `multistream_profile_idc = 0`, one `sub_xlayer_id` per view and
  `multistream_doh_constraint_flag = 1` (the sequence header sets
  `monotonic_output_order_flag = 0`, which requires both DOH constraint flags to
  be 1).
- **Two LCR scopes, identical content.** The global LCR carries
  `lcr_global_payload_present_flag = 1` with one `lcr_global_payload()` per
  layer, and each layer also gets a local LCR whose `lcr_xlayer_info` bytes are
  exactly the global payload bytes. `avmdec` compares the two structures with
  `memcmp` when a local LCR names a parent global LCR that lists the same layer,
  so identical bytes are mandatory; sharing one `write_xlayer_info()` between
  both writers guarantees it.
- `lcr_max_pic_width/height` in the activated LCR must equal the sequence
  header's `max_frame_width/height`; both are the superblock-padded dimensions.
  `lcr_cropping_window_present_flag` matches `seq_cropping_window_present_flag`
  (0), and `lcr_mlayer_map = 1` / `lcr_tlayer_map = 1` match the sequence
  header's single embedded and temporal layer.
- **Views are declared, not implied.** Each local LCR sets `lcr_view_type`
  (`VIEW_LEFT`/`VIEW_RIGHT` for two views, `VIEW_EXPLICIT` plus `lcr_view_id`
  otherwise), `lcr_layer_type` (`TEXTURE_LAYER`, optionally `AUX_LAYER` plus
  `lcr_auxiliary_type`), `lcr_layer_atlas_segment_id = view index`,
  `lcr_priority_order = view index` and `lcr_rendering_method = 0`. The global
  LCR sets `lcr_global_purpose_id` to `STEREOSCOPIC_VIEWPORTS` or
  `MULTIVIEW_PLAYBACK`.
- **Atlas geometry.** `AtlasLayout::Row` emits one column per view, one row with
  the maximum view height; `Column` emits one row per view, one column with the
  maximum view width. `ats_uniform_spacing_flag = 0` whenever view sizes differ,
  `ats_single_region_per_atlas_segment_flag = 1` so each region is one segment,
  and segment ids are the view indices. `lcr_enforce_tile_alignment_flag` is set
  only when all views share one size.
- The spec's §5.2.2 `obu_extension_flag` (0) is written before the trailing bits
  of every extensible OBU (sequence header, LCR, atlas); `avmdec` reads it.

Validation, all with stock `avmdec`:

| Scenario | Result |
|---|---|
| 2-view stereoscopic, qindex 0, one frame | per-view output bit-exact against the source planes (`--num-streams=2`) |
| 3-view `MULTIVIEW_PLAYBACK`, column layout, qindex 0 | per-view output bit-exact |
| 2 views, 3 frames, qindex 0 | per-view multi-frame output bit-exact |
| 64x64 + 128x64 views | non-uniform atlas grid, both views bit-exact |
| texture + alpha auxiliary layer | both layers bit-exact |
| extracted singlestream per layer (`extract_layer`) | decodes bit-exactly with both `avmdec` and av2codec's decoder, confirming §7.3.1 requirement 7 |

`dav2d` currently decodes a multistream as a sequence of frames (it ignores
LCR/atlas and reports "Decoded 2/1 frames"), so it cannot be used to validate
per-view output; it still decodes the extracted singlestreams.

## 9. Remaining work

Decoder side (in priority order):

P1 — metadata parsing: `src/decode/lcr.rs`, `src/decode/atlas.rs`,
`src/decode/ops.rs` with plain structs mirroring §5.8–5.11, range checks from
§6.8/§6.9, and a `LayerContext` holding `lcr_list[xlayer][id]`,
`atlas_list[xlayer][id]`, `ops_list[xlayer][id]` plus MSDO state. Unit tests:
golden bit sequences, the LCR/atlas fixture above, and negative cases (reserved
IDs, non-identical repeated records, multistream atlas outside the global
context). The writers in `src/bitstream/{lcr,atlas,msdo}.rs` can be mirrored for
the parser tests.

P2 — public layer catalogue: `Decoder::layers() -> &[LayerInfo]` exposing
xlayer id, view type/id, layer type/auxiliary type, atlas segment id, priority,
rendering method, max resolution and cropping window; `Decoder::atlas()`
exposing canvas size and segment rectangles. No decode path changes.

P3 — layer selection and multistream input: decode one extended layer at a time
(either extracted from one bitstream or supplied as N files), tag each
`DecodedFrame` with `mlayer_id`/`xlayer_id`, and verify per-layer conformance
(sequence header agreement, `lcr_enforce_tile_alignment_flag`).

P4 — optional composition: a separately gated module implementing Annex D
(background init, rectangular spatial mapping, alpha pairing) and the Annex G.4
crop-then-place rule, with a pluggable resampler. Kept opt-in because the spec
does not require it and the resampling policy is implementation-defined.

P5 — operating point selection: honour OPS to skip layers/temporal layers the
caller did not select, matching `avmdec --select-local-ops` behaviour closely
enough for cross-checks.

Encoder side:

- Pair auxiliary layers with their texture layer in the same atlas segment
  (`lcr_priority_order` / `lcr_rendering_method`) instead of giving every view
  its own segment.
- Emit OPS OBUs for layer/temporal-layer selection.
- Support multiple embedded layers per extended layer (alpha and depth in one
  layer), which also exercises `lcr_mlayer_map` beyond the single bit used now.
- Optional encoder-side padding and cropping windows (`lcr_rep_info`) for
  hardware-aligned coding of non-multiple-of-64 views.

Test vector gap: because `avmenc` cannot produce genuine multiview content, all
multiview tests use av2codec's own encoder output; per-view results are then
checked against `avmdec --num-streams`, which is the only available independent
compositor-free reference.

## 10. References


- AV2 spec §5.2.2 (OBU header), §5.6 (MSDO), §5.8 (LCR), §5.9 (atlas),
  §5.10–5.11 (OPS), §6.6, §6.8, §6.9, §6.10, §7.3.2, §7.3.6, §7.6.
- AV2 spec Annex D (multistream composition), Annex F.2.2 (multistream
  extraction), Annex G (layer/atlas usage examples, including the 3×3
  360-degree grid, conferencing layout and ROI-with-padding examples).
- AVM: `av2/decoder/obu_lcr.c`, `av2/decoder/obu_atlas.c`,
  `av2/decoder/annexF.c`, `av2/encoder/bitstream_lcr.c`,
  `av2/encoder/bitstream_atlas.c`, `av2/common/enums.h`
  (`LAYER_TYPES`, `VIEW_TYPES`, `ATLAS_TYPES`, `GLOBAL_PURPOSE_ID_TYPES`),
  `avm/avm_codec.h` (OBU type values), `apps/avmdec.c` (`--num-streams`,
  `--select-local-ops`).
