# Multiview encoder

## Scope

Produce conformant AV2 multistream bitstreams from N independent views, each
view coded as its own extended layer with the existing intra encoder, and make
the result verifiable against the reference decoder.

## Design

- One bitstream file. Per temporal unit: temporal delimiter, MSDO, global LCR
  (extended layer 31), enhanced atlas (extended layer 31), then one coded
  extended layer unit per view in ascending `obu_xlayer_id` order (local LCR,
  sequence header, CLK). This follows §7.3.6 and §7.3.7 ordering.
- Profile 0 is interoperability point 0, so Table A.4 requires the MSDO OBU as
  soon as there is more than one extended layer.
- The global LCR carries one `lcr_global_payload()` per layer; the local LCR of
  the same layer repeats the identical `lcr_xlayer_info` bytes. `avmdec`
  compares the parsed structures with `memcmp`, so the two writers share
  `write_xlayer_info()`.
- `seq_lcr_id` is set to the local LCR id so each layer activates the record
  that describes it.
- Views are described with `lcr_view_type` (`VIEW_LEFT`/`VIEW_RIGHT` for two
  views, `VIEW_EXPLICIT` plus `lcr_view_id` for more), `lcr_layer_type`,
  optional `lcr_auxiliary_type`, an atlas segment id equal to the view index,
  `lcr_priority_order`, and a global purpose id.
- Atlas layout is derived from the views: one region per view, one column per
  view for `Row` and one row per view for `Column`; non-uniform spacing when
  resolutions differ; one region per segment, so segment ids are view indices.
- `lcr_enforce_tile_alignment_flag` is set only when all views share one size,
  because the intra encoder uses a single tile per frame.
- Frames are produced by `encode_intra_frame_payload`, the same intra path used
  by the single-layer encoder, so per-view output matches a standalone encode.

## Validation

`tests/multiview.rs` encodes stereoscopic, three-view, multi-frame,
mixed-resolution and auxiliary-layer streams and checks that
`avmdec --num-streams=N` returns the source planes bit-exactly (qindex 0). It
also extracts each layer with `extract_layer` and checks that the singlestream
decodes bit-exactly with both `avmdec` and av2codec's decoder, which confirms
the §7.3.1 requirement that each extracted stream is a valid bitstream.

`dav2d` is not usable for these checks: it ignores the layer metadata and emits
all layers as consecutive frames.

## Follow-ups

Decoder-side LCR/atlas parsing, layer selection, optional Annex D composition
and operating point selection; encoder-side atlas-segment sharing for alpha
layers, OPS emission and multiple embedded layers per extended layer.
