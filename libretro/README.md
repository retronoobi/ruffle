# Ruffle libretro (Windows x64, OpenGL)

Experimental offline Flash core for RetroArch 1.7.5 and EmuVR. This branch builds
and publishes **only the libretro core**. The standalone, web player, Vulkan and
DirectX backends are not part of this build.

The port is based on Ruffle commit `8c3ac234252f60af93687f116c5474ae16a03b2e`.
Ruffle and this core are licensed under MIT OR Apache-2.0; see `LICENSE.md`.

## Install

Download the `ruffle-libretro-vVERSION-windows-x86_64.zip` release asset. With the
game closed, copy:

- `ruffle_libretro.dll` to `RetroArch/cores`.
- `ruffle_libretro.info` to `RetroArch/info`.

Load a local `.swf`. OpenGL 3.3-capable hardware is required for the shader
features. The frontend is deliberately asked for the legacy OpenGL compatibility
context: requesting a 3.3 context directly breaks RetroArch 1.7.5 initialization.

**Network access is permanently disabled.** HTTP/HTTPS, sockets, external URL
navigation, UNC paths, mapped network drives and file redirection/reparse points
are refused. There is no setting to enable them. Games requiring servers lose
those features. This restriction applies to the core, not to GitHub Actions
downloading build dependencies.

## Controls and options

Mouse: movement, three buttons and wheel. Keyboard: keys, text input and editing.
Polling-only frontends also support basic US-layout text input; Unicode text
is preserved when character callbacks are supplied.

The right analog stick moves the cursor; L2/R2/L3 click left/right/middle. The
left stick and D-pad produce direction keys. Other joypad keys can be remapped.

Core options include quality, scale, alignment, Flash/AIR runtime, player version,
loading behavior, frame rate, volume, cursor size, storage and gamepad mappings.
Quality defaults to **high**, with medium/low available without restarting.
Current frontend quality is reapplied after loading a state. Automatic rendering
skip can repeat an image under load while simulation and audio keep advancing.

Per-game overrides can be saved beside the movie as `Game.ruffle.json`:

```json
{
  "ruffle_quality": "high",
  "ruffle_cursor_size": "32",
  "parameters": { "exampleFlashVar": "value" }
}
```

SharedObjects are stored below the frontend save directory in `ruffle/`, using
SHA-256 names. Memory-only storage is also available. No SRAM is exposed.

## Save states and compatibility limits

Standard save/load slots are supported through **reconstruction**, not a memory
dump. A compressed state records inputs, render decisions, quality changes,
initial settings/storage and the clock. Loading creates a new player and replays
the history, including audio decoding, until the checkpoint. Long sessions take
time to load. A checksum, SWF digest and audio-history signature detect invalid or
divergent reconstruction. The libretro serialization buffer is fixed at 16 MiB.

Use the same SWF and keep its local companion assets available. States from
0.3.0–0.3.2 are accepted; compatibility with future versions is not guaranteed.
Rewind, run-ahead and netplay are unsupported. Context loss currently restarts
the player; a saved state can then be loaded manually.

The native renderer covers 2D shapes, bitmaps, gradients, masks, blend modes and
filters. Some CPU filters approximate Flash behavior. PixelBender has native
OpenGL tests. **Stage3D remains experimental and has not received broad game
compatibility testing.** This release does not claim full Flash compatibility.

## Build

Requirements:

- Windows x64, PowerShell 7, Git and Python 3.11+.
- Rust **1.98.1**, GNU host `x86_64-pc-windows-gnu`, with rustfmt and Clippy.
- MSYS2 UCRT64 GCC/linker on `PATH`.
- Java 17 on `PATH`, to compile Ruffle's playerglobal.

For example, install/select the Rust toolchain with:

```powershell
rustup toolchain install 1.98.1-x86_64-pc-windows-gnu --profile minimal --component rustfmt --component clippy
$env:RUSTUP_TOOLCHAIN = "1.98.1-x86_64-pc-windows-gnu"
```

Then, from the repository root:

```powershell
cargo fmt -p ruffle_libretro --check
cargo clippy -p ruffle_libretro --all-targets --no-deps --locked -- -D warnings
./libretro/build.ps1 -Task Test
./libretro/build.ps1 -Task Package
```

The ZIP, checksum and release notes appear in `libretro/dist`. Build scripts use
`PATH`, `CARGO_HOME` and `CARGO_TARGET_DIR`; no personal installation paths are
required. Use `-Offline` after dependencies have been cached.

Graphics contracts require SDL2 and a working desktop OpenGL driver:

```powershell
./libretro/build.ps1 -Task Test -GraphicsTests -SdlPath "C:/path/to/SDL2.dll"
```

These tests are explicitly ignored in ordinary CI: hosted Windows runners do
not provide the tested desktop OpenGL environment. CI still runs the pure unit
tests and loads the produced DLL to verify its exported API and metadata.
No Flash games, save files or proprietary test assets are included in releases.

Set `RUFFLE_TEST_OUTPUT` to redirect native test output. Without it, output goes
to `target/libretro-tests`. Optional `RUFFLE_STATE_GAME`/`RUFFLE_STATE_LOAD` test
variables can reconstruct a local state and write pictures, PCM and timings.

## Automatic builds and releases

`.github/workflows/libretro.yml` runs only this core's checks/build on relevant
pushes and pull requests. It uploads a Windows x64 ZIP and SHA-256 checksum.
Upstream desktop/web/translation workflows are intentionally absent on this
dedicated branch.

To publish a version, update `libretro/Cargo.toml`, update the lockfile, commit,
then push a matching tag:

```powershell
git tag libretro-v0.3.3
git push origin libretro-v0.3.3
```

Tag builds reject a mismatch with the crate version. After formatting, Clippy,
unit tests and ABI/package checks pass, a separate job creates the GitHub Release
with only the core ZIP and its checksum. Only that job has `contents: write`;
third-party actions are pinned to full commit IDs. No standalone, web builds,
game files or local test data are published.
