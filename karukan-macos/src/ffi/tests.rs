//! FFI boundary tests for `karukan-macos`.
//!
//! These tests exercise the C API as Swift would, going through every
//! `extern "C"` function.  They use `KARUKAN_DATA_DIR` to redirect data files
//! to a temporary directory so that `~/Library/Application Support/` is never
//! touched during testing.

use std::ffi::{CStr, CString, c_char};
use std::ptr;
use std::sync::Mutex;

use super::KarukanSession;
use super::input::{
    karukan_apply_live_candidate, karukan_push_char, karukan_push_key, karukan_select_candidate,
    karukan_set_composing_hiragana,
};
use super::lifecycle::{karukan_session_free, karukan_session_init, karukan_session_new};
use super::query::{
    karukan_get_candidate, karukan_get_candidate_count, karukan_get_candidate_cursor,
    karukan_get_commit, karukan_get_composing_hiragana, karukan_get_preedit,
    karukan_get_preedit_caret, karukan_get_preedit_len, karukan_has_commit,
    karukan_is_consonant_pending, karukan_is_empty, karukan_save_learning,
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
const KEY_UP: u32 = 7;
const KEY_DOWN: u32 = 8;
#[allow(dead_code)]
const KEY_TAB: u32 = 9;
const KEY_CONVERT_HIRAGANA: u32 = 10;
const KEY_CONVERT_KATAKANA: u32 = 11;
const KEY_CONVERT_ASCII: u32 = 12;

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
        Some(
            unsafe { std::ffi::CStr::from_ptr(ptr) }
                .to_str()
                .unwrap_or(""),
        )
    }

    fn select_candidate(&self, index: u32) -> bool {
        karukan_select_candidate(self.0, index) == 1
    }

    // --- live conversion helpers ---

    fn apply_live_candidate(&self, candidate: &str, source: &str) -> bool {
        let cand = CString::new(candidate).unwrap();
        let src = CString::new(source).unwrap();
        karukan_apply_live_candidate(self.0, cand.as_ptr(), src.as_ptr()) == 1
    }

    fn set_composing_hiragana(&self, hiragana: &str) -> bool {
        let cs = CString::new(hiragana).unwrap();
        karukan_set_composing_hiragana(self.0, cs.as_ptr()) == 1
    }

    /// Returns the composing hiragana text via the buffer API.
    /// Empty string when not in Composing state or input_buf is empty.
    fn composing_hiragana(&self) -> String {
        let mut buf = vec![0u8; 512];
        let len = karukan_get_composing_hiragana(self.0, buf.as_mut_ptr() as *mut c_char, 512);
        if len <= 0 {
            return String::new();
        }
        std::str::from_utf8(&buf[..len as usize])
            .unwrap_or("")
            .to_owned()
    }

    fn is_consonant_pending(&self) -> bool {
        karukan_is_consonant_pending(self.0) == 1
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
// Live conversion — apply_live_candidate
// ---------------------------------------------------------------------------

#[test]
fn test_apply_live_candidate_basic() {
    let s = TestSession::new();
    for ch in "nadesi".chars() {
        s.push_char(&ch.to_string());
    }
    // input_buf.text == "なでし"
    assert_eq!(s.preedit(), "なでし");

    // バックグラウンド推論が "撫子" を返してきた
    assert!(s.apply_live_candidate("撫子", "なでし"));
    // preedit が変換済みテキストに更新される
    assert_eq!(s.preedit(), "撫子");
    // Composing のまま（コミットなし）
    assert!(!s.has_commit());
    assert!(!s.is_empty());
}

#[test]
fn test_apply_live_candidate_stale_source_ignored() {
    let s = TestSession::new();
    for ch in "nadesi".chars() {
        s.push_char(&ch.to_string());
    }
    assert_eq!(s.preedit(), "なでし");

    // source が "なで" (stale) — input_buf.text は "なでし"
    // FFI の戻り値は preedit.dirty の残留状態に依存するため確認しない。
    // 重要なのは preedit テキストが変わっていないこと。
    s.apply_live_candidate("撫子", "なで");
    assert_eq!(s.preedit(), "なでし");
}

#[test]
fn test_apply_live_candidate_when_not_composing_ignored() {
    let s = TestSession::new();
    // Empty 状態 — 無視される
    assert!(!s.apply_live_candidate("撫子", ""));
    assert!(s.is_empty());
}

#[test]
fn test_apply_live_candidate_with_pending_romaji() {
    let s = TestSession::new();
    s.push_char("a"); // "あ" → input_buf
    s.push_char("k"); // romaji バッファに "k" 残存、input_buf は "あ" のまま
    assert_eq!(s.preedit(), "あk");

    // source = "あ" (input_buf.text) — stale ではない
    assert!(s.apply_live_candidate("亜", "あ"));
    // preedit = "亜" + "k" (live + romaji buffer)
    assert_eq!(s.preedit(), "亜k");
}

#[test]
fn test_live_candidate_cleared_by_backspace() {
    let s = TestSession::new();
    for ch in "aiu".chars() {
        s.push_char(&ch.to_string());
    }
    s.apply_live_candidate("愛憂", "あいう");
    assert_eq!(s.preedit(), "愛憂");

    // Backspace → live_candidate クリア、ひらがな表示に戻る
    s.push_key(KEY_BACKSPACE);
    // "あいう" の末尾 "う" が削除されて "あい"
    assert_eq!(s.preedit(), "あい");
    assert!(!s.is_empty());
}

#[test]
fn test_live_candidate_commit_matching_source() {
    let s = TestSession::new();
    for ch in "aiu".chars() {
        s.push_char(&ch.to_string());
    }
    s.apply_live_candidate("愛憂", "あいう");
    assert_eq!(s.preedit(), "愛憂");

    // Return → live_candidate をコミット
    s.push_key(KEY_RETURN);
    assert!(s.has_commit());
    assert_eq!(s.commit_text(), "愛憂");
    assert!(s.is_empty());
}

#[test]
fn test_live_candidate_stale_commit_falls_back_to_hiragana() {
    let s = TestSession::new();
    for ch in "aiu".chars() {
        s.push_char(&ch.to_string());
    }
    assert_eq!(s.preedit(), "あいう");

    // source = "あい" だが input_buf は "あいう" — stale なので preedit は変わらない
    // (FFI の戻り値は preedit.dirty 残留状態に依存するため確認しない)
    s.apply_live_candidate("愛憂", "あい");
    assert_eq!(s.preedit(), "あいう"); // preedit が更新されていない

    // Return → live_candidate は stale なのでひらがなをコミット
    s.push_key(KEY_RETURN);
    assert!(s.has_commit());
    assert_eq!(s.commit_text(), "あいう");
}

#[test]
fn test_escape_two_step_with_live_candidate() {
    let s = TestSession::new();
    for ch in "aiu".chars() {
        s.push_char(&ch.to_string());
    }
    s.apply_live_candidate("愛憂", "あいう");
    assert_eq!(s.preedit(), "愛憂");

    // 1 回目 Escape: live_candidate をクリアしてひらがな表示に戻る
    assert!(s.push_key(KEY_ESCAPE));
    assert!(!s.is_empty()); // まだ Composing
    assert_eq!(s.preedit(), "あいう"); // ひらがな復元

    // 2 回目 Escape: 全キャンセル
    assert!(s.push_key(KEY_ESCAPE));
    assert!(s.is_empty());
    assert!(!s.has_commit());
}

// ---------------------------------------------------------------------------
// set_composing_hiragana
// ---------------------------------------------------------------------------

#[test]
fn test_set_composing_hiragana_from_empty() {
    let s = TestSession::new();
    assert!(s.is_empty());
    assert!(s.set_composing_hiragana("かきく"));
    assert!(!s.is_empty());
    assert_eq!(s.preedit(), "かきく");
}

#[test]
fn test_set_composing_hiragana_overwrites_existing() {
    let s = TestSession::new();
    s.push_char("a"); // "あ"
    assert_eq!(s.preedit(), "あ");

    s.set_composing_hiragana("なでしこ");
    assert_eq!(s.preedit(), "なでしこ");
}

#[test]
fn test_set_composing_hiragana_empty_string_is_noop() {
    let s = TestSession::new();
    // 空文字列は no-op — セッションは Empty のまま
    s.set_composing_hiragana("");
    assert!(s.is_empty());
    assert_eq!(s.preedit_len(), 0);
}

#[test]
fn test_set_composing_hiragana_clears_romaji_buffer() {
    let s = TestSession::new();
    s.push_char("k"); // romaji バッファに "k" が残る
    assert!(s.is_consonant_pending());

    // set_composing_hiragana はローマ字バッファをリセットする
    s.set_composing_hiragana("さくら");
    assert_eq!(s.preedit(), "さくら");
    assert!(!s.is_consonant_pending()); // romaji バッファがクリアされた
}

// ---------------------------------------------------------------------------
// karukan_get_composing_hiragana
// ---------------------------------------------------------------------------

#[test]
fn test_get_composing_hiragana_returns_text() {
    let s = TestSession::new();
    for ch in "aiu".chars() {
        s.push_char(&ch.to_string());
    }
    assert_eq!(s.composing_hiragana(), "あいう");
}

#[test]
fn test_get_composing_hiragana_empty_when_not_composing() {
    let s = TestSession::new();
    assert_eq!(s.composing_hiragana(), ""); // Empty 状態
}

#[test]
fn test_get_composing_hiragana_empty_after_commit() {
    let s = TestSession::new();
    s.push_char("a");
    s.push_key(KEY_RETURN);
    assert!(s.is_empty());
    assert_eq!(s.composing_hiragana(), "");
}

// ---------------------------------------------------------------------------
// karukan_is_consonant_pending
// ---------------------------------------------------------------------------

#[test]
fn test_is_consonant_pending_true_for_pending_consonant() {
    let s = TestSession::new();
    s.push_char("k"); // "k" が romaji バッファに残る
    assert!(s.is_consonant_pending());
}

#[test]
fn test_is_consonant_pending_false_after_complete_kana() {
    let s = TestSession::new();
    s.push_char("k");
    s.push_char("a"); // "か" — バッファ消費
    assert!(!s.is_consonant_pending());
}

#[test]
fn test_is_consonant_pending_false_when_empty() {
    let s = TestSession::new();
    assert!(!s.is_consonant_pending());
}

// ---------------------------------------------------------------------------
// Conversion 状態 — 追加 FFI テスト
// ---------------------------------------------------------------------------

#[test]
fn test_conversion_return_commits_ffi() {
    let s = TestSession::new();
    s.push_char("a");
    s.push_key(KEY_SPACE); // → Conversion

    assert!(!s.is_empty());
    assert!(s.candidate_count() > 0);

    assert!(s.push_key(KEY_RETURN));
    assert!(s.has_commit());
    assert!(!s.commit_text().is_empty());
    assert!(s.is_empty());
}

#[test]
fn test_conversion_escape_returns_to_composing_ffi() {
    let s = TestSession::new();
    s.push_char("a");
    s.push_key(KEY_SPACE); // → Conversion

    assert!(s.push_key(KEY_ESCAPE));
    // Composing に戻る
    assert!(!s.is_empty());
    assert_eq!(s.preedit(), "あ");
    assert_eq!(s.candidate_count(), 0);
    assert!(!s.has_commit());
}

#[test]
fn test_conversion_cursor_down_ffi() {
    let s = TestSession::new();
    for ch in "aiu".chars() {
        s.push_char(&ch.to_string());
    }
    s.push_key(KEY_SPACE);

    let count = s.candidate_count();
    assert!(count >= 1);
    assert_eq!(s.candidate_cursor(), 0);

    s.push_key(KEY_DOWN);
    // count == 1 なら wrap して 0 のまま; count > 1 なら 1 へ
    let expected = if count == 1 { 0 } else { 1 };
    assert_eq!(s.candidate_cursor(), expected);
}

#[test]
fn test_conversion_cursor_full_wrap_ffi() {
    let s = TestSession::new();
    s.push_char("a");
    s.push_key(KEY_SPACE);

    let count = s.candidate_count();
    assert!(count >= 1);

    // count 回 Down を押すと先頭 (0) に戻る
    for _ in 0..count {
        s.push_key(KEY_DOWN);
    }
    assert_eq!(s.candidate_cursor(), 0);
}

#[test]
fn test_conversion_cursor_down_then_up_ffi() {
    let s = TestSession::new();
    s.push_char("a");
    s.push_key(KEY_SPACE);

    assert_eq!(s.candidate_cursor(), 0);
    s.push_key(KEY_DOWN);
    s.push_key(KEY_UP);
    assert_eq!(s.candidate_cursor(), 0);
}

#[test]
fn test_conversion_backspace_cancels_and_deletes_ffi() {
    let s = TestSession::new();
    // "あい" を Conversion へ
    s.push_char("a");
    s.push_char("i");
    s.push_key(KEY_SPACE);

    // Backspace in Conversion: cancel_conversion → Composing + do_backspace (末尾1文字削除)
    assert!(s.push_key(KEY_BACKSPACE));
    assert!(!s.is_empty()); // Composing に戻る
    assert_eq!(s.preedit(), "あ"); // "い" が削除された
    assert_eq!(s.candidate_count(), 0);
}

// ---------------------------------------------------------------------------
// select_candidate — 境界値テスト (FFI)
// ---------------------------------------------------------------------------

#[test]
fn test_select_candidate_in_range_commits_ffi() {
    let s = TestSession::new();
    s.push_char("a");
    s.push_key(KEY_SPACE);

    assert!(s.select_candidate(0));
    assert!(s.has_commit());
    assert!(!s.commit_text().is_empty());
    assert!(s.is_empty());
}

#[test]
fn test_select_candidate_out_of_range_ffi() {
    let s = TestSession::new();
    s.push_char("a");
    s.push_key(KEY_SPACE);

    // 範囲外 → false、Conversion 状態を維持
    assert!(!s.select_candidate(999));
    assert!(!s.is_empty());
    assert!(!s.has_commit());
}

#[test]
fn test_select_candidate_not_in_conversion_ffi() {
    let s = TestSession::new();
    s.push_char("a"); // Composing 状態 (Conversion ではない)
    assert!(!s.select_candidate(0));
}

// ---------------------------------------------------------------------------
// 変換ショートカット (FFI) — ConvertHiragana / ConvertKatakana / ConvertAscii
// ---------------------------------------------------------------------------

#[test]
fn test_convert_hiragana_key_ffi() {
    let s = TestSession::new();
    for ch in "nihongo".chars() {
        s.push_char(&ch.to_string());
    }
    assert_eq!(s.preedit(), "にほんご");

    assert!(s.push_key(KEY_CONVERT_HIRAGANA));
    assert!(s.has_commit());
    assert_eq!(s.commit_text(), "にほんご");
    assert!(s.is_empty());
}

#[test]
fn test_convert_katakana_key_ffi() {
    let s = TestSession::new();
    for ch in "nihongo".chars() {
        s.push_char(&ch.to_string());
    }

    assert!(s.push_key(KEY_CONVERT_KATAKANA));
    assert!(s.has_commit());
    assert_eq!(s.commit_text(), "ニホンゴ");
    assert!(s.is_empty());
}

#[test]
fn test_convert_ascii_key_ffi() {
    let s = TestSession::new();
    // "にほんご" → hiragana_to_romaji → "nihonngo"
    for ch in "nihongo".chars() {
        s.push_char(&ch.to_string());
    }

    assert!(s.push_key(KEY_CONVERT_ASCII));
    assert!(s.has_commit());
    assert_eq!(s.commit_text(), "nihonngo");
    assert!(s.is_empty());
}

#[test]
fn test_convert_keys_when_empty_not_consumed_ffi() {
    let s = TestSession::new();
    // Empty 状態では全ての変換キーが非消費
    assert!(!s.push_key(KEY_CONVERT_HIRAGANA));
    assert!(!s.push_key(KEY_CONVERT_KATAKANA));
    assert!(!s.push_key(KEY_CONVERT_ASCII));
}

#[test]
fn test_convert_katakana_from_conversion_ffi() {
    let s = TestSession::new();
    s.push_char("a"); // "あ"
    s.push_key(KEY_SPACE); // → Conversion

    assert!(s.push_key(KEY_CONVERT_KATAKANA));
    assert!(s.has_commit());
    assert_eq!(s.commit_text(), "ア");
    assert!(s.is_empty());
}

// ---------------------------------------------------------------------------
// 入力バリデーション (FFI)
// ---------------------------------------------------------------------------

#[test]
fn test_control_char_rejected_ffi() {
    let s = TestSession::new();
    // Ctrl+A (\x01) は制御文字なので拒否される
    assert!(!s.push_char("\x01"));
    assert!(s.is_empty());
}

#[test]
fn test_empty_string_push_char_rejected_ffi() {
    let s = TestSession::new();
    // 空文字列（コードポイントなし）は拒否される
    let cs = CString::new("").unwrap();
    assert_eq!(karukan_push_char(s.ptr(), cs.as_ptr()), 0);
    assert!(s.is_empty());
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

// ---------------------------------------------------------------------------
// 全角スペース
// ---------------------------------------------------------------------------

/// Empty 状態で Space を押すと全角スペース (U+3000) がコミットされる。
#[test]
fn test_space_on_empty_commits_fullwidth_space_ffi() {
    let s = TestSession::new();
    assert!(s.is_empty());
    // Space を押すと consumed かつ U+3000 がコミットされる。
    assert!(s.push_key(KEY_SPACE), "Space in Empty should be consumed");
    assert!(s.has_commit(), "should have pending commit");
    assert_eq!(s.commit_text(), "\u{3000}", "should commit fullwidth space");
    assert!(s.is_empty(), "state should remain Empty");
}

/// Composing 状態で Space を押しても全角スペースはコミットされない（変換モードへ）。
#[test]
fn test_space_on_composing_does_not_commit_fullwidth_space_ffi() {
    let s = TestSession::new();
    s.push_char("a"); // "あ"
    s.push_key(KEY_SPACE); // → Conversion
    // コミットはされない（変換候補が表示される）。
    assert!(
        !s.has_commit(),
        "Space in Composing should not commit fullwidth space"
    );
}
