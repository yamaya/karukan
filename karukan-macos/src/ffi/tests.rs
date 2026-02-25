//! FFI boundary tests for `karukan-macos`.
//!
//! These tests exercise the C API as Swift would, going through every
//! `extern "C"` function.  They use `KARUKAN_DATA_DIR` to redirect data files
//! to a temporary directory so that `~/Library/Application Support/` is never
//! touched during testing.

use std::ffi::{CStr, CString};
use std::ptr;
use std::sync::Mutex;

use super::KarukanSession;
use super::input::{karukan_push_char, karukan_push_key};
use super::lifecycle::{karukan_session_free, karukan_session_init, karukan_session_new};
use super::input::karukan_select_candidate;
use super::query::{
    karukan_get_candidate, karukan_get_candidate_count, karukan_get_candidate_cursor,
    karukan_get_commit, karukan_get_preedit, karukan_get_preedit_caret, karukan_get_preedit_len,
    karukan_has_commit, karukan_is_empty, karukan_save_learning,
};

// ---------------------------------------------------------------------------
// KARUKAN_DATA_DIR is a process-wide environment variable.
// Serialise tests that mutate it to prevent races.
// ---------------------------------------------------------------------------
static ENV_LOCK: Mutex<()> = Mutex::new(());

// KarukanKey constants (mirrors karukan_macos.h)
const KEY_RETURN: u32 = 1;
const KEY_BACKSPACE: u32 = 2;
const KEY_ESCAPE: u32 = 3;
const KEY_SPACE: u32 = 4;

// ---------------------------------------------------------------------------
// RAII test helper
// ---------------------------------------------------------------------------

/// RAII wrapper that creates a session in a temporary directory and frees it
/// on drop.  Mirrors `TestEngine` in `karukan-im/src/ffi/tests.rs`.
struct TestSession(*mut KarukanSession);

impl TestSession {
    fn new() -> Self {
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().expect("tempdir");

        // SAFETY: tests run serialised via ENV_LOCK; no concurrent env mutation.
        unsafe { std::env::set_var("KARUKAN_DATA_DIR", dir.path()) };

        let ptr = karukan_session_new();
        assert!(!ptr.is_null(), "karukan_session_new returned null");

        let rc = karukan_session_init(ptr);
        assert_eq!(rc, 0, "karukan_session_init failed");

        // Keep dir alive until after init so the path exists if init reads it.
        drop(dir);
        // Remove the override so other tests get the default behaviour.
        // SAFETY: same as above.
        unsafe { std::env::remove_var("KARUKAN_DATA_DIR") };

        Self(ptr)
    }

    fn ptr(&self) -> *mut KarukanSession {
        self.0
    }

    // --- input helpers ---

    fn push_char(&self, c: &str) -> bool {
        let cs = CString::new(c).unwrap();
        // In Rust 2024 edition, safe `extern "C"` fns are callable without unsafe.
        karukan_push_char(self.0, cs.as_ptr()) == 1
    }

    fn push_key(&self, key: u32) -> bool {
        karukan_push_key(self.0, key) == 1
    }

    // --- query helpers ---

    fn preedit(&self) -> &str {
        let ptr = karukan_get_preedit(self.0);
        if ptr.is_null() {
            return "";
        }
        // SAFETY: karukan_get_preedit guarantees a valid null-terminated UTF-8
        // string that outlives the current push operation.
        unsafe { CStr::from_ptr(ptr) }.to_str().unwrap_or("")
    }

    fn preedit_len(&self) -> u32 {
        karukan_get_preedit_len(self.0)
    }

    fn preedit_caret(&self) -> u32 {
        karukan_get_preedit_caret(self.0)
    }

    fn has_commit(&self) -> bool {
        karukan_has_commit(self.0) == 1
    }

    fn commit_text(&self) -> &str {
        let ptr = karukan_get_commit(self.0);
        if ptr.is_null() {
            return "";
        }
        // SAFETY: same as preedit().
        unsafe { CStr::from_ptr(ptr) }.to_str().unwrap_or("")
    }

    fn is_empty(&self) -> bool {
        karukan_is_empty(self.0) == 1
    }

    fn candidate_count(&self) -> u32 {
        karukan_get_candidate_count(self.0)
    }

    fn candidate_cursor(&self) -> u32 {
        karukan_get_candidate_cursor(self.0)
    }

    fn candidate_text(&self, index: u32) -> Option<&str> {
        let ptr = karukan_get_candidate(self.0, index);
        if ptr.is_null() {
            return None;
        }
        // SAFETY: pointer is valid until the next push_* call.
        Some(unsafe { std::ffi::CStr::from_ptr(ptr) }.to_str().unwrap_or(""))
    }

    fn select_candidate(&self, index: u32) -> bool {
        karukan_select_candidate(self.0, index) == 1
    }
}

impl Drop for TestSession {
    fn drop(&mut self) {
        karukan_session_free(self.0);
    }
}

// ---------------------------------------------------------------------------
// Lifecycle tests
// ---------------------------------------------------------------------------

#[test]
fn test_session_lifecycle() {
    let _s = TestSession::new();
    // Passes if new/init/free do not crash.
}

// ---------------------------------------------------------------------------
// Null-pointer safety tests
// ---------------------------------------------------------------------------

#[test]
fn test_null_safety() {
    // Every function must survive a null pointer without crashing.
    // In Rust 2024 edition, safe extern "C" fns don't require unsafe blocks.
    let cs = CString::new("a").unwrap();
    assert_eq!(karukan_push_char(ptr::null_mut(), cs.as_ptr()), 0);
    assert_eq!(karukan_push_char(ptr::null_mut(), ptr::null()), 0);
    assert_eq!(karukan_push_key(ptr::null_mut(), KEY_RETURN), 0);

    assert!(karukan_get_preedit(ptr::null()).is_null());
    assert_eq!(karukan_get_preedit_len(ptr::null()), 0);
    assert_eq!(karukan_get_preedit_caret(ptr::null()), 0);

    assert_eq!(karukan_has_commit(ptr::null()), 0);
    assert!(karukan_get_commit(ptr::null()).is_null());

    assert_eq!(karukan_is_empty(ptr::null()), 1); // null → "empty" default
    karukan_save_learning(ptr::null_mut()); // must not crash
    karukan_session_free(ptr::null_mut()); // must not crash
}

// ---------------------------------------------------------------------------
// Basic romaji input tests
// ---------------------------------------------------------------------------

#[test]
fn test_basic_romaji_a() {
    let s = TestSession::new();
    assert!(s.push_char("a"));
    assert_eq!(s.preedit(), "あ");
    assert!(!s.is_empty());
}

#[test]
fn test_romaji_ka() {
    let s = TestSession::new();
    assert!(s.push_char("k"));
    assert_eq!(s.preedit(), "k");

    assert!(s.push_char("a"));
    assert_eq!(s.preedit(), "か");
}

#[test]
fn test_romaji_full_sentence() {
    let s = TestSession::new();
    // "konnnichiha" → "こんにちは"
    // (ko→こ, nn→ん, ni→に, chi→ち, ha→は)
    for ch in "konnnichiha".chars() {
        s.push_char(&ch.to_string());
    }
    assert_eq!(s.preedit(), "こんにちは");
}

#[test]
fn test_sokuon() {
    let s = TestSession::new();
    for ch in "kka".chars() {
        s.push_char(&ch.to_string());
    }
    assert_eq!(s.preedit(), "っか");
}

#[test]
fn test_youon_kya() {
    let s = TestSession::new();
    for ch in "kya".chars() {
        s.push_char(&ch.to_string());
    }
    assert_eq!(s.preedit(), "きゃ");
}

#[test]
fn test_nn_conversion() {
    let s = TestSession::new();
    for ch in "nn".chars() {
        s.push_char(&ch.to_string());
    }
    assert_eq!(s.preedit(), "ん");
}

// ---------------------------------------------------------------------------
// Commit tests
// ---------------------------------------------------------------------------

#[test]
fn test_commit_on_return() {
    let s = TestSession::new();
    s.push_char("a");
    s.push_char("i");
    assert_eq!(s.preedit(), "あい");

    assert!(s.push_key(KEY_RETURN));
    assert!(s.has_commit());
    assert_eq!(s.commit_text(), "あい");
    assert!(s.is_empty());
    assert_eq!(s.preedit_len(), 0);
}

#[test]
fn test_commit_flushes_romaji_buffer() {
    // "k" stays in romaji buffer; Return should flush it as "k".
    let s = TestSession::new();
    s.push_char("k");
    assert_eq!(s.preedit(), "k");

    s.push_key(KEY_RETURN);
    assert!(s.has_commit());
    assert_eq!(s.commit_text(), "k");
}

// ---------------------------------------------------------------------------
// Cancel (Escape) tests
// ---------------------------------------------------------------------------

#[test]
fn test_escape_cancel() {
    let s = TestSession::new();
    s.push_char("a");
    s.push_char("i");

    assert!(s.push_key(KEY_ESCAPE));
    assert!(!s.has_commit());
    assert!(s.is_empty());
    assert_eq!(s.preedit_len(), 0);
}

#[test]
fn test_escape_when_empty_does_not_consume() {
    let s = TestSession::new();
    assert!(!s.push_key(KEY_ESCAPE));
}

// ---------------------------------------------------------------------------
// Backspace tests
// ---------------------------------------------------------------------------

#[test]
fn test_backspace_clears_romaji_buffer() {
    let s = TestSession::new();
    s.push_char("k");
    assert_eq!(s.preedit(), "k");

    s.push_key(KEY_BACKSPACE);
    assert_eq!(s.preedit_len(), 0);
    assert!(s.is_empty());
}

#[test]
fn test_backspace_removes_hiragana() {
    let s = TestSession::new();
    s.push_char("k");
    s.push_char("a"); // → "か"
    assert_eq!(s.preedit(), "か");

    s.push_key(KEY_BACKSPACE); // removes "か"
    assert_eq!(s.preedit_len(), 0);
    assert!(s.is_empty());
}

#[test]
fn test_backspace_partial() {
    let s = TestSession::new();
    s.push_char("a"); // あ
    s.push_char("i"); // い
    assert_eq!(s.preedit(), "あい");

    s.push_key(KEY_BACKSPACE); // removes い
    assert_eq!(s.preedit(), "あ");
    assert!(!s.is_empty());

    s.push_key(KEY_BACKSPACE); // removes あ
    assert_eq!(s.preedit_len(), 0);
    assert!(s.is_empty());
}

#[test]
fn test_backspace_when_empty_does_not_consume() {
    let s = TestSession::new();
    assert!(!s.push_key(KEY_BACKSPACE));
}

// ---------------------------------------------------------------------------
// State / is_empty tests
// ---------------------------------------------------------------------------

#[test]
fn test_is_empty_initially() {
    let s = TestSession::new();
    assert!(s.is_empty());
}

#[test]
fn test_is_not_empty_while_composing() {
    let s = TestSession::new();
    s.push_char("a");
    assert!(!s.is_empty());
}

#[test]
fn test_is_empty_after_commit() {
    let s = TestSession::new();
    s.push_char("a");
    s.push_key(KEY_RETURN);
    assert!(s.is_empty());
}

// ---------------------------------------------------------------------------
// Caret position tests
// ---------------------------------------------------------------------------

#[test]
fn test_caret_after_single_hiragana() {
    let s = TestSession::new();
    s.push_char("a"); // "あ" = 3 bytes in UTF-8
    assert_eq!(s.preedit_caret(), 3);
}

#[test]
fn test_caret_with_romaji_buffer() {
    let s = TestSession::new();
    s.push_char("a"); // "あ" (3 bytes) confirmed
    s.push_char("k"); // "k" (1 byte) in romaji buffer
    // preedit = "あk", caret = 3 + 1 = 4
    assert_eq!(s.preedit_caret(), 4);
}

// ---------------------------------------------------------------------------
// UTF-8 / string safety tests
// ---------------------------------------------------------------------------

#[test]
fn test_preedit_valid_utf8_and_null_terminated() {
    let s = TestSession::new();
    s.push_char("a");

    let ptr = karukan_get_preedit(s.ptr());
    assert!(!ptr.is_null());

    // SAFETY: karukan_get_preedit guarantees a valid null-terminated UTF-8 string.
    let cstr = unsafe { CStr::from_ptr(ptr) };
    let str_val = cstr.to_str().expect("preedit must be valid UTF-8");
    assert!(!str_val.is_empty());
}

#[test]
fn test_preedit_pointer_stable_before_next_push() {
    let s = TestSession::new();
    s.push_char("a");
    let ptr1 = karukan_get_preedit(s.ptr());
    // Second call without a new push — pointer should be stable (same address).
    let ptr2 = karukan_get_preedit(s.ptr());
    assert_eq!(ptr1, ptr2);
}

// ---------------------------------------------------------------------------
// Unknown key test
// ---------------------------------------------------------------------------

#[test]
fn test_unknown_key_does_not_consume() {
    let s = TestSession::new();
    s.push_char("a"); // enter composing state
    // Key value 255 is not defined in KarukanKey.
    assert!(!s.push_key(255));
}

// ---------------------------------------------------------------------------
// Space key test (Phase 3: triggers conversion)
// ---------------------------------------------------------------------------

#[test]
fn test_space_key_triggers_conversion() {
    let s = TestSession::new();
    s.push_char("a"); // composing "あ"
    // Space should be consumed (triggers conversion attempt).
    assert!(s.push_key(KEY_SPACE));
    // In Conversion state: preedit shows the first candidate (at minimum the hiragana itself).
    // The session must not be empty (candidate or preedit is set).
    assert!(!s.is_empty());
    // Candidate cache should have at least 1 entry (fallback = the hiragana reading).
    assert!(s.candidate_count() > 0);
}

// ---------------------------------------------------------------------------
// Learning cache persistence test
// ---------------------------------------------------------------------------

#[test]
fn test_save_learning_no_crash() {
    let _lock = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: serialised via ENV_LOCK.
    unsafe { std::env::set_var("KARUKAN_DATA_DIR", dir.path()) };

    let ptr = karukan_session_new();
    karukan_session_init(ptr);

    // save_learning should not crash even when the cache is empty.
    karukan_save_learning(ptr);
    karukan_session_free(ptr);

    // SAFETY: same as above.
    unsafe { std::env::remove_var("KARUKAN_DATA_DIR") };
}
