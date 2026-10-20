#!/bin/bash
# End-to-end test of sorapatch-core on SMALL SYNTHETIC files only (no game data, no .iso names).
#   tests/run_synthetic.sh [WORKDIR]     (default: testdata/ in the installer workspace)
# Makes a fake 3 MiB "disc" with a ULUS-10566 header, a "modded" copy (changed bytes + appended
# tail, like a mode-B disc), encodes with the real xdelta3 under several settings, applies with
# our pure-Rust decoder, and checks every plain-word problem case. Needs: cargo, xdelta3, python3.
set -euo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd)
W=${1:-$HERE/testdata}
mkdir -p "$W"
W=$(cd "$W" && pwd)
case "$W" in */dumps*) echo "refusing to work under dumps/"; exit 2;; esac
cd "$HERE"
nice cargo build --release -q -p sorapatch-cli
CLI=${CARGO_TARGET_DIR:-$HERE/target}/release/sorapatch-cli
"$CLI" --check-strings

python3 -I - "$W" <<'PY'
import os, random, sys, zlib
w = sys.argv[1]
rnd = random.Random(12)
def disc(disc_id, size):
    img = bytearray(rnd.getrandbits(8) for _ in range(size))
    img[16*2048:20*2048] = bytes(4*2048)
    pvd = 16*2048
    img[pvd] = 1; img[pvd+1:pvd+6] = b"CD001"
    r = pvd + 156
    img[r] = 34; img[r+2:r+6] = (18).to_bytes(4, "little"); img[r+10:r+14] = (2048).to_bytes(4, "little")
    d = 18*2048
    for i, n in ((0, 0), (34, 1)):
        img[d+i] = 34; img[d+i+32] = 1; img[d+i+33] = n
    name = b"UMD_DATA.BIN;1"; e = d + 68
    img[e] = 33 + len(name) + 1; img[e+2:e+6] = (19).to_bytes(4, "little")
    img[e+10:e+14] = (32).to_bytes(4, "little"); img[e+32] = len(name); img[e+33:e+33+len(name)] = name
    s = f"{disc_id}|0123456789ABCDEF|0001|G".encode()
    img[19*2048:19*2048+len(s)] = s
    return img
clean = disc("ULUS-10566", 3 << 20)
mod = bytearray(clean)
for off in (300_000, 1_000_000, 2_500_000):          # changed regions ("EBOOT words", replaced files)
    mod[off:off+5000] = bytes(rnd.getrandbits(8) for _ in range(5000))
mod[2_000_000:2_000_000] = b"INSERTED" * 4096        # shifted data
tail = bytes(rnd.getrandbits(8) for _ in range(400_000)) + b"\0" * 300_000
mod += tail                                           # appended tail (mode-B style), incl. a zero run
older = bytearray(mod); older[-1] ^= 0xFF
open(f"{w}/synthetic-clean.img", "wb").write(clean)
open(f"{w}/synthetic-mod.img", "wb").write(mod)
open(f"{w}/synthetic-older.img", "wb").write(older)
open(f"{w}/synthetic-eu.img", "wb").write(disc("ULES-01505", 3 << 20))
open(f"{w}/synthetic-othergame.img", "wb").write(disc("ULUS-10391", 3 << 20))
open(f"{w}/synthetic-truncated.img", "wb").write(clean[:1 << 20])
words = [bytes(rnd.choice(b"abcdefghijklmnopqrstuvwxyz") for _ in range(rnd.randint(2, 9))) for _ in range(300)]
text = b" ".join(rnd.choice(words) for _ in range(60_000))   # compressible, so djw really engages
open(f"{w}/synthetic-mod-text.img", "wb").write(bytes(clean) + text)
changed = bytearray(clean); changed[-10] ^= 1
open(f"{w}/synthetic-changed.img", "wb").write(changed)
open(f"{w}/synthetic-packed.zip", "wb").write(b"PK\x03\x04" + bytes(100))
open(f"{w}/synthetic-compressed.cso", "wb").write(b"CISO" + bytes(100))
open(f"{w}/synthetic-empty.img", "wb").write(b"")
PY

sha() { sha256sum "$1" | cut -d' ' -f1; }
size() { stat -c %s "$1"; }
mkrel() { # mkrel DIR XDELTA_FLAGS...   (MOD=file overrides the target)
  local d=$1; shift
  local mod=${MOD:-$W/synthetic-mod.img}
  mkdir -p "$d"
  xdelta3 -e -f "$@" -s "$W/synthetic-clean.img" "$mod" "$d/patch.vcdiff"
  cat > "$d/release.json" <<EOF
{
  "format": 1,
  "title": "Synthetic test release",
  "version": "0.0-test",
  "input":  { "disc_id": "ULUS-10566", "size": $(size "$W/synthetic-clean.img"), "sha256": "$(sha "$W/synthetic-clean.img")" },
  "output": { "file_name": "Synthetic Test Output.img", "size": $(size "$mod"), "sha256": "$(sha "$mod")" },
  "older_outputs": [ { "version": "0.0-older", "sha256": "$(sha "$W/synthetic-older.img")" } ],
  "other_regions": { "ULES-01505": "European", "ULJM-05814": "Japanese" }
}
EOF
}

PASS=0; FAIL=0
CLEAN_SHA=$(sha "$W/synthetic-clean.img")
ok()  { echo "PASS $*"; PASS=$((PASS+1)); }
bad() { echo "FAIL $*"; FAIL=$((FAIL+1)); }

# 1) round trips under several encoder settings (small windows force many windows/segments)
for v in "default:-9 -S none" "fast:-1 -S none" "smallwin:-9 -S none -W 16384 -B 524288" "level0:-0 -S none"; do
  name=${v%%:*}; flags=${v#*:}
  R=$W/rel-$name; O=$W/out-$name
  rm -rf "$O"; mkdir -p "$O"
  # shellcheck disable=SC2086
  mkrel "$R" $flags
  if "$CLI" "$R" "$W/synthetic-clean.img" --out-dir "$O" > "$O.log" && cmp -s "$O/Synthetic Test Output.img" "$W/synthetic-mod.img"; then
    ok "roundtrip $name ($(size "$R/patch.vcdiff") B patch, $(grep -c '^progress' "$O.log") progress lines)"
  else bad "roundtrip $name"; cat "$O.log"; fi
  [ -z "$(find "$O" -name '.*part')" ] || bad "temp file left in $O"
done
R=$W/rel-default; O=$W/out-default

# 2) never overwrite: second run gets " (2)"
"$CLI" "$R" "$W/synthetic-clean.img" --out-dir "$O" > /dev/null && [ -f "$O/Synthetic Test Output (2).img" ] \
  && ok "second run writes '(2)' and keeps the first" || bad "no-overwrite"
[ "$(sha "$W/synthetic-clean.img")" = "$CLEAN_SHA" ] && ok "input file unchanged after 5 builds" || bad "input changed"

# 3) problem cases -> expected code, no output file
expect() { # expect CODE FILE [extra args]
  local code=$1 f=$2; shift 2
  local O=$W/out-neg; rm -rf "$O"; mkdir -p "$O"
  out=$("$CLI" "$R" "$f" --out-dir "$O" "$@" 2>&1) && rc=0 || rc=$?
  if [ $rc -eq 1 ] && grep -q "^PROBLEM $code\$" <<<"$out" && [ -z "$(ls -A "$O")" ]; then
    ok "$code: $(grep -A1 '^PROBLEM' <<<"$out" | tail -1 | sed 's/^ *//')"
  else bad "$code (rc $rc)"; echo "$out"; fi
}
expect wrong_region     "$W/synthetic-eu.img"
expect other_game       "$W/synthetic-othergame.img"
expect incomplete       "$W/synthetic-truncated.img"
expect modified         "$W/synthetic-changed.img"
expect already_patched  "$W/synthetic-mod.img"
expect older_patch      "$W/synthetic-older.img"
expect archive          "$W/synthetic-packed.zip"
expect compressed_image "$W/synthetic-compressed.cso"
expect empty            "$W/synthetic-empty.img"
expect is_folder        "$W"
expect not_found        "$W/does-not-exist.img"
expect not_disc         "$R/patch.vcdiff"
RO=$W/readonly; mkdir -p "$RO"; chmod 555 "$RO"
out=$("$CLI" "$R" "$W/synthetic-clean.img" --out-dir "$RO" 2>&1) || true
grep -q "^PROBLEM cant_write" <<<"$out" && ok "cant_write: read-only folder" || { bad "cant_write"; echo "$out"; }
chmod 755 "$RO"

# 4) a patch made with secondary compression is refused cleanly (developer error, no file left)
MOD=$W/synthetic-mod-text.img mkrel "$W/rel-djw" -9 -S djw
MOD=$W/synthetic-mod-text.img mkrel "$W/rel-text" -9 -S none
O=$W/out-text; rm -rf "$O"; mkdir -p "$O"
"$CLI" "$W/rel-text" "$W/synthetic-clean.img" --out-dir "$O" > /dev/null && cmp -s "$O/Synthetic Test Output.img" "$W/synthetic-mod-text.img" \
  && ok "roundtrip text tail (-S none $(size "$W/rel-text/patch.vcdiff") B vs -S djw $(size "$W/rel-djw/patch.vcdiff") B)" || bad "roundtrip text"
O=$W/out-djw; rm -rf "$O"; mkdir -p "$O"
out=$("$CLI" "$W/rel-djw" "$W/synthetic-clean.img" --out-dir "$O" 2>&1) || true
grep -q "E-PATCH" <<<"$out" && [ -z "$(ls -A "$O")" ] && ok "djw patch refused (E-PATCH), nothing left behind" || { bad "djw"; echo "$out"; }

# 5) a corrupted patch byte is caught (Adler-32 / hash), nothing left behind
mkdir -p "$W/rel-corrupt"; cp "$R/release.json" "$W/rel-corrupt/"
python3 -I -c "
import sys; b=bytearray(open(sys.argv[1],'rb').read()); b[len(b)//2]^=0x40; open(sys.argv[2],'wb').write(b)" \
  "$R/patch.vcdiff" "$W/rel-corrupt/patch.vcdiff"
O=$W/out-corrupt; rm -rf "$O"; mkdir -p "$O"
out=$("$CLI" "$W/rel-corrupt" "$W/synthetic-clean.img" --out-dir "$O" 2>&1) || true
grep -q "^PROBLEM build_failed" <<<"$out" && [ -z "$(ls -A "$O")" ] && ok "corrupt patch: $(grep -o 'E-[A-Z]*' <<<"$out" | head -1), nothing left behind" || { bad "corrupt"; echo "$out"; }

echo "== $PASS passed, $FAIL failed (workdir $W)"
[ $FAIL -eq 0 ]
