//! Signed updates of the native programs, and swapping a running program's own binary: pgm-dist-internal
//! (PGM_Dist_Internal) since 5.0.0. The release public key is still built in from VR_RELEASE_PUBKEY (native/build.sh).
pub use pgm_dist_internal::update::*;
