#!/bin/sh
# run.sh -- headless Copperline conformance test for the CPLDIcy board
# window contract (docs/board-facts.md, docs/PLAN.md section 4 tier 2),
# against a real Copperline instance with manifest/cpldicy.toml fitted.
#
# Builds a minimal bootable ADF (xdftool) with just C:icy_probe and a
# one-line Startup-Sequence, boots it, and asserts the SUB=<name>=PASS
# lines the probe emits over serial (Copperline forwards serial to its
# stdout via `--serial stdout`). Modeled on
# copperline-bridgeboard-plugin's tests/copperline/run.sh -- see
# icy_probe.c's own header comment for what this probe does and does NOT
# cover (no i2c.library yet -- that's tier 3, docs/PLAN.md section 4).
#
# Prereqs:
#   - m68k-amigaos-gcc on PATH (bebbo amiga-gcc), or Docker with
#     ghcr.io/sidick/amiga-dev:1 (used automatically as a fallback --
#     override with DOCKER_IMAGE=), or CC= pointing at another cross-gcc
#   - xdftool on PATH (amitools: `pip install amitools`) or XDFTOOL=path
#   - A Copperline build on PATH, or COPPERLINE=/path/to/copperline
#   - A real Kickstart ROM: KICK=path/to/kickstart-1.3.rom (or unset to
#     use Copperline's bundled AROS)
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)

COPPERLINE=${COPPERLINE:-copperline}
KICK=${KICK:-}
XDFTOOL=${XDFTOOL:-xdftool}
BENCH=${BENCH:-15}
DOCKER_IMAGE=${DOCKER_IMAGE:-ghcr.io/sidick/amiga-dev:1}
ADF="$HERE/minimal-boot.adf"
BIN="$HERE/icy_probe"
MACHINE_CONFIG="machine.toml"

command -v "$COPPERLINE" >/dev/null || { echo "FAIL: $COPPERLINE not found" >&2; exit 2; }
command -v "$XDFTOOL" >/dev/null || { echo "FAIL: $XDFTOOL not found (pip install amitools)" >&2; exit 2; }
[ -z "$KICK" ] || [ -e "$KICK" ] || { echo "FAIL: KICK set but missing: $KICK" >&2; exit 2; }
[ -e "$ROOT/manifest/cpldicy.toml" ] || { echo "FAIL: missing $ROOT/manifest/cpldicy.toml" >&2; exit 2; }
[ -e "$ROOT/target/wasm32-unknown-unknown/release/cpldicy_plugin.wasm" ] || {
    echo "FAIL: missing the release WASM plugin (run 'make' in the repo root)" >&2
    exit 2
}
[ -n "$KICK" ] && echo "ROM: $KICK" || echo "ROM: bundled AROS"

CC=${CC:-}
if [ -z "$CC" ] && command -v m68k-amigaos-gcc >/dev/null 2>&1; then
    CC=m68k-amigaos-gcc
fi

# Freestanding, same discipline as the bridgeboard/hostsocket probes: no
# crt, no libgcc.
if [ -n "$CC" ]; then
    echo "compiler: $CC"
    "$CC" -nostdlib -nostartfiles -O2 -Wall -Wextra -m68000 -msoft-float \
        -o "$BIN" "$HERE/icy_probe.c"
else
    command -v docker >/dev/null || {
        echo "FAIL: no m68k-amigaos-gcc on PATH and no docker to fall back to" >&2
        exit 2
    }
    echo "compiler: docker $DOCKER_IMAGE m68k-amigaos-gcc"
    docker run --rm -v "$HERE:/work" -w /work "$DOCKER_IMAGE" \
        m68k-amigaos-gcc -nostdlib -nostartfiles -O2 -Wall -Wextra -m68000 -msoft-float \
        -o icy_probe icy_probe.c
fi

rm -f "$ADF"
"$XDFTOOL" "$ADF" format 'ICYProbe' ofs + \
    boot install boot1x + \
    makedir c + makedir s + \
    write "$BIN" c/icy_probe + \
    write "$HERE/startup-sequence.txt" s/startup-sequence \
    > /dev/null

OUT=$(mktemp)
trap 'rm -f "$OUT"' EXIT INT TERM

set -- --config "$MACHINE_CONFIG" --noaudio --serial stdout --benchmark-until "$BENCH"
[ -n "$KICK" ] && set -- "$@" "$KICK"
( cd "$HERE" && "$COPPERLINE" "$@" ) >"$OUT" 2>/dev/null \
    || { echo "FAIL: $COPPERLINE exited non-zero" >&2; cat "$OUT" >&2; exit 3; }

tr -d '\r' <"$OUT" >"$OUT.n" && mv "$OUT.n" "$OUT"
echo "----- serial capture -----"; cat "$OUT"; echo "--------------------------"
grep -q '^END' "$OUT" 2>/dev/null || { echo "FAIL: no END marker (board never autoconfigured? raise BENCH?)" >&2; exit 1; }

SUBTESTS="find_board reset_defaults register_roundtrip master_tx_ack master_rx_roundtrip"
fails=0
for s in $SUBTESTS; do
    grep -q "^SUB=${s}=PASS$" "$OUT" 2>/dev/null || {
        echo "FAIL: SUB=${s} did not report PASS" >&2
        fails=$((fails + 1))
    }
done
grep -q '^RESULT=PASS' "$OUT" 2>/dev/null || { echo "FAIL: RESULT=PASS not found" >&2; fails=$((fails + 1)); }

[ "$fails" -eq 0 ] || exit 1

echo "PASS: CPLDIcy board window-contract conformance probe (via serial)"
