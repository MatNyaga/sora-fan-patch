#!/bin/bash
# Real-release GUI check of the Linux AppImage (P1.10d / RR-21), inside the sorapatch-build container (Xvfb):
#   A  a non-disc file          -> "This isn't a PSP game file", nothing written
#   B  the clean US ISO         -> "Done!", output file (caller checks its SHA-256 against release.json)
#   C  close the window at ~300 MB written -> app quits, no hidden .part left
# BWAIT=N: wait up to 2N s for B (default 240; a spinning-disk Docker store needs ~600).
# The clean ISO must be mounted READ-ONLY into the container. WORKDIR must be on a drive with ~4 GB free.
#   tests/app_release_gui.sh APPIMAGE CLEAN_ISO WORKDIR
# Host side (from the installer workspace root), e.g.:
#   docker run --rm --user $(id -u):$(id -g) -e HOME=$W -v /etc/passwd:/etc/passwd:ro -v /etc/group:/etc/group:ro \
#     -v $PWD:$PWD:ro -v $W:$W -v $CLEAN:$CLEAN:ro sorapatch-build $PWD/tests/app_release_gui.sh "$APPIMAGE" $CLEAN $W
set -u
APP=$1; CLEAN=$2; W=$3; HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$W"; gcc -O1 -o "$W/wmclose" "$HERE/wmclose.c" -lX11 || exit 3
export WEBKIT_DISABLE_COMPOSITING_MODE=1 WEBKIT_DISABLE_DMABUF_RENDERER=1 LIBGL_ALWAYS_SOFTWARE=1 NO_AT_BRIDGE=1
export APPIMAGE_EXTRACT_AND_RUN=1
Xvfb :99 -screen 0 1000x820x24 >/dev/null 2>&1 & XPID=$!; export DISPLAY=:99; sleep 1
start() { mkdir -p "$W/$1"; SORAPATCH_AUTOTEST=$2 SORAPATCH_AUTOTEST_OUT=$W/$1 dbus-run-session -- "$APP" >"$W/$1.log" 2>&1 & P=$!; }
# dbus-run-session does not pass a kill on to the app: stop the app itself, or its window lingers into the next test.
stop() { pkill -x sora-fan-patch 2>/dev/null; kill $P 2>/dev/null; wait $P 2>/dev/null
         for _ in $(seq 1 40); do pgrep -x sora-fan-patch >/dev/null || break; sleep 0.25; done; }

echo "== A non-disc file"; start A /etc/os-release; sleep 12; import -window root "$W/A.png"; stop
[ -z "$(ls -A "$W/A")" ] && echo "A PASS nothing written" || echo "A FAIL"

echo "== B clean ISO"; t0=$(date +%s); start B "$CLEAN"
for _ in $(seq 1 ${BWAIT:-240}); do sleep 2; kill -0 $P 2>/dev/null || break; [ -n "$(find "$W/B" -maxdepth 1 -type f ! -name '.*')" ] && break; done
sleep 3; import -window root "$W/B.png"; stop
echo "B output after $(( $(date +%s) - t0 )) s:"; ls -la "$W/B"

echo "== C close mid-build"; start C "$CLEAN"
for _ in $(seq 1 240); do sleep 1; kill -0 $P 2>/dev/null || break; [ -n "$(find "$W/C" -maxdepth 1 -name '.*.part' -size +300M)" ] && break; done
ls -la "$W/C"; "$W/wmclose" sora; t0=$(date +%s)
for _ in $(seq 1 120); do sleep 0.25; pgrep -x sora-fan-patch >/dev/null || break; done
alive=$(pgrep -x sora-fan-patch >/dev/null && echo yes || echo no); stop
echo "C app still running after close: $alive ($(( $(date +%s) - t0 )) s)"
[ "$alive" = no ] && [ -z "$(ls -A "$W/C")" ] && echo "C PASS quit + no partial file" || { echo "C FAIL"; ls -la "$W/C"; }
kill $XPID 2>/dev/null; wait 2>/dev/null
