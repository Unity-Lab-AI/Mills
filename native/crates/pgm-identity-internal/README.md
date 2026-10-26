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

## Linked machines (T-309, plans/linked-machines.md)

`devices::Devices` also keeps the person's linked machines: a machine is a device token with `kind: "machine"` in the
same `root/device-tokens.json` (sha256 of the token only; the new top-level `revoked` object holds sha256 of unlinked
machine tokens, so the machine is told "revoked" and not just "unknown"; an older reader ignores it).

- Link: `start_ex("vr-machine", device, ttl, ip, &info)` (`info`: `publicKey` (ed25519, 64 hex, needed), `machineId`, `os`,
  `app`, `appVersion`, `mode`, `folder`) answers `kind` and the `defaultScope` (one folder, read-write); `peek(user, code)`
  is what the approval screen shows (facts, key fingerprint, default scope, and the person's own records it could
  replace); `decide_ex(user, code, approve, replace)`; the next `poll` gives the token, bound to the approving account.
- Machines: `machines_of(user)` (the owner's cards, never a token or key), `machine_counts()` (admin: how many and how
  many online per person, nothing else), `machine_update` (rename / pause), `machine_seen(token, held)` (poll: `Linked`
  / `Revoked` / `Unknown`, online while polling), `machine_report` (scope in words, mode), `verify_machine_reply`
  (the reply must be signed by the key recorded at approval: `pgm_net_internal::machine::device`).
- Revoke: `revoke(user, id)` (Unlink), `revoke_all_ex(user, keep_machines)` (a password change or sign-out everywhere
  revokes machines too unless "keep my machines linked"); both tombstone the token and run the unlink hook
  (`set_unlink_hook`: where a machine's poll and in-flight jobs end).
- Re-link after a wiped store: the same `machineId` (or device name) is offered in `replaceable`; `replace: <id>` moves the
  name and history to the new record and revokes the old token. The new link always starts at the default scope.

### One machine, two hubs (VibeRoller and Hyper Prod Man)

Hyper Prod Man is its own program with its own data and its own accounts (made from a VibeRoller login at the first
sign-in through PGM Gateway, then independent: own password hash, sessions, device lock, so a password change in one
does not reach the other). A machine record is therefore **per hub and per account**: to use one computer from both,
link it twice, once against `https://<vibe-roller hub>` and once against `https://<portal>/p/hyperprodman`, each with
its own device code, its own approval by that hub's account, its own token and record. The machine keeps one entry per
hub in its state dir (hub address, token); the device key may be the same file, or one key per hub (better: a key
registered with one hub is no use against the other). Revoking at one hub does not touch the other. HPM's side (its
routes on pgm-product-internal) is not built yet.

## Use it in another project

```rust
use pgm_identity_internal::{Auth, Login};
let auth = Auth::new(std::path::Path::new("/srv/my-app/accounts"), "admin", true, "[my-app]");
match auth.login(&req, None).await {
    Login::SignedIn { token, user, .. } => { /* give the page its token */ }
    Login::Answer(status, body) => { /* 401 / 403 / 429 … with body */ }
}
```
