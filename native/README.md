# VibeRoller native (Rust)

One Cargo workspace for the Rust side of VibeRoller. Since 5.0.0 most of the code is in reusable libraries
(`crates/pgm-*-internal`, names and order: `crates/NAMES.md`); the programs are thin around them:

- `crates/vr-hub`: the hub in front of the JavaScript one (its routes, its state, the `ChatHost` / `AiHub` traits)
- `crates/vr-host`: the AI host program (control app, install, Windows service; the job loop is pgm-ai-internal's)
- `crates/vr-desktop`: the desktop app (Tauri). It also carries **Desktop Unity**: a see-through child window over the
  app's window (`ui/companion.html` / `.js`, built-in clips in `ui/companion/`), hidden while the app is minimised,
  and the scripts that film her looks on that PC (`companion-look.py`, `companion-do.py`, `companion-clips.py`, run
  hidden with the image engine's Python). `vr-desktop --companion <move> [--look <text>]` gives her an order from a
  terminal; Settings → App → Desktop Unity has On / Off, her look, "Tell her" and Reset.
- Every program the native code starts on Windows is started without a console window (`CREATE_NO_WINDOW`), so no
  black boxes flash up.
- `crates/vr-core`: re-exports only, so older `vr_core::…` paths keep working

Building and testing:
- Toolchain: rustup in user space for the hub's account, plus the `x86_64-pc-windows-gnu` target and a mingw-w64
  linker for Windows builds.
- `sh build.sh check` checks both targets; `cargo build --release` (Linux), `cargo build --release --target
  x86_64-pc-windows-gnu` (Windows).
- `cargo test --workspace --exclude vr-desktop`. The regression suite (`node bridge/test/regress.js`, also with
  `--rust-hub`) decides what ships.
- Build output (`native/target/`) is never committed.
