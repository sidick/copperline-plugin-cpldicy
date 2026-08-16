#!/bin/sh
# run-thermal.sh -- tier 2 extension (docs/PLAN.md section 4, issue #8):
# the same closed thermal loop plugin/tests/flagship.rs's
# closed_thermal_loop_scripted_temperature_drives_fan_response test
# exercises host-side, but driven by a real m68k guest (thermal_probe.c)
# against a real Copperline instance with manifest/cpldicy.toml and a
# scripted thermal-scenario.txt fitted, instead of Rust code acting as
# its own guest. See thermal_probe.c's own header for what it does and
# why it polls instead of trying to guess a fixed delay.
#
# Prereqs: same as run.sh (m68k-amigaos-gcc or Docker
# ghcr.io/sidick/amiga-dev:1, xdftool, a Copperline build).
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)

COPPERLINE=${COPPERLINE:-copperline}
KICK=${KICK:-}
XDFTOOL=${XDFTOOL:-xdftool}
BENCH=${BENCH:-30}
DOCKER_IMAGE=${DOCKER_IMAGE:-ghcr.io/sidick/amiga-dev:1}
ADF="$HERE/thermal-boot.adf"
BIN="$HERE/thermal_probe"
MACHINE_CONFIG="thermal-machine.toml"

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

# Freestanding, same discipline as icy_probe.c: no crt, no libgcc.
# thermal_probe.c deliberately avoids float and even general 32-bit
# multiply/divide -- this toolchain's libgcc.a ships neither for the
# plain m68000 multilib (confirmed by hand), so temperature math here
# stays fixed-point with hand-rolled shift-and-add arithmetic instead.
if [ -n "$CC" ]; then
    echo "compiler: $CC"
    "$CC" -nostdlib -nostartfiles -O2 -Wall -Wextra -m68000 -msoft-float \
        -o "$BIN" "$HERE/thermal_probe.c"
else
    command -v docker >/dev/null || {
        echo "FAIL: no m68k-amigaos-gcc on PATH and no docker to fall back to" >&2
        exit 2
    }
    echo "compiler: docker $DOCKER_IMAGE m68k-amigaos-gcc"
    docker run --rm -v "$HERE:/work" -w /work "$DOCKER_IMAGE" \
        m68k-amigaos-gcc -nostdlib -nostartfiles -O2 -Wall -Wextra -m68000 -msoft-float \
        -o thermal_probe thermal_probe.c
fi

rm -f "$ADF"
"$XDFTOOL" "$ADF" format 'ThermalProbe' ofs + \
    boot install boot1x + \
    makedir c + makedir s + \
    write "$BIN" c/thermal_probe + \
    write "$HERE/thermal-startup-sequence.txt" s/startup-sequence \
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

SUBTESTS="find_board ltc2990_cool_baseline ltc2990_temperature_rises fan_duty_write_ack fan_spins_up"
fails=0
for s in $SUBTESTS; do
    grep -q "^SUB=${s}=PASS$" "$OUT" 2>/dev/null || {
        echo "FAIL: SUB=${s} did not report PASS" >&2
        fails=$((fails + 1))
    }
done
grep -q '^RESULT=PASS' "$OUT" 2>/dev/null || { echo "FAIL: RESULT=PASS not found" >&2; fails=$((fails + 1)); }

[ "$fails" -eq 0 ] || exit 1

echo "PASS: real m68k guest drove the closed thermal loop end-to-end (via serial)"
