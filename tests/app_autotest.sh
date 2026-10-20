#!/bin/bash
# Headless end-to-end run of the REAL app window (Linux, inside the sorapatch-build container):
# starts the app under Xvfb with the autotest hook, waits for the result, screenshots the window,
# and checks the built file byte-for-byte. Synthetic files only (run tests/run_synthetic.sh first).
#   tests/app_autotest.sh INPUT_FILE EXPECTED_FILE_OR_- SHOT.png [OUT_DIR]
set -euo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd)
APP=$HERE/app/src-tauri/target/release/sora-fan-patch
IN=$1; EXPECT=$2; SHOT=$3; OUT=${4:-$HERE/testdata/out-app}
rm -rf "$OUT"; mkdir -p "$OUT"
export WEBKIT_DISABLE_COMPOSITING_MODE=1 WEBKIT_DISABLE_DMABUF_RENDERER=1 LIBGL_ALWAYS_SOFTWARE=1 NO_AT_BRIDGE=1
export SORAPATCH_AUTOTEST=$IN SORAPATCH_AUTOTEST_OUT=$OUT
Xvfb :99 -screen 0 1000x820x24 >/dev/null 2>&1 &
XPID=$!
export DISPLAY=:99
sleep 1
dbus-run-session -- "$APP" >"$OUT.log" 2>&1 &
APID=$!
# wait for a finished file (success case) or a fixed time (problem cases)
for _ in $(seq 1 40); do
  sleep 0.5
  [ -n "$(find "$OUT" -maxdepth 1 -type f ! -name '.*' 2>/dev/null)" ] && break
done
sleep 2   # let the done/problem screen render
import -window root "$SHOT"
kill "$APID" 2>/dev/null || true
kill "$XPID" 2>/dev/null || true
wait 2>/dev/null || true
if [ "$EXPECT" = "-" ]; then
  [ -z "$(ls -A "$OUT")" ] && echo "PASS no file written (problem case), screenshot $SHOT" || { echo "FAIL unexpected output"; ls -la "$OUT"; exit 1; }
else
  f=$(find "$OUT" -maxdepth 1 -type f ! -name '.*' | head -1)
  [ -n "$f" ] && cmp -s "$f" "$EXPECT" && echo "PASS app built '$(basename "$f")' = expected bytes, screenshot $SHOT" \
    || { echo "FAIL app output"; cat "$OUT.log"; exit 1; }
fi
