//! **pgm-net-internal** = PGM_Net_Internal (PsyGraphMediaNetworkInternal): low-latency networking and syncing.
//! - [`http`]: the HTTP server kit: a request read whole with a cap, JSON / streamed answers, query parameters, CORS,
//!   hardening headers, the origin rule, a reverse proxy to a backend that waits through its restarts, stop signals
//! - [`client`]: HTTP clients (rustls, HTTP/2; one that follows no redirects) and a JSON API client with a token
//! - [`hostlink`]: a program on another machine connected OUT to its hub: hello + HKDF / AES-GCM session key, long
//!   poll for jobs with the hub's heartbeat, whole / streamed / sealed answers, re-hello (the VibeRoller AI host
//!   protocol)
//! - [`machine`]: the linked-machines core: scope model + evaluator, path canonicaliser, safety floor
//! - [`poll`]: long polls that never miss a wake-up
//! - [`product`]: the product link: a product signing its requests to a PGM Gateway on the same machine (the
//!   gateway's verifier, the product's client)

pub mod client;
pub mod hostlink;
pub mod http;
pub mod machine;
pub mod poll;
pub mod product;
