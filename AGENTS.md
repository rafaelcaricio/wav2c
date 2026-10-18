# av2codec — AV2 Codec Development Protocol

## Core Principle: AV2 Specification Compliance

av2codec MUST BE SPECIFICATION COMPLIANT. Both the encoder and decoder must produce and consume valid AV2 bitstreams. The decoder must accept ANY AV2-compliant bitstream — it may skip or ignore unsupported codec tools, but the tools that ARE implemented must work correctly. av2codec will gradually evolve to implement more AV2 tools, but what exists must be correct.

`docs/spec/AV2_Spec_v1.0.0.md` is the normative reference and outranks the reference implementation. Where the AVM source and the spec disagree, the spec wins and the divergence is written up in `docs/reports/`.

## Goal

Encode AV2-conformant intra-only bitstreams (profile 0, 8-bit 4:2:0) that stock avmdec decodes — including lossless, quantization matrices, parity hiding, MRLS, and 2–4 layer multiview carrying a layer configuration record, enhanced atlas, and MSDO. Decode any AV2-compliant intra-only bitstream, or fail closed with `UnsupportedFeature` instead of emitting wrong pixels.

## Autonomous Closed-Loop Protocol

Execute phases sequentially. Each phase follows this cycle:

1. **Plan** — Define scope, identify what changes in headers/encoding/container
2. **Research** — Use a subagent to study the spec and avmdec decoder expectations
3. **Document** — Use a subagent to write implementation plan to `docs/plans/`
4. **Implement** — Subagent-driven development with code review per task
5. **Validate** — Generate test artifacts with ffmpeg, decode with avmdec and dav2d, write regression tests
6. **Loop** — Commit, update roadmap, update documentation, proceed to next phase

## Development Rules

- Never stop to ask questions — proceed with best approach
- No code comments — self-documenting code through clear naming
- Name things by their domain meaning and current role in the codec, never by timeline or migration framing; avoid names like `legacy`, `new`, `old`, or similar labels inside the codebase
- Tasks only done when code compiles with all features working
- Use subagent development for parallel work
- All tests must pass before committing: `cargo test --workspace` and `cargo clippy --workspace --tests`. The workspace includes `demos/live-camera` and `demos/wasm-multiview` while `default-members` is the codec alone, so a bare `cargo test` silently skips the demo crates
- Unimplemented codec tools fail closed: return `DecodeError::UnsupportedFeature` rather than decoding something the crate cannot represent
- Every phase adds automated regression tests that protect previous work
- avmdec at `../avm_build/avmdec` is the authoritative decoder
- dav2d at `../dav2d` is the secondary cross-check; it ignores layer configuration records, atlas segments, and MSDO, so it cannot compose multiview content
- Can add debug logging to avmdec for investigation, but encoder must work with stock avmdec
- No code changes that affect the AV2 decoding process are allowed in avmdec
- Must execute an end-to-end transcoding with av2codec and decode with reference avmdec; When necessary create test frames with FFmpeg
- Committed artifacts (WASM module, slides PDF, QR images, sample OBUs) are regenerated with their script in the same commit as the change they reflect

## Test Strategy

- Unit tests for each module (bitwriter, obu, entropy, cdf, tile, frame, sequence)
- Integration tests that encode → decode with avmdec → verify pixel values
- Cross-decoder parity tests assert av2codec's own decoder matches avmdec bit-for-bit
- Fail-closed tests assert unsupported tools are rejected, not guessed
- Multiview tests decode every layer with `avmdec --num-streams=N`
- Generate reference inputs with ffmpeg (solid colors, gradients, real images)
- Every new phase must not break tests from previous phases

## Layout

| Path | Contents |
|---|---|
| `src/` | Codec library and `av2codec` CLI |
| `demos/` | Multiview demos, kept out of the codec API; see `demos/README.md` |
| `docs/spec/` | Normative AV2 v1.0.0 specification |
| `docs/plans/` | Per-phase designs and implementation plans |
| `docs/reports/` | Investigations, audits, and conformance findings |

## Phase Roadmap

Capability status lives in the matrix at the top of `README.md`; designs live in `docs/plans/`.

- Phase 0: Infrastructure [COMPLETE]
- Phase 1: Multi-frame all-intra encoding with DC coefficients [COMPLETE]
- Phase 2: AC coefficient support [COMPLETE]
- Phase 3: CDF adaptation engine [COMPLETE]
- Phase 4: Recursive partition tree [COMPLETE]
- Phase 5: Multi-size transforms, 4x4 through 64x64 [COMPLETE]
- Phase 6: All-intra prediction modes [COMPLETE]

## Key Technical Reference

- AV2 specification: `docs/spec/AV2_Spec_v1.0.0.md`
- AVM reference source: `../avm_ref/` (encoder and decoder reference implementation)
- avmdec binary: `../avm_build/avmdec`
- avmdec usage: `../avm_build/avmdec -o /tmp/output.y4m <input.obu>`
- avmdec raw output: `../avm_build/avmdec --rawvideo -o /tmp/output.raw <input.obu>`
- avmdec multistream: `../avm_build/avmdec --num-streams=N -o /tmp/output.y4m <input.obu>`
- AV2 bitstream format: OBU-based (Open Bitstream Units)
- Video mode: `single_picture_header_flag = 0`, CLK OBUs with order_hint
- Profile 0: 8-bit 4:2:0
- Entropy coding: CDF-based arithmetic range coder (Daala-derived)
- CDF format: cumulative distribution functions with adaptation counter
- Sequence header flags are emitted by `src/bitstream/sequence.rs`; the filter and inter tool groups are switched off
- Intra modes: 13 base modes (DC, V, H, D45-D203, Smooth×3, Paeth)
- Transforms: DCT + ADST (integer arithmetic, no floating point in codec path)
- Extended layers: OBU header carries `obu_xlayer_id` (5 bits) with `GLOBAL_XLAYER_ID = 31`; multiview layers are independent, and composition is informative per Annex D and G
