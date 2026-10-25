# Sora fan patch app (source)

Source code of the small desktop app that adds the fan-made fighter Sora to **your own** US copy of
Dissidia 012 Duodecim Final Fantasy for the PSP (disc code ULUS-10566, `.iso` file). Made by **Mat**.

> No Dissidia files are included. Sora's model and sounds are fan-ported from Kingdom Hearts II.
> Fan-made, not affiliated with Square Enix or Disney.
>
> Built by Mat with help from AI tools (Claude, Codex).

## What it does

1. Checks the file you give it: right game, US version, complete and unmodified (SHA-256 of the whole file).
2. Reads the game's `EBOOT.BIN` from your file and decrypts it in memory (nothing is written).
3. Applies the patch (our changes only, VCDIFF) to make a **new** file next to the original. Your file is only
   ever read.
4. Checks the new file's SHA-256 before giving it its real name. Anything wrong, a cancel or a crash leaves no
   half-written file behind.

Everything runs offline, in pure Rust: no external programs, no C code, no downloads at run time.

## Patch only: what is and isn't here

- **Here:** the program (core library, command-line tool, desktop app), its tests and the CI workflow.
- **Never here, never in the app:** Dissidia files of any kind (no ISO, no EBOOT, encrypted or decrypted, no
  extracted game data). The tests use small synthetic files made by `tests/run_synthetic.sh` (a 3 MiB fake "disc").
- **Release data** (`release.json` + `patch.vcdiff`) holds only our changes: the new code and Sora's model and
  sounds, fan-ported from Kingdom Hearts II. It is checked to contain no original Dissidia code or unchanged disc
  data, is kept separate from the program and is not committed here; it is embedded into the app at build time
  (see below).

## Layout

```
core/      sorapatch-core: file checks, SHA-256, ISO path lookup, EBOOT decrypt (pspcrypt.rs),
           streaming VCDIFF decoder (vcdiff.rs), safe temp file + rename
cli/       sorapatch-cli: the same steps and texts on the command line (tests, CI, power users)
app/ui/    the window: HTML/CSS/JS, all player-facing texts in strings.en.js
app/src-tauri/  Tauri v2 shell; build.rs embeds the release chosen by SORAPATCH_RELEASE_DIR
app/Dockerfile  Linux build box (Ubuntu 22.04 + WebKitGTK 4.1 dev packages)
mock/      look review page (open mock/index.html in a browser)
tests/     run_synthetic.sh + run_packs_synthetic.sh (end-to-end checks), app_autotest.sh / app_release_gui.sh
           (real window, Xvfb)
.github/workflows/build.yml   Windows / macOS / Linux builds
```

## Build

Needs Rust (1.80 or newer; CI uses 1.97.1). For the tests also `xdelta3` and `python3`.

```sh
cargo test --release            # core + cli unit tests
tests/run_synthetic.sh          # synthetic end-to-end checks; also creates testdata/rel-default
tests/run_packs_synthetic.sh    # the same for pack releases (fighter checklist)
target/release/sorapatch-cli --check-strings
```

Command-line use: `sorapatch-cli RELEASE_DIR YOUR_GAME.iso [--out-dir DIR]`.

**Pack releases (fighter-pack checklist).** A release folder with `release.json` format 3 holds `packs/*.vcdiff`
(one per pack: its own bytes, rebuilt in memory against your disc) and `sets/*.vcdiff` (one small patch per mix of
fighters). The app shows a "Fighters to add" checklist (all ticked); the result file's SHA-256 and the fighter list
inside it (roster id) are checked before it gets its name. Command line: `sorapatch-cli RELEASE_DIR YOUR_GAME.iso
--packs sora` (default: all fighters), `sorapatch-cli RELEASE_DIR --list`. Tests: `tests/run_packs_synthetic.sh`.
A release marked `local_test` is refused by the app build (`SORAPATCH_ALLOW_LOCAL_TEST=1` for local tests only).

**Desktop app.** `SORAPATCH_RELEASE_DIR=<release folder>` (release.json + patch.vcdiff, or release.json + packs/ + sets/)
chooses the embedded release;
without it the synthetic test release from `tests/run_synthetic.sh` is embedded (a test build that cannot patch
the game). Bundles need the Tauri CLI (`cargo install tauri-cli --version 2.12.3 --locked`):

```sh
cd app/src-tauri
cargo tauri build                      # on the OS you build for
```

- **Linux:** needs the WebKitGTK 4.1 dev packages (see `app/Dockerfile`, or build inside that box:
  `docker build -t sorapatch-build app`). `cargo tauri build --bundles appimage` makes the AppImage.
- **Windows / macOS:** build on that OS, or let CI do it (below).

## CI builds and release data

`.github/workflows/build.yml` runs the tests and builds a Windows setup `.exe` + portable `.exe`, macOS `.dmg`
(Apple Silicon and Intel) and a Linux AppImage. Every build ships `LICENSE.txt` and `SOURCE.txt` (a link to the
exact commit). Builds are uploaded as workflow artifacts; nothing is published automatically.

- Pushes and pull requests build with the **synthetic test release** (artifacts named `…-synthetic-test`).
- A real build: Actions → **build** → **Run workflow**, with `release_zip_url` = a link to a zip holding
  `release.json` + `patch.vcdiff` (+ `README.txt`) at its top level, or for a pack release `release.json` +
  `packs/*.vcdiff` + `sets/*.vcdiff` (+ `README.txt`), and `release_zip_sha256` = the zip's SHA-256.
  The run refuses a wrong checksum, any other file in the zip, a pack without a `blob` (raw pack data) and a
  release marked `local_test`.

## First start of an unsigned app

The downloads are not code-signed, so the system warns once:

- **Windows:** "Windows protected your PC" → **More info** → **Run anyway**.
- **macOS:** the first open is blocked → **System Settings → Privacy & Security → Open Anyway** (older macOS:
  right-click the app → **Open**). If macOS says the app "is damaged", run
  `xattr -dr com.apple.quarantine "/Applications/Sora Fan Patch.app"` in Terminal.
- **Linux:** make the AppImage executable (Properties → "Allow executing", or `chmod +x`). Some distributions
  need `libfuse2`.

## Versions and where it runs

What changed in each version: [CHANGELOG.md](CHANGELOG.md). The patched game is tested in PPSSPP (PC) and on PS Vita
with Adrenaline (versions 1.0 and 1.1). A real PSP is not tested yet.

## Licence and credits

Copyright © 2026 Mat. This program is free software under the **GNU General Public License v3.0 or later**
(see [LICENSE](LICENSE)).

The EBOOT decryption in `core/src/pspcrypt.rs` is a Rust port (written fresh, no C copied) of the PSP executable
decryption in:

- **pspdecrypt** by John-K and contributors, <https://github.com/John-K/pspdecrypt> (GPL-3.0),
- whose PRX decrypter comes from **PPSSPP**, `Core/ELF/PrxDecrypter.cpp`, <https://github.com/hrydgard/ppsspp>
  (GPL-2.0-or-later),
- and whose KIRK engine is **libkirk** by Draan, with help from coyotebean, Davee, hitchhikr, kgsws, liquidzigong,
  Mathieulh, Proxima and SilverSpring (GPL-3.0-or-later).

Only the "type 2" path for tag `D91613F0`, KIRK commands 7 and 1 and the command-10 CMAC checks were ported; the
key bytes are the publicly known PSP keys from those projects. All three licences allow combination under
GPL-3.0-or-later. Rust dependencies (RustCrypto, serde, fs4, Tauri, …) are MIT/Apache-2.0.

All art (the star emblem, the sky) is our own and drawn in code (`app/src-tauri/icons/make_icons.py`, CSS).
