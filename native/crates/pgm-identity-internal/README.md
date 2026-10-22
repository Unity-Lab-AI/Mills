# pgm-identity-internal: PGM_Identity_Internal (PsyGraphMediaIdentityInternal)

Who someone is.

- `Auth::new(root, default_user, behind_proxy, tag)`: the accounts under `root` (the default account is `root`
  itself, others `root/profiles/<name>` with config.json / auth.json)
  - `login(&req, refuse) -> Login` (`Answer(status, json)` or `SignedIn { token, user, client, ip }`): scrypt
    password, device lock (several devices), per-address limits (5 / 20 / 3 at once)
  - `session_user(&req)` (Bearer token → account), `forget_session`, `drop_sessions`, `signed_in`
  - `accounts`, `account`, `is_admin`, `lock_of`, `unlock`, `allow_device`, `outside_ip`
- `check_password` / `hash_password` (the same scrypt as Node's crypto.scryptSync), `new_password`, `token_key`,
  `account_key` (HMAC of a secret and an account), `lock_devices`, `valid_username`, `admin_in`, `MAX_DEVICES`

Files: `root/front-sessions.json` (sha256 of tokens only), `root/device-locks.json`; on first start an older hub's
`hub-state.json` sessions and each account's `state.json` lock are taken over.

## Use it in another project

```rust
use pgm_identity_internal::{Auth, Login};
let auth = Auth::new(std::path::Path::new("/srv/my-app/accounts"), "admin", true, "[my-app]");
match auth.login(&req, None).await {
    Login::SignedIn { token, user, .. } => { /* give the page its token */ }
    Login::Answer(status, body) => { /* 401 / 403 / 429 … with body */ }
}
```
