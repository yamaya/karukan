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

/// Select the candidate at `index` and commit it immediately.
///
/// Used by `candidateSelected(_:)` in Swift when the user clicks a candidate
/// in the `IMKCandidates` panel.
///
/// Returns `1` on success, `0` if not in Conversion state or index is out of range.
/// Returns `0` if `session` is `NULL`.
#[unsafe(no_mangle)]
pub extern "C" fn karukan_select_candidate(
    session: *mut KarukanSession,
    index: u32,
) -> c_int {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        if ffi_mut!(session, 0).select_candidate(index as usize) { 1 } else { 0 }
    }))
    .unwrap_or(0)
}

/// 長文コミット後の残余ひらがなを Composing 状態として注入する。
///
/// `karukan_push_key(KARUKAN_KEY_RETURN)` でコミットした直後に呼び、
/// 文節分割で切り取った後半のひらがなを次の Composing 入力として引き継ぐ。
/// メインスレッドからのみ呼ぶこと。
///
/// 戻り値: 1=成功, 0=hiragana_utf8 が NULL または空文字列
#[unsafe(no_mangle)]
pub extern "C" fn karukan_set_composing_hiragana(
    session: *mut KarukanSession,
    hiragana_utf8: *const c_char,
) -> c_int {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        let s = ffi_mut!(session, 0);
        if hiragana_utf8.is_null() {
            return 0;
        }
        let hiragana = match unsafe { std::ffi::CStr::from_ptr(hiragana_utf8) }.to_str() {
            Ok(h) => h,
            Err(_) => return 0,
        };
        s.set_composing_hiragana(hiragana);
        1
    }))
    .unwrap_or(0)
}

/// バックグラウンド推論の結果を session に適用する。
///
/// Composing 状態でなければ無視する（stale な結果が Conversion 中に届いた場合など）。
/// preedit を変換済みテキストに更新し、dirty フラグを立てる。
///
/// メインスレッドからのみ呼ぶこと。
///
/// 戻り値: 1=適用成功（preedit dirty）、0=Composing 状態でなく無視した
#[unsafe(no_mangle)]
pub extern "C" fn karukan_apply_live_candidate(
    session: *mut KarukanSession,
    candidate_utf8: *const c_char,
) -> c_int {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        let s = ffi_mut!(session, 0);
        if candidate_utf8.is_null() {
            return 0;
        }
        let candidate = match unsafe { std::ffi::CStr::from_ptr(candidate_utf8) }.to_str() {
            Ok(s) => s,
            Err(_) => return 0,
        };
        s.apply_live_candidate(candidate);
        if s.preedit.dirty { 1 } else { 0 }
    }))
    .unwrap_or(0)
}
