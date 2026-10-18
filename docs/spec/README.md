# AV2 Specification Reference

`AV2_Spec_v1.0.0.md` is a local Markdown conversion of the published AV2
Bitstream & Decoding Process Specification, version 1.0.0 (Final
Deliverable, 28 May 2026).

- Source: https://av2.aomedia.org/v1.0.0/index.html
- Upstream repository: https://github.com/AOMediaCodec/av2-spec
- Repository revision: `016ffa9cbb0d5538f92736100f82f78733581149`
- Source HTML SHA-256: `b267c96181f40799ec1c866ca8de3e936804617b71ac19437430d516aa73f1e3`
  (see `sha256sum` output below when regenerating)
- Published PDF: `v1.0.0/20260528_38f28e7_AV2_Spec_v1.0.0.pdf` in the
  upstream repository

The Markdown is generated, not normative. The published HTML/PDF is the
authoritative text. Formatting of complex tables and figures may differ.

## Regenerating

```sh
# Dependencies: Python 3 with beautifulsoup4 and lxml, plus a pandoc 3.x
# binary on PATH.
curl -sL https://av2.aomedia.org/v1.0.0/index.html -o /tmp/av2_spec.html
python3 tools/convert_av2_spec.py /tmp/av2_spec.html \
  docs/spec/AV2_Spec_v1.0.0.md --pandoc pandoc
```

The converter writes section headings, syntax listings, and tables as
Markdown, and strips navigation/UI markup. It is intended for offline
reference and code review, not redistribution.
