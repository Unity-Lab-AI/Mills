//! **pgm-dist-internal** = PGM_Dist_Internal (PsyGraphMediaDistributionInternal): shipping programs.
//! - [`releases`]: pre-built programs in `dist/<target>/`, described in manifests signed with Ed25519 (and, for a
//!   host, HMAC with its own secret), their files handed out
//! - [`update`]: a program updating itself: verify a signed manifest, swap its own binary in (the old one kept), a
//!   self-test of the new one, roll back
//! - [`zip`]: a small zip writer for download packages
//! - [`version`]: what a checkout runs (version + commit) and a build name read once at start

pub mod releases;
pub mod update;
pub mod version;
pub mod zip;
