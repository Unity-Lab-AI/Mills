//! Starting other programs the same way on Windows and Linux (found on the PATH, PATHEXT on Windows, never with a
//! window of their own): pgm-platform-internal (PGM_Platform_Internal) since 5.0.0.
pub use pgm_platform_internal::proc::run;
