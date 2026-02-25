//! Session lifecycle: `karukan_session_new`, `karukan_session_init`, `karukan_session_free`.

use std::ffi::c_int;
use std::panic::AssertUnwindSafe;

use super::{KarukanSession, ffi_mut, init_logging};

/// Allocate a new `KarukanSession` and return an owning raw pointer.
///
/// This function is **lightweight** (no I/O, no model loading) and is safe to
/// call on the main thread.  Call [`karukan_session_init`] from a background
/// thread to load resources.
///
/// Returns `NULL` on allocation failure or if a panic occurs.
#[unsafe(no_mangle)]
pub extern "C" fn karukan_session_new() -> *mut KarukanSession {
    std::panic::catch_unwind(|| {
        init_logging();
        Box::into_raw(Box::new(KarukanSession::new()))
    })
    .unwrap_or(std::ptr::null_mut())
}

/// Load resources for the session (system dictionary, learning cache).
///
/// Phase 1 loads only the system dictionary and learning cache.
/// Phase 3 will add model loading (which may take several seconds on first
/// run while downloading from HuggingFace).
///
/// **Call from a background thread** so that the main thread is not blocked.
/// (In Phase 1 this is instant if the files do not exist, but the calling
/// convention is established here so Swift does not need to change in Phase 3.)
///
/// Returns `0` on success, `-1` on error.
#[unsafe(no_mangle)]
pub extern "C" fn karukan_session_init(session: *mut KarukanSession) -> c_int {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        let session = ffi_mut!(session, -1);
        session.init_resources();
        0
    }))
    .unwrap_or(-1)
}

/// Free a `KarukanSession` previously returned by [`karukan_session_new`].
///
/// Saves the learning cache before dropping the session.
/// Passing `NULL` is a no-op.
#[unsafe(no_mangle)]
pub extern "C" fn karukan_session_free(session: *mut KarukanSession) {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        if session.is_null() {
            return;
        }
        // SAFETY: `session` was created by `karukan_session_new` via
        // `Box::into_raw`.  We reconstruct the `Box` to regain ownership and
        // drop it properly.
        let mut s = unsafe { Box::from_raw(session) };
        s.save_learning();
        // `s` is dropped here, freeing the allocation.
    }))
    .ok(); // If a panic occurs, the session is already owned by `Box::from_raw`
    // and will be dropped when the box goes out of scope — no leak.
}
