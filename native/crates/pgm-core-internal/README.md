# pgm-core-internal: PGM_Core_Internal (PsyGraphMediaCoreInternal)

The basics every PsyGraphMedia library and program shares. Nothing here knows about any one product.

- `Error` / `Result`: an error meant for a person, with an HTTP-like status (`Error::status(404, "…")`, `.code()`)
- `b64` / `unb64` (standard base64, URL-safe accepted), `now_ms` (Date.now())
- `crypto`: `random_bytes` / `random_hex`, `sha256` / `sha256_hex`, `hmac_hex` / `hmac_ok` (constant time),
  `aes_gcm_seal` / `aes_gcm_open` (tag appended, as WebCrypto), `hkdf32`
- `time`: `iso_now` / `iso_at` (exactly JavaScript's `toISOString()`), `civil`
- `json`: `read_json`, `write_json` (atomic: temp file + rename), `write_json_private` (and owner-only), `s` / `s_any`
- `fs`: `private` (owner-only: Linux mode 600, Windows ACL via icacls), `readable_by_others`
- `text`: `urlencode` (encodeURIComponent)

## Use it in another project

```toml
[dependencies]
pgm-core-internal = { path = "../VibeRoller/native/crates/pgm-core-internal" }
```

```rust
use pgm_core_internal::{json, time, crypto};
let f = std::path::Path::new("state.json");
json::write_json_private(f, &serde_json::json!({ "at": time::iso_now(), "id": crypto::random_hex(8) }));
```
