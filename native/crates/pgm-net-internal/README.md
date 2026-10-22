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
- `poll`: `wait_for(notify, timeout, look)`: a long poll that never misses a wake-up

## Use it in another project

```rust
use pgm_net_internal::{http, poll};
// in a hyper service: read the request, answer JSON, harden it
let (req, _parts) = match http::read(incoming, peer, 1 << 20).await { Ok(x) => x, Err(too_big) => return too_big };
let resp = http::json(200, &serde_json::json!({ "you": req.peer.to_string() }));
let resp = http::harden(resp, ("x-my-server", "1.0"));
```
