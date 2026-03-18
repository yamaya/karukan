//! Session lifecycle: `karukan_session_new`, `karukan_session_init`, `karukan_session_free`,
//! `karukan_prewarm`, `karukan_is_prewarmed`.

use std::ffi::c_int;
use std::panic::AssertUnwindSafe;

use super::{KarukanSession, ffi_mut, init_logging};
use crate::session::SHARED_CONVERTER;

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
/// Loads the system dictionary, user dictionaries, and learning cache.
/// Missing or unreadable resources are silently skipped (logged as warnings)
/// — the session remains usable without them.
///
/// **Call from a background thread** so that the main thread is not blocked.
///
/// Returns `0` on success (including when some resources are missing),
/// `-1` only on panic (null pointer, etc.).
#[unsafe(no_mangle)]
pub extern "C" fn karukan_session_init(session: *mut KarukanSession) -> c_int {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        let session = ffi_mut!(session, -1);
        session.init_resources();
        0
    }))
    .unwrap_or(-1)
}

/// Pre-load the shared `KanaKanjiConverter` (model + backend).
///
/// Call from a background thread at application launch.  If the model is
/// already loaded this is a cheap no-op.  Once complete,
/// [`karukan_session_init`] becomes fast because the model is shared via
/// `Arc` and only dictionary / learning-cache I/O remains.
///
/// Returns `0` on success, `-1` on error (e.g. model download failed).
#[unsafe(no_mangle)]
pub extern "C" fn karukan_prewarm() -> c_int {
    std::panic::catch_unwind(|| {
        init_logging();

        // If already loaded, nothing to do.
        if SHARED_CONVERTER.get().is_some() {
            return 0;
        }

        use karukan_engine::kanji::model_config::registry;
        use karukan_engine::{Backend, KanaKanjiConverter};
        use std::sync::Arc;

        let result = registry()
            .default_variant()
            .ok_or_else(|| "no default variant in models.toml".to_string())
            .and_then(|(family, variant)| {
                Backend::from_variant(family, variant).map_err(|e| e.to_string())
            })
            .and_then(|backend| KanaKanjiConverter::new(backend).map_err(|e| e.to_string()))
            .map(Arc::new);

        match result {
            Ok(arc) => {
                let _ = SHARED_CONVERTER.set(arc);
                tracing::info!("karukan_prewarm: KanaKanjiConverter loaded");
                0
            }
            Err(e) => {
                tracing::warn!("karukan_prewarm failed: {}", e);
                -1
            }
        }
    })
    .unwrap_or(-1)
}

/// Check whether the shared `KanaKanjiConverter` has been pre-loaded.
///
/// Returns `1` if [`karukan_prewarm`] (or a prior [`karukan_session_init`])
/// has successfully loaded the model, `0` otherwise.
///
/// This is a lock-free read and safe to call from any thread.
#[unsafe(no_mangle)]
pub extern "C" fn karukan_is_prewarmed() -> c_int {
    if SHARED_CONVERTER.get().is_some() { 1 } else { 0 }
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
