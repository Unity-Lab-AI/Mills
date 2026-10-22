# pgm-dist-internal: PGM_Dist_Internal (PsyGraphMediaDistributionInternal)

Shipping programs.

- `releases::Releases::at(dist, key_file)` (or `new(root)` for VibeRoller's layout): `signed(app, target,
  host_secret, (version, commit))` → `{ manifest, sig, hmac? }`, `file(app, target, name)`, `list(rel)`
- `update`: `verify` / `verify_with(signed, host_secret, pubkey)`, `swap_in(exe, bytes, sha256)` (old one kept as
  `.old`), `selftest`, `roll_back`, `self_paths`, `newer`, `ed25519_sign` / `ed25519_ok` / `ed25519_new`,
  `RELEASE_PUBKEY` (built in with VR_RELEASE_PUBKEY)
- `zip`: `zip(&[Entry])`, `e`, `crlf`
- `version`: `release_of(checkout)` → (version, commit); `Build` (read once at start: `get` → (build name, modules),
  `commit`)

## Use it in another project

```rust
use pgm_dist_internal::{update, releases::Releases};
let r = Releases::at("dist".into(), "release-key.json".into());
if let Some(signed) = r.signed("my-app", "x86_64-unknown-linux-gnu", None, &("1.2.0".into(), "abc".into())) { /* serve it */ }
// in the program itself: verify what the server sent, then swap its own binary in
let m = update::verify_with(&signed_from_server, None, MY_PUBKEY_HEX)?;
```
