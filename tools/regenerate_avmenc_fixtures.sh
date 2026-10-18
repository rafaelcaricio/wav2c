#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
AVMENC=${AVMENC:-${AVM_BUILD:-../avm_build}/avmenc}
WORK=${WORK:-$ROOT/target/fixture_inputs}
mkdir -p "$WORK" "$ROOT/tests/fixtures"

python3 - "$WORK" <<'PY'
import sys, os
out = sys.argv[1]
os.makedirs(out, exist_ok=True)

def write_y4m(path, w, h, fn):
    y = bytes(fn(x, y) for y in range(h) for x in range(w))
    cw, ch = w // 2, h // 2
    u = bytes([128]) * (cw * ch)
    v = bytes([128]) * (cw * ch)
    with open(path, "wb") as f:
        f.write(f"YUV4MPEG2 W{w} H{h} F30:1 Ip A1:1 C420jpeg\nFRAME\n".encode())
        f.write(y); f.write(u); f.write(v)

write_y4m(f"{out}/gradient_256x128.y4m", 256, 128,
          lambda x, y: ((x * 255) // 255 + (y * 255) // 127) // 2)
write_y4m(f"{out}/gradient_128x128.y4m", 128, 128,
          lambda x, y: ((x * 255) // 127 + (y * 255) // 127) // 2)
import random
rnd = random.Random(128 * 100 + 128)
write_y4m(f"{out}/noise_128x128.y4m", 128, 128, lambda x, y: rnd.randrange(256))
PY

COMMON="--codec=av2 --obu --i420 --limit=1 --cpu-used=8 -y --disable-warnings"
TOOLS_OFF="--enable-cdef=0 --enable-gdf=0 --enable-restoration=0 --enable-ccso=0 --enable-intrabc=0 --enable-palette=0 --enable-intra-dip=0 --enable-mrls=0 --enable-ibp=0 --enable-cfl-intra=0 --enable-paeth-intra=0 --enable-angle-delta=0 --enable-ist=0 --enable-flip-idtx=0 --enable-idtx-intra=0 --enable-cctx=0 --enable-tcq=0 --enable-qm=0 --enable-deblocking=0 --enable-cdef-on-skip-txfm=0 --enable-lf-sub-pu=0 --enable-intra-edge-filter=0 --enable-fsc=0 --enable-parity-hiding=0 --enable-avg-cdf=0 --enable-sdp=0 --enable-extended-sdp=0 --enable-rect-partitions=0 --enable-ext-partitions=0 --enable-uneven-4way-partitions=0 --enable-tx-partition=0 --max-pb-aspect-ratio=8 --max-partition-size=64 --min-partition-size=64 --sb-size=64"

avmenc() {
    "$AVMENC" $COMMON "$@" --disable-warnings
}

avmenc $TOOLS_OFF --end-usage=q --qp=100 -w 256 -h 128 \
    -o "$ROOT/tests/fixtures/avmenc_minimal.obu" "$WORK/gradient_256x128.y4m"
avmenc $TOOLS_OFF --end-usage=q --cq-level=30 -w 128 -h 128 \
    -o "$ROOT/tests/fixtures/avmenc_128x128_q30.obu" "$WORK/noise_128x128.y4m"
avmenc "${TOOLS_OFF/--enable-ibp=0/--enable-ibp=1}" --end-usage=q --cq-level=30 -w 128 -h 128 \
    -o "$ROOT/tests/fixtures/avmenc_128x128_q30_nofilter.obu" "$WORK/gradient_128x128.y4m"
avmenc $TOOLS_OFF --end-usage=q --cq-level=30 -w 128 -h 128 \
    -o "$ROOT/tests/fixtures/avmenc_128x128_q30_noibc.obu" "$WORK/gradient_128x128.y4m"

avmenc $TOOLS_OFF --end-usage=q --qp=0 -w 128 -h 128 \
    -o "$ROOT/tests/fixtures/avmenc_lossless_128x128.obu" "$WORK/gradient_128x128.y4m"
for tool in parity:parity-hiding qm:qm mrls:mrls; do
    name=${tool%%:*}
    flag=${tool##*:}
    avmenc $TOOLS_OFF --enable-$flag=1 --end-usage=q --cq-level=30 -w 128 -h 128 \
        -o "$ROOT/tests/fixtures/avmenc_${name}_128x128.obu" "$WORK/gradient_128x128.y4m"
done

avmenc $TOOLS_OFF --enable-lcr=1 --enable-atlas=1 --end-usage=q --cq-level=30 -w 128 -h 128 \
    -o "$ROOT/tests/fixtures/avmenc_lcr_atlas_128x128.obu" "$WORK/gradient_128x128.y4m"

rm -rf "$WORK"
