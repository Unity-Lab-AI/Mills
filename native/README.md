# VibeRoller native (Rust)

One Cargo workspace for the Rust side of VibeRoller: `crates/vr-core` (shared library) now; the AI host (`vr-host`), the
desktop app (Tauri) and the hub follow (board T-125 … T-134).

- Toolchain: rustup in user space for the hub's account (board T-122), plus the `x86_64-pc-windows-gnu` target and a
  mingw-w64 linker for Windows builds (board T-123).
- Build: `cargo build --release` (Linux), `cargo build --release --target x86_64-pc-windows-gnu` (Windows).
- Test: `cargo test`. The JavaScript hub and host remain the reference; the regression suite
  (`node bridge/test/regress.js`) decides when a Rust part can replace its JavaScript one.
- Build output (`native/target/`) is never committed.
