#!/bin/sh
# run-oracle.sh -- tier 3 (docs/PLAN.md section 4): boots unmodified
# guest oracle software (i2c.library's "bcu" driver, I2CScan,
# i2csensors.library, simplesensors, FannyCtl) against a real Copperline
# instance with manifest/cpldicy.toml fitted. No software written by
# this project touches the guest -- this is the "no special casing"
# validation docs/PLAN.md's Validation section calls for.
#
# Binaries come from nondistributable/ (git-ignored -- see that
# directory's own provenance notes / docs/board-facts.md §7): i2c.library
# v40's "bcu" driver + I2CScan from Aminet docs/hard/i2clib40.lha, and
# i2csensors.library/simplesensors/FannyCtl compiled binaries checked
# into Henryk Richter's gitlab.com/HenrykRichter/i2csensors repo.
#
# Output capture: these are ordinary AmigaDOS-linked binaries (not the
# freestanding RawPutChar-over-serial probes the tier-2 rig uses), so
# their Output() goes through dos.library, not Copperline's serial
# capture. Instead, a [[filesys]] host-directory mount (oracle-out/)
# gives the guest a live ORACLEOUT: volume; the Startup-Sequence
# redirects each tool's output there with plain ">" (no dependence on
# Shell's ">>" append support), and this script reads the files back
# afterward.
#
# Prereqs: same as run.sh (xdftool, a Copperline build), plus the
# nondistributable/ binaries -- run `make fetch-oracle` first if absent
# (see Makefile).
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
NONDIST="$ROOT/nondistributable"

COPPERLINE=${COPPERLINE:-copperline}
KICK=${KICK:-}
XDFTOOL=${XDFTOOL:-xdftool}
BENCH=${BENCH:-30}
ADF="$HERE/oracle-boot.adf"
OUTDIR="$HERE/oracle-out"
MACHINE_CONFIG="oracle-machine.toml"

command -v "$COPPERLINE" >/dev/null || { echo "FAIL: $COPPERLINE not found" >&2; exit 2; }
command -v "$XDFTOOL" >/dev/null || { echo "FAIL: $XDFTOOL not found (pip install amitools)" >&2; exit 2; }
[ -z "$KICK" ] || [ -e "$KICK" ] || { echo "FAIL: KICK set but missing: $KICK" >&2; exit 2; }
[ -e "$ROOT/target/wasm32-unknown-unknown/release/cpldicy_plugin.wasm" ] || {
    echo "FAIL: missing the release WASM plugin (run 'make' in the repo root)" >&2
    exit 2
}

I2C_BCU="$NONDIST/i2clib40/i2clib40/libs/i2c.library.bcu"
I2CSCAN="$NONDIST/i2clib40/i2clib40/bin/I2CScan"
I2CSENSORS_LIB="$NONDIST/i2csensors/i2csensors.library"
SIMPLESENSORS="$NONDIST/i2csensors/simplesensors"
DIAGNOSTICS="$NONDIST/i2csensors/diagnostics"
FANNYCTL="$NONDIST/i2csensors/FannyCtl"
LTC2990_CFG="$NONDIST/i2csensors/Sensors/LTC2990.cfg"
MAX31760_CFG="$NONDIST/i2csensors/Sensors/MAX31760_A0_Fanny.cfg"
for f in "$I2C_BCU" "$I2CSCAN" "$I2CSENSORS_LIB" "$SIMPLESENSORS" "$DIAGNOSTICS" "$FANNYCTL" "$LTC2990_CFG" "$MAX31760_CFG"; do
    [ -e "$f" ] || { echo "FAIL: missing oracle binary $f (run 'make fetch-oracle')" >&2; exit 2; }
done

[ -n "$KICK" ] && echo "ROM: $KICK" || echo "ROM: bundled AROS"

mkdir -p "$OUTDIR"
rm -f "$OUTDIR"/*.log

rm -f "$ADF"
"$XDFTOOL" "$ADF" format 'OracleBoot' ofs + \
    boot install boot1x + \
    makedir c + makedir libs + makedir devs + makedir devs/sensors + makedir s + \
    write "$I2CSCAN" c/i2cscan + \
    write "$SIMPLESENSORS" c/simplesensors + \
    write "$DIAGNOSTICS" c/diagnostics + \
    write "$FANNYCTL" c/fannyctl + \
    write "$I2C_BCU" libs/i2c.library + \
    write "$I2CSENSORS_LIB" libs/i2csensors.library + \
    write "$LTC2990_CFG" devs/sensors/ltc2990.cfg + \
    write "$MAX31760_CFG" devs/sensors/max31760.cfg + \
    write "$HERE/oracle-startup-sequence.txt" s/startup-sequence \
    > /dev/null

OUT=$(mktemp)
trap 'rm -f "$OUT"' EXIT INT TERM

set -- --config "$MACHINE_CONFIG" --noaudio --benchmark-until "$BENCH"
[ -n "$KICK" ] && set -- "$@" "$KICK"
( cd "$HERE" && "$COPPERLINE" "$@" ) >"$OUT" 2>/dev/null \
    || { echo "FAIL: $COPPERLINE exited non-zero" >&2; cat "$OUT" >&2; exit 3; }

echo "----- emulator log (tail) -----"
tail -20 "$OUT"
echo "--------------------------------"

[ -e "$OUTDIR/00-done.log" ] || {
    echo "FAIL: Startup-Sequence never completed (00-done.log missing) -- raise BENCH?" >&2
    exit 1
}

echo ""
echo "===== I2CScan ====="
cat "$OUTDIR/01-i2cscan.log" 2>/dev/null || echo "(missing)"
echo ""
echo "===== FannyCtl ====="
cat "$OUTDIR/02-fannyctl.log" 2>/dev/null || echo "(missing)"
echo ""
echo "===== simplesensors ====="
cat "$OUTDIR/03-simplesensors.log" 2>/dev/null || echo "(missing)"
echo ""
echo "===== diagnostics ====="
cat "$OUTDIR/04-diagnostics.log" 2>/dev/null || echo "(missing)"
echo ""

echo "Oracle pass ran to completion. Inspect the logs above (and $OUTDIR) for pass/fail judgement -- unlike run.sh, this script doesn't grep for PASS/FAIL markers, since it's driving unmodified third-party tools with their own native output formats, not this project's own probe."
