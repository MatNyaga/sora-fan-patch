#!/bin/bash
# End-to-end test of the PACK RELEASE path (release.json format 3, P2.11: fighter-pack checklist) on SMALL
# SYNTHETIC files only (no game data, no .iso names).
#   tests/run_packs_synthetic.sh [WORKDIR]     (default: testdata/packs in the installer workspace)
# Makes a fake "disc" (ISO9660 tree with UMD_DATA.BIN + PSP_GAME/USRDIR/DATA), an engine pack and two fighter
# packs (random bytes = "our own data"), three result discs that carry EXTRA.REG / EXTRA.IDX with a roster id and
# COPY from those packs, encodes each set patch with the real xdelta3 against "disc ++ engine ++ packs", then checks
# every pack-release rule with sorapatch-cli. Needs: cargo, xdelta3, python3.
set -euo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd)
W=${1:-$HERE/testdata/packs}
rm -rf "$W"
mkdir -p "$W"
W=$(cd "$W" && pwd)
case "$W" in */dumps*) echo "refusing to work under dumps/"; exit 2;; esac
cd "$HERE"
nice cargo build --release -q -p sorapatch-cli
CLI=${CARGO_TARGET_DIR:-$HERE/target}/release/sorapatch-cli

python3 -I - "$W" <<'PY'
import hashlib, json, os, random, struct, subprocess, sys
w = sys.argv[1]
rnd = random.Random(2011)
rb = lambda n: bytes(rnd.getrandbits(8) for _ in range(n))

def iso(files, pad_to=0):
    """ISO9660 tree, root at sector 18, one sector per directory (enough for the core's iso_find)."""
    dirs = ['']
    for p, _ in files:
        parts = p.split('/')
        for k in range(1, len(parts)):
            d = '/'.join(parts[:k])
            if d not in dirs:
                dirs.append(d)
    dlba = {d: 18 + i for i, d in enumerate(dirs)}
    nxt = 18 + len(dirs)
    flba = []
    for _, b in files:
        flba.append(nxt)
        nxt += max(1, -(-len(b) // 2048))
    img = bytearray(max(nxt * 2048, pad_to))
    pvd = 16 * 2048
    img[pvd] = 1; img[pvd + 1:pvd + 6] = b'CD001'
    img[pvd + 156] = 34; img[pvd + 158:pvd + 162] = struct.pack('<I', 18); img[pvd + 166:pvd + 170] = struct.pack('<I', 2048)
    def rec(at, lba, size, name, is_dir):
        rl = 33 + len(name) + (1 - len(name) % 2)
        img[at] = rl; img[at + 2:at + 6] = struct.pack('<I', lba); img[at + 10:at + 14] = struct.pack('<I', size)
        img[at + 25] = 2 if is_dir else 0; img[at + 32] = len(name); img[at + 33:at + 33 + len(name)] = name
        return at + rl
    parent = lambda p: p.rsplit('/', 1)[0] if '/' in p else ''
    for d in dirs:
        at = rec(dlba[d] * 2048, dlba[d], 2048, b'\0', True)
        at = rec(at, 18, 2048, b'\1', True)
        for c in dirs:
            if c and parent(c) == d:
                at = rec(at, dlba[c], 2048, c.rsplit('/', 1)[-1].encode(), True)
        for i, (p, b) in enumerate(files):
            if parent(p) == d:
                at = rec(at, flba[i], len(b), (p.rsplit('/', 1)[-1] + ';1').encode(), False)
    for i, (_, b) in enumerate(files):
        img[flba[i] * 2048:flba[i] * 2048 + len(b)] = b
    return bytes(img)

UMD = ('UMD_DATA.BIN', b'ULUS-10566|0123456789ABCDEF|0001|G')
D = 'PSP_GAME/USRDIR/DATA/'
big = rb(2_500_000)
clean = iso([UMD, (D + 'BIG.BIN', big)], 3 << 20)
eng, pb = rb(20_000), rb(150_000)
# pack a is shipped as a VCDIFF ("pack delta"): its pack data = some of the disc's own bytes + its own bytes;
# pack b is shipped raw (its pack data is only its own bytes). Both forms are format 3.
pa = big[100_000:160_000] + rb(200_000)
def reg(rid):
    return b'XREG' + struct.pack('<HH', 1, 0x48) + bytes(0x38) + struct.pack('<II', rid, 0x2F2EAE84) + rb(100)
def idx(rid):
    return b'XIDX' + struct.pack('<HH', 1, 0x24) + bytes(0x18) + struct.pack('<I', rid) + rb(24)
RID = {'a': 0x11111111, 'b': 0x22222222, 'a+b': 0x33333333}
def target(key, blobs):
    edited = bytearray(big); edited[1000:1100] = rb(100)     # a vanilla file the set changes a little
    files = [UMD, (D + 'BIG.BIN', bytes(edited)), (D + 'EXTRA.REG', reg(RID[key])), (D + 'EXTRA.IDX', idx(RID[key])),
             (D + 'MODULE.PRX', eng)] + [(D + 'PACK%d.BIN' % i, b) for i, b in enumerate(blobs)]
    return iso(files, len(clean) + 64 * 1024)   # results are bigger than the clean disc, like real ones
tgt = {'a': target('a', [pa]), 'b': target('b', [pb]), 'a+b': target('a+b', [pa, pb])}
os.makedirs(f'{w}/rel/packs'); os.makedirs(f'{w}/rel/sets')
for name, b in (('engine', eng), ('b', pb)):
    open(f'{w}/rel/packs/{name}.bin', 'wb').write(b)
open(f'{w}/clean.img', 'wb').write(clean)
open(f'{w}/src.tmp', 'wb').write(clean + eng)
open(f'{w}/blob-a.tmp', 'wb').write(pa)
subprocess.run(['xdelta3', '-e', '-9', '-S', 'none', '-B', str(1 << 26), '-f', '-s', f'{w}/src.tmp', f'{w}/blob-a.tmp',
                f'{w}/rel/packs/a.vcdiff'], check=True)
os.remove(f'{w}/blob-a.tmp')
pa_delta = open(f'{w}/rel/packs/a.vcdiff', 'rb').read()
sha = lambda b: hashlib.sha256(b).hexdigest()
data = lambda f, b: {'file': f, 'size': len(b), 'sha256': sha(b)}
sets = []
blob = {'engine': eng, 'a': pa, 'b': pb}
for key in ('a', 'a+b'):            # 'b' alone is NOT offered (pack_mix case)
    packs = key.split('+')
    src = clean + eng + b''.join(blob[p] for p in packs)
    open(f'{w}/src.tmp', 'wb').write(src)
    open(f'{w}/tgt-{key}.img', 'wb').write(tgt[key])
    patch = f'{w}/rel/sets/{key}.vcdiff'
    subprocess.run(['xdelta3', '-e', '-9', '-S', 'none', '-B', str(1 << 26), '-f', '-s', f'{w}/src.tmp', f'{w}/tgt-{key}.img', patch], check=True)
    sets.append({'packs': packs, 'source_packs': ['engine'] + packs, 'file_name': 'Synthetic + %s (test).img' % ' + '.join(p.upper() for p in packs),
                 'size': len(tgt[key]), 'sha256': sha(tgt[key]), 'roster_id': '%08x' % RID[key],
                 'patch': data(f'sets/{key}.vcdiff', open(patch, 'rb').read())})
os.remove(f'{w}/src.tmp')
older = bytearray(tgt['a']); older[-1] ^= 0xFF
open(f'{w}/older.img', 'wb').write(older)
open(f'{w}/tgt-b.img', 'wb').write(tgt['b'])
man = {'format': 3, 'title': 'Synthetic pack test', 'version': '9.9',
       'input': {'disc_id': 'ULUS-10566', 'size': len(clean), 'sha256': sha(clean)},
       'packs': [{'id': 'engine', 'kind': 'engine', 'name': 'Fighter engine', 'always': True, 'version': '2.6.0',
                  'package_sha256': 'e' * 64, 'data': data('packs/engine.bin', eng)},
                 {'id': 'a', 'kind': 'fighter', 'name': 'Alpha', 'blurb': 'first test pack', 'owner': 33, 'version': '1.0.0',
                  'package_sha256': 'a' * 64, 'data': data('packs/a.vcdiff', pa_delta),
                  'blob': {'size': len(pa), 'sha256': sha(pa)}, 'blob_source': ['engine']},
                 {'id': 'b', 'kind': 'fighter', 'name': 'Beta', 'blurb': 'second test pack', 'owner': 34, 'version': '1.0.0',
                  'package_sha256': 'b' * 64, 'data': data('packs/b.bin', pb)}],
       'sets': sets,
       'older_outputs': [{'version': '9.0', 'sha256': sha(bytes(older)), 'names': 'Alpha'}],
       'other_regions': {'ULES-01505': 'European'}}
json.dump(man, open(f'{w}/rel/release.json', 'w'), indent=1)
print('synthetic pack release: clean %d B, packs engine %d / a %d (delta %d) / b %d B, sets %s' % (len(clean), len(eng), len(pa), len(pa_delta), len(pb),
      ', '.join('%s %d B' % (s['patch']['file'], s['patch']['size']) for s in sets)))
PY

pass=0; fail=0
ok()  { echo "PASS $1"; pass=$((pass+1)); }
bad() { echo "FAIL $1"; fail=$((fail+1)); }
shaof() { sha256sum "$1" | cut -d' ' -f1; }
want() { python3 -I -c "import json,sys; m=json.load(open(sys.argv[1])); print([s['sha256'] for s in m['sets'] if '+'.join(s['packs'])==sys.argv[2]][0])" "$W/rel/release.json" "$1"; }
REL=$W/rel

# build cases: ticked set -> exact file, exact name, nothing else in the folder
build_case() {  # name packs-arg key
    local o=$W/out-$1; rm -rf "$o"; mkdir -p "$o"
    if "$CLI" "$REL" "$W/clean.img" --out-dir "$o" $2 > "$o.log" 2>&1 && [ "$(ls "$o" | wc -l)" = 1 ] \
        && [ "$(shaof "$o"/*)" = "$(want "$3")" ]; then ok "$1 ($(ls "$o"))"; else bad "$1"; cat "$o.log"; fi
}
build_case tick-a "--packs a" a
build_case tick-ab "--packs b,a" a+b
build_case default-all "" a+b
cmp -s "$W/out-tick-a/Synthetic + A (test).img" "$W/tgt-a.img" && ok "set a = expected bytes" || bad "set a bytes"

problem_case() {  # name expected-code input [args...]
    local n=$1 code=$2 in=$3; shift 3
    local o=$W/out-$n; rm -rf "$o"; mkdir -p "$o"
    set +e; "$CLI" "$REL" "$in" --out-dir "$o" "$@" > "$o.log" 2>&1; local rc=$?; set -e
    if [ $rc = 1 ] && grep -q "PROBLEM $code" "$o.log" && [ -z "$(ls -A "$o")" ]; then ok "$n -> $code, nothing written"
    else bad "$n (rc $rc)"; cat "$o.log"; ls -A "$o"; fi
}
problem_case only-b pack_mix "$W/clean.img" --packs b
problem_case none-ticked no_pack "$W/clean.img" --packs ""
problem_case unknown-pack pack_mix "$W/clean.img" --packs zeta
problem_case already-a already_has "$W/out-tick-a/Synthetic + A (test).img"
grep -q "already has Alpha!" "$W/out-already-a.log" && ok "already_has names the fighters (Alpha)" || bad "already_has text"
problem_case already-ab already_has "$W/tgt-a+b.img"
grep -q "already has Alpha and Beta!" "$W/out-already-ab.log" && ok "already_has names both (Alpha and Beta)" || bad "already_has ab text"
problem_case older older_has "$W/older.img"
problem_case notoffered-result modified "$W/tgt-b.img"

# damaged release files: refused, nothing left behind
cp -r "$REL" "$W/rel-bad"
python3 -I -c "import sys; p=sys.argv[1]; b=bytearray(open(p,'rb').read()); b[500]^=1; open(p,'wb').write(b)" "$W/rel-bad/packs/a.vcdiff"
REL=$W/rel-bad problem_case damaged-pack build_failed "$W/clean.img" --packs a
grep -q "code: E-SUM\|E-SUM" "$W/out-damaged-pack.log" && ok "damaged pack data = E-SUM" || bad "damaged pack code"
# the a+b patch never reads pack a's damaged byte? it does (COPY from pack a) -> also E-SUM before writing
REL=$W/rel-bad problem_case damaged-pack-ab build_failed "$W/clean.img" --packs a,b
rm -rf "$W/rel-bad"
# the manifest's pack-data fingerprint does not match what the pack delta rebuilds -> E-SUM, nothing kept
cp -r "$REL" "$W/rel-blob"
python3 -I -c "
import json,sys; p=sys.argv[1]; m=json.load(open(p)); m['packs'][1]['blob']['sha256']='0'*64; json.dump(m,open(p,'w'))" "$W/rel-blob/release.json"
REL=$W/rel-blob problem_case wrong-pack-data build_failed "$W/clean.img" --packs a
grep -q "pack data does not match" "$W/out-wrong-pack-data.log" && ok "rebuilt pack data checked (E-SUM)" || bad "pack data check"
rm -rf "$W/rel-blob"
# wrong roster id in release.json while the result sha is right -> E-ROSTER, nothing kept
cp -r "$REL" "$W/rel-rid"
python3 -I -c "
import json,sys; p=sys.argv[1]; m=json.load(open(p)); m['sets'][0]['roster_id']='deadbeef'; json.dump(m,open(p,'w'))" "$W/rel-rid/release.json"
REL=$W/rel-rid problem_case wrong-roster build_failed "$W/clean.img" --packs a
grep -q "E-ROSTER" "$W/out-wrong-roster.log" && ok "roster id mismatch = E-ROSTER" || bad "roster code"
rm -rf "$W/rel-rid"
# release folder problems: exit 2 at load (no input read)
cp -r "$REL" "$W/rel-miss"; rm "$W/rel-miss/sets/a.vcdiff"
set +e; "$CLI" "$W/rel-miss" "$W/clean.img" > "$W/miss.log" 2>&1; rc=$?; set -e
[ $rc = 2 ] && grep -q "sets/a.vcdiff" "$W/miss.log" && ok "missing set patch -> release error" || { bad "missing set patch"; cat "$W/miss.log"; }
rm -rf "$W/rel-miss"
cp -r "$REL" "$W/rel-test"
python3 -I -c "
import json,sys; p=sys.argv[1]; m=json.load(open(p)); m['packs'][2]['test']=True; json.dump(m,open(p,'w'))" "$W/rel-test/release.json"
set +e; "$CLI" "$W/rel-test" "$W/clean.img" > "$W/test.log" 2>&1; rc=$?; set -e
[ $rc = 2 ] && grep -q "test pack" "$W/test.log" && ok "test pack outside a local_test release -> refused" || { bad "test pack"; cat "$W/test.log"; }
rm -rf "$W/rel-test"
"$CLI" "$REL" --list > "$W/list.log" && grep -q "^pack a 1.0.0 \[fighter\] Alpha" "$W/list.log" && grep -q "^set a+b -> " "$W/list.log" \
    && ok "--list" || { bad "--list"; cat "$W/list.log"; }
# a format-3 run never touches the input
[ "$(shaof "$W/clean.img")" = "$(python3 -I -c "import json; print(json.load(open('$REL/release.json'))['input']['sha256'])")" ] \
    && ok "input unchanged" || bad "input changed"
echo "pack-release synthetic: $pass passed, $fail failed"
[ $fail = 0 ]
