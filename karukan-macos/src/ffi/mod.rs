//! C FFI layer for `karukan-macos`.
//!
//! All `extern "C"` functions wrap their bodies in
//! `std::panic::catch_unwind(AssertUnwindSafe(...))` so that a Rust panic
//! cannot unwind through the FFI boundary (undefined behaviour in C/Swift).
//!
//! # Null-pointer safety
//! Every function accepts null pointers and returns a safe default instead of
//! crashing.  The `ffi_ref!` / `ffi_mut!` macros enforce this uniformly.

pub(crate) mod input;
pub(crate) mod lifecycle;
pub(crate) mod query;

#[cfg(test)]
mod tests;

// Re-export the opaque session type so FFI sub-modules can reference it.
pub(crate) use crate::session::KarukanSession;

// ---------------------------------------------------------------------------
// Null-check macros
// ---------------------------------------------------------------------------

/// Obtain a shared reference from a raw pointer, returning `$default` on null.
///
/// # Example
/// ```ignore
/// let session = ffi_ref!(ptr, std::ptr::null());
/// ```
macro_rules! ffi_ref {
    ($ptr:expr, $default:expr) => {{
        if $ptr.is_null() {
            return $default;
        }
        // SAFETY: caller guarantees the pointer is valid for the duration of
        // the function call.  Null is handled above.
        unsafe { &*$ptr }
    }};
}

/// Obtain a mutable reference from a raw pointer, returning `$default` on null.
/// The single-argument form returns `()` (for `-> ()` functions).
///
/// # Example
/// ```ignore
/// let session = ffi_mut!(ptr, 0_i32);
/// let session = ffi_mut!(ptr); // for void functions
/// ```
macro_rules! ffi_mut {
    ($ptr:expr) => {{
        if $ptr.is_null() {
            return;
        }
        // SAFETY: same as ffi_ref!
        unsafe { &mut *$ptr }
    }};
    ($ptr:expr, $default:expr) => {{
        if $ptr.is_null() {
            return $default;
        }
        // SAFETY: same as ffi_ref!
        unsafe { &mut *$ptr }
    }};
}

pub(crate) use ffi_mut;
pub(crate) use ffi_ref;

// ---------------------------------------------------------------------------
// One-time logging initialisation
// ---------------------------------------------------------------------------

use std::sync::Once;

static INIT_LOGGING: Once = Once::new();

/// Initialise the `tracing` subscriber exactly once per process.
///
/// Routes logs to OSLog (visible in Console.app and `log stream`) so that
/// Rust-side events (model loading, conversion, errors) appear alongside
/// Swift's OSLog output under subsystem `io.github.yamaya.karukan`.
///
/// Default level: `info` so that model download/load progress is visible.
/// Override with `RUST_LOG` (e.g. `RUST_LOG=debug`).
pub(crate) fn init_logging() {
    INIT_LOGGING.call_once(|| {
        use tracing_subscriber::prelude::*;
        let oslog_layer = tracing_oslog::OsLogger::new(
            "io.github.yamaya.karukan",
            "rust",
        );
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
        tracing_subscriber::registry()
            .with(filter)
            .with(oslog_layer)
            .init();
    });
}
