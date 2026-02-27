//! State-query functions called by Swift after each key event.
//!
//! Returned pointers are valid until the next `karukan_push_*` call.
//! Swift callers **must** copy the string immediately (e.g. `String(cString:)`).

#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::{c_char, c_int};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

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

/// Returns `1` if the romaji converter has an unconverted consonant pending
/// (e.g. "k", "sh", "ch"), `0` otherwise.
///
/// Swift uses this to decide whether to delay the preedit update so that
/// the bare consonant does not flicker before being resolved to kana.
/// Returns `0` if `session` is `NULL`.
#[unsafe(no_mangle)]
pub extern "C" fn karukan_is_consonant_pending(session: *const KarukanSession) -> c_int {
    std::panic::catch_unwind(|| {
        if ffi_ref!(session, 0).is_consonant_pending() {
            1
        } else {
            0
        }
    })
    .unwrap_or(0)
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
// Live conversion
// ---------------------------------------------------------------------------

/// Composing 状態のひらがなを `buf` にコピーする。
///
/// バックグラウンドスレッドで推論を起動する直前に、メインスレッドから呼ぶこと。
/// Composing 状態でなければ 0 を返す（コピーなし）。
///
/// 戻り値: コピーした文字数（null 終端を除くバイト数）。null ポインタ時は 0。
#[unsafe(no_mangle)]
pub extern "C" fn karukan_get_composing_hiragana(
    session: *const KarukanSession,
    buf: *mut c_char,
    buf_len: usize,
) -> c_int {
    std::panic::catch_unwind(|| {
        let s = ffi_ref!(session, 0);
        let Some(text) = s.composing_hiragana() else {
            return 0;
        };
        let bytes = text.as_bytes();
        let copy_len = bytes.len().min(buf_len.saturating_sub(1));
        if copy_len == 0 || buf.is_null() {
            return 0;
        }
        // SAFETY: buf is non-null (checked above), buf_len >= copy_len + 1.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, copy_len);
            *buf.add(copy_len) = 0;
        }
        copy_len as c_int
    })
    .unwrap_or(0)
}

/// ひらがなを変換して上位1候補をヒープ確保した文字列で返す。
///
/// `Arc<KanaKanjiConverter>` のみ使用するためバックグラウンドスレッドから呼べる。
/// 戻り値は `karukan_free_string` で解放すること。
/// モデル未ロード時やエラー時は null を返す。
///
/// # Safety
/// session の `converter` フィールド（Arc）は Send+Sync であり読み取り専用アクセスのみ行う。
/// 呼び出し中に session が解放・変更されないことを呼び出し側が保証すること。
#[unsafe(no_mangle)]
pub extern "C" fn karukan_convert_top1(
    session: *const KarukanSession,
    hiragana_utf8: *const c_char,
) -> *mut c_char {
    std::panic::catch_unwind(|| {
        let s = ffi_ref!(session, std::ptr::null_mut());
        let Some(conv) = s.converter.as_ref() else {
            return std::ptr::null_mut();
        };
        let conv = Arc::clone(conv);
        if hiragana_utf8.is_null() {
            return std::ptr::null_mut();
        }
        let hiragana = match unsafe { std::ffi::CStr::from_ptr(hiragana_utf8) }.to_str() {
            Ok(s) => s,
            Err(_) => return std::ptr::null_mut(),
        };
        match conv.convert(hiragana, "", 1) {
            Ok(candidates) if !candidates.is_empty() => {
                std::ffi::CString::new(candidates[0].as_str())
                    .map(|cs| cs.into_raw())
                    .unwrap_or(std::ptr::null_mut())
            }
            _ => std::ptr::null_mut(),
        }
    })
    .unwrap_or(std::ptr::null_mut())
}

/// `karukan_convert_top1` が返したポインタを解放する。
///
/// null ポインタを渡すと no-op。
#[unsafe(no_mangle)]
pub extern "C" fn karukan_free_string(ptr: *mut c_char) {
    if !ptr.is_null() {
        // SAFETY: ptr は karukan_convert_top1 が CString::into_raw() で生成したもの。
        unsafe { drop(std::ffi::CString::from_raw(ptr)) };
    }
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
