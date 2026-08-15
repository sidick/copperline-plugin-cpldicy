#!/bin/sh
# fetch-oracle.sh -- pulls the guest-oracle binaries tests/copperline/
# run-oracle.sh needs into nondistributable/ (git-ignored, never
# committed -- see docs/board-facts.md §7 and PLAN.md's licence-hygiene
# note: nothing from these third-party redistributions is vendored into
# this repo's own source, only downloaded to a local, ignored directory
# for running the emulator against).
#
# Sources:
#   - i2c.library v40 (Wilhelm Noeker/Brian Ipsen), Aminet docs/hard,
#     postcard-ware for non-profit use.
#   - i2csensors.library/simplesensors/diagnostics/FannyCtl (Henryk
#     Richter, GPL-2.0), gitlab.com/HenrykRichter/i2csensors.
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
NONDIST="$ROOT/nondistributable"
mkdir -p "$NONDIST"

if [ ! -e "$NONDIST/i2clib40/i2clib40/libs/i2c.library.bcu" ]; then
    echo "Fetching i2clib40.lha..."
    curl -sL -o "$NONDIST/i2clib40.lha" "http://aminet.net/docs/hard/i2clib40.lha"
    mkdir -p "$NONDIST/i2clib40"
    ( cd "$NONDIST/i2clib40" && lha xw=. "$NONDIST/i2clib40.lha" >/dev/null )
fi

I2CS="$NONDIST/i2csensors"
mkdir -p "$I2CS/Sensors"
BASE="https://gitlab.com/HenrykRichter/i2csensors/-/raw/master"
[ -e "$I2CS/FannyCtl" ] || curl -sL -o "$I2CS/FannyCtl" "$BASE/Fanny/c/FannyCtl"
[ -e "$I2CS/simplesensors" ] || curl -sL -o "$I2CS/simplesensors" "$BASE/sensors/c/simplesensors"
[ -e "$I2CS/diagnostics" ] || curl -sL -o "$I2CS/diagnostics" "$BASE/sensors/c/diagnostics"
[ -e "$I2CS/i2csensors.library" ] || curl -sL -o "$I2CS/i2csensors.library" "$BASE/sensors/libs/i2csensors.library"
[ -e "$I2CS/Sensors/LTC2990.cfg" ] || curl -sL -o "$I2CS/Sensors/LTC2990.cfg" "$BASE/sensors/devs/Sensors/LTC2990.cfg"
[ -e "$I2CS/Sensors/MAX31760_A0_Fanny.cfg" ] || curl -sL -o "$I2CS/Sensors/MAX31760_A0_Fanny.cfg" "$BASE/sensors/devs/Sensors/MAX31760_A0_Fanny.cfg"

echo "Oracle binaries present in $NONDIST"
