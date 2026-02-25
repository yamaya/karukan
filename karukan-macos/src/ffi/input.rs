//! Key and character input functions.
//!
//! These are the primary entry points called by Swift on every key event.

#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::{c_char, c_int};
use std::panic::AssertUnwindSafe;

use crate::session::KarukanKey;

use super::{KarukanSession, ffi_mut};

/// Push a single printable character into the IME.
///
/// `c` must be a null-terminated UTF-8 string containing exactly one Unicode
/// code point.  Only the first code point is used; control characters are
/// rejected.
///
/// Returns `1` if the IME consumed the character, `0` otherwise.
/// Returns `0` if `session` or `c` is `NULL`.
#[unsafe(no_mangle)]
pub extern "C" fn karukan_push_char(session: *mut KarukanSession, c: *const c_char) -> c_int {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        let session = ffi_mut!(session, 0);
        if c.is_null() {
            return 0;
        }

        // SAFETY: `c` is non-null (checked above) and is expected to be a
        // valid null-terminated C string from Swift / Objective-C.
        let s = unsafe {
            match std::ffi::CStr::from_ptr(c).to_str() {
                Ok(s) => s,
                Err(_) => return 0,
            }
        };

        // Accept only the first code point; reject control characters.
        let ch = match s.chars().next() {
            Some(ch) if !ch.is_control() => ch,
            _ => return 0,
        };

        if session.push_char(ch) { 1 } else { 0 }
    }))
    .unwrap_or(0)
}

/// Push a special key into the IME.
///
/// `key` must be one of the `KARUKAN_KEY_*` constants defined in
/// `karukan_macos.h`.  Unknown values are treated as "not consumed".
///
/// Returns `1` if the IME consumed the key, `0` otherwise.
/// Returns `0` if `session` is `NULL`.
#[unsafe(no_mangle)]
pub extern "C" fn karukan_push_key(session: *mut KarukanSession, key: u32) -> c_int {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        let session = ffi_mut!(session, 0);
        let key = match KarukanKey::from_u32(key) {
            Some(k) => k,
            None => return 0,
        };
        if session.push_key(key) { 1 } else { 0 }
    }))
    .unwrap_or(0)
}
