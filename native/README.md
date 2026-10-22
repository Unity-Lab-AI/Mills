# VibeRoller native (Rust)

One Cargo workspace for the Rust side of VibeRoller. Since 5.0.0 most of the code is in reusable libraries
(`crates/pgm-*-internal`, names and order: `crates/NAMES.md`); the programs are thin around them:

- `crates/vr-hub`: the hub in front of the JavaScript one (its routes, its state, the `ChatHost` / `AiHub` traits)
- `crates/vr-host`: the AI host program (control app, install, Windows service; the job loop is pgm-ai-internal's)
- `crates/vr-desktop`: the desktop app (Tauri)
- `crates/vr-core`: re-exports only, so older `vr_core::…` paths keep working

Building and testing:
- Toolchain: rustup in user space for the hub's account, plus the `x86_64-pc-windows-gnu` target and a mingw-w64
  linker for Windows builds.
- `sh build.sh check` checks both targets; `cargo build --release` (Linux), `cargo build --release --target
  x86_64-pc-windows-gnu` (Windows).
- `cargo test --workspace --exclude vr-desktop`. The regression suite (`node bridge/test/regress.js`, also with
  `--rust-hub`) decides what ships.
- Build output (`native/target/`) is never committed.
