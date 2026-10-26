# pgm-net-internal: PGM_Net_Internal (PsyGraphMediaNetworkInternal)

Low-latency networking and syncing.

- `http`: the HTTP server kit. `Req` (+ `header`, `json`, `param`), `read` (whole body with a cap, else 413), `full`,
  `json` answers, `url_params`, `harden` (nosniff, no referrer, no framing, who answered), `add_cors` / `preflight` /
  `origin_allowed`, `Backend` (a reverse proxy to a program behind this one, with a shared secret header, streamed
  back, waiting up to 30 s through its restarts; `internal` calls), `shutdown_signal`
- `client`: `http_client` (rustls + ring, HTTP/2, user agent), `no_redirects`, `ApiClient` (JSON API with a bearer
  token; the answer's own error message on failure; raw bytes both ways)
- `hostlink`: a program connected OUT to its hub (never opens a port): `HubLink` (`hello` with a nonce → HKDF
  session key checked by key id, `poll` with heartbeat / re-hello, `reply`, `stream`, sealed `events`), `HostKey`,
  `Envelope`, the job and hello types
- `machine::device`: a linked machine's own ed25519 key: `DeviceKey` (generate, `sign_reply`), `verify_reply` (the hub's
  check), `LinkStore` (owner-only `device-key` and `token` files, `wipe`, `handle_answer`: a 401 "revoked" wipes both)
- `machine::exec` / `jobs` / `transfer` / `runner` (T-311): what a linked machine does. `Machine::handle` is the one door
  every hub request passes (kill switch, the hub's signature and the owner, the scope, rate limits, the tool, an audit
  line); the tools are `list` / `read` / `write` / `delete` / `mkdir` (the AI host's `pc_job` results, through the scope's
  canonicaliser), `machine_info`, `shell_run` and `job_start` / `job_read` / `job_stop` (cwd confined, advisory, never root,
  jobs kept on the machine), `proc_list` / `proc_kill` (own children only by default), `re_*` (a `Containers` the app plugs in;
  without one they say there is no container engine) and `transfer_get` / `transfer_put` (big files by hub ticket, chunked,
  resumable, sha256-checked). `runner::link` (device code) and `Runner::run` (poll with backoff, signed replies, wipe on
  "revoked") are what vr-host, vr-desktop and jcodepgm reuse; `vr-host machine link|run|stop|start|status` is the first user.
- `machine::service` (T-313): ONE template (`Unit`) for the persistent mode: a systemd USER unit (Linux: runs as the person, never root,
  `Restart=on-failure`, no restart after exit 3 "unlinked" or 4 "refused to start") and a per-user Scheduled Task at sign-in (Windows: no
  administrator, least privilege, restart on failure), with `install.sh` / `uninstall.sh` and `install.cmd` / `uninstall.cmd` twins;
  `check_state_dir`, run by `Runner::open` and `runner::link`, refuses to start when the state folder (token, device key, hub key, link
  record, scope) is group- or world-readable (Linux) or has a Users / Everyone ACL entry (Windows, the `icacls` helper of
  `pgm_core_internal::fs`). Users: `vr-host machine install | uninstall`, vr-desktop's Tools window (it installs itself, headless:
  `<app> machine run --state <dir>`) and `jcodepgm machine install`.
- `poll`: `wait_for(notify, timeout, look)`: a long poll that never misses a wake-up

## Running the linked-machines tests on Windows (R339)

R321 to R333 and R340 are the unit tests under `machine::` (scope, paths, floor, device, sign, audit, kill, limits, elevate, changes,
exec, service). They run on Linux in the suite (`cargo test -p pgm-net-internal machine::`: 90 tests). The Windows path rules
(drive letters, `\\?\`, UNC, alternate streams, reserved names, case) are pure logic and run on every host; the tests that need a real
file system with links or Unix permissions are `#[cfg(unix)]`, and Windows has its own (`service::tests::a_broad_acl_on_the_state_folder_is_refused`
uses `icacls`). On a Windows PC with the Rust toolchain (rustup, MSVC build tools) and this repository checked out:

```
cd native
cargo test -p pgm-net-internal machine::
cargo test -p pgm-net-internal machine::paths:: machine::service:: -- --nocapture
cargo test -p pgm-ai-internal host::
cargo build -p vr-host
target\debug\vr-host.exe machine install --files-only --out %TEMP%\vr-machine-service
```

Expected: every test passes (the `cfg(unix)` ones are simply not compiled), and the last command writes `install.cmd`, `uninstall.cmd`,
`install.sh`, `uninstall.sh`, `vibe-roller-machine.service` and `vibe-roller-machine.task.xml` to that folder without touching the task
scheduler. Then, as the normal user (not elevated), `target\debug\vr-host.exe machine install` registers the task "VibeRoller linked
machine"; `schtasks /Query /TN "VibeRoller linked machine" /XML` must show `LeastPrivilege` and a `LogonTrigger`, and
`vr-host machine uninstall` removes it. Nothing here was run on Windows when it was written (no Windows toolchain on the build host):
the first Windows run is the check. Report any failure with the test name.

## Use it in another project

```rust
use pgm_net_internal::{http, poll};
// in a hyper service: read the request, answer JSON, harden it
let (req, _parts) = match http::read(incoming, peer, 1 << 20).await { Ok(x) => x, Err(too_big) => return too_big };
let resp = http::json(200, &serde_json::json!({ "you": req.peer.to_string() }));
let resp = http::harden(resp, ("x-my-server", "1.0"));
```
