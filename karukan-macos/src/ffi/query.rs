//! State-query functions called by Swift after each key event.
//!
//! Returned pointers are valid until the next `karukan_push_*` call.
//! Swift callers **must** copy the string immediately (e.g. `String(cString:)`).

#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::{c_char, c_int};
use std::panic::AssertUnwindSafe;

use super::{KarukanSession, ffi_mut, ffi_ref};

// ---------------------------------------------------------------------------
// Preedit
// ---------------------------------------------------------------------------

/// Returns a pointer to the current preedit text (null-terminated UTF-8).
///
/// The pointer is valid until the next `karukan_push_*` call.
/// Returns `NULL` if `session` is `NULL`.
#[unsafe(no_mangle)]
pub extern "C" fn karukan_get_preedit(session: *const KarukanSession) -> *const c_char {
    std::panic::catch_unwind(|| {
        let s = ffi_ref!(session, std::ptr::null());
        s.preedit.text.as_ptr()
    })
    .unwrap_or(std::ptr::null())
}

/// Returns the byte length of the preedit text (excluding the null terminator).
/// Returns `0` if `session` is `NULL`.
#[unsafe(no_mangle)]
pub extern "C" fn karukan_get_preedit_len(session: *const KarukanSession) -> u32 {
    std::panic::catch_unwind(|| {
        let s = ffi_ref!(session, 0);
        s.preedit.text.as_bytes().len() as u32
    })
    .unwrap_or(0)
}

/// Returns the preedit caret position as a byte offset within the preedit string.
///
/// Used by `IMKInputController` to position the insertion-point underline.
/// Returns `0` if `session` is `NULL`.
#[unsafe(no_mangle)]
pub extern "C" fn karukan_get_preedit_caret(session: *const KarukanSession) -> u32 {
    std::panic::catch_unwind(|| ffi_ref!(session, 0).preedit.caret_bytes).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Commit
// ---------------------------------------------------------------------------

/// Returns `1` if there is a pending commit text, `0` otherwise.
/// Returns `0` if `session` is `NULL`.
#[unsafe(no_mangle)]
pub extern "C" fn karukan_has_commit(session: *const KarukanSession) -> c_int {
    std::panic::catch_unwind(|| {
        if ffi_ref!(session, 0).commit.dirty {
            1
        } else {
            0
        }
    })
    .unwrap_or(0)
}

/// Returns a pointer to the pending commit text (null-terminated UTF-8).
///
/// Call [`karukan_has_commit`] first; if `0`, the returned string is empty.
/// The pointer is valid until the next `karukan_push_*` call.
/// Returns `NULL` if `session` is `NULL`.
#[unsafe(no_mangle)]
pub extern "C" fn karukan_get_commit(session: *const KarukanSession) -> *const c_char {
    std::panic::catch_unwind(|| {
        let s = ffi_ref!(session, std::ptr::null());
        s.commit.text.as_ptr()
    })
    .unwrap_or(std::ptr::null())
}

// ---------------------------------------------------------------------------
// Session state
// ---------------------------------------------------------------------------

/// Returns `1` if the session has no pending input, `0` if composing.
///
/// Use this to decide whether to pass keys through to the application
/// (e.g. arrow keys and function keys when the session is empty).
/// Returns `1` (empty) if `session` is `NULL`.
#[unsafe(no_mangle)]
pub extern "C" fn karukan_is_empty(session: *const KarukanSession) -> c_int {
    std::panic::catch_unwind(|| {
        if ffi_ref!(session, 1).is_empty() {
            1
        } else {
            0
        }
    })
    .unwrap_or(1) // default to "empty" on error — safer than claiming composing
}

// ---------------------------------------------------------------------------
// Candidates
// ---------------------------------------------------------------------------

/// Returns the number of conversion candidates available.
///
/// Returns `0` if `session` is `NULL` or there is no active conversion.
#[unsafe(no_mangle)]
pub extern "C" fn karukan_get_candidate_count(session: *const KarukanSession) -> u32 {
    std::panic::catch_unwind(|| {
        ffi_ref!(session, 0).candidate_cache.items.len() as u32
    })
    .unwrap_or(0)
}

/// Returns a pointer to the null-terminated UTF-8 text of the `index`-th candidate.
///
/// The pointer is valid until the next `karukan_push_*` or
/// `karukan_select_candidate` call on the same session.
/// Returns `NULL` if `session` is `NULL` or `index` is out of range.
#[unsafe(no_mangle)]
pub extern "C" fn karukan_get_candidate(
    session: *const KarukanSession,
    index: u32,
) -> *const c_char {
    std::panic::catch_unwind(|| {
        let s = ffi_ref!(session, std::ptr::null());
        s.candidate_cache
            .items
            .get(index as usize)
            .map(|c| c.as_ptr())
            .unwrap_or(std::ptr::null())
    })
    .unwrap_or(std::ptr::null())
}

/// Returns the index of the currently selected candidate.
///
/// Returns `0` if `session` is `NULL`.
#[unsafe(no_mangle)]
pub extern "C" fn karukan_get_candidate_cursor(session: *const KarukanSession) -> u32 {
    std::panic::catch_unwind(|| ffi_ref!(session, 0).candidate_cache.cursor).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/// Persist the learning cache to disk if it has unsaved changes.
///
/// Call this when the input context deactivates (focus change / IME switch)
/// so that recent selections are not lost.
/// No-op if `session` is `NULL` or the cache has no unsaved changes.
#[unsafe(no_mangle)]
pub extern "C" fn karukan_save_learning(session: *mut KarukanSession) {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        ffi_mut!(session).save_learning();
    }))
    .ok();
}
