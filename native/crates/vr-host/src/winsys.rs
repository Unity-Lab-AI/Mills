//! The few Windows calls the host needs that the standard library has no word for: pgm-platform-internal
//! (PGM_Platform_Internal) since 5.0.0.
#![cfg(windows)]
pub use pgm_platform_internal::proc::CREATE_NO_WINDOW;
