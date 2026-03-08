/**
 * karukan_macos.h — C FFI interface for karukan-macos
 *
 * This header is the Swift–Rust bridge for KarukanIM.appex (Phase 2+).
 * Include it in the Xcode project's bridging header or use it with
 * @_silgen_name / @_cdecl in Swift.
 *
 * All functions are thread-safe with respect to null pointers (null is
 * returned / no-op executed).  A single KarukanSession must NOT be used
 * from multiple threads concurrently.
 *
 * Pointer lifetimes
 * -----------------
 * Pointers returned by karukan_get_preedit(), karukan_get_commit(), and
 * karukan_get_candidate() are valid until the next karukan_push_* or
 * karukan_select_candidate call on the same session.  Copy them
 * immediately using String(cString:) in Swift.
 */

#ifndef KARUKAN_MACOS_H
#define KARUKAN_MACOS_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* -------------------------------------------------------------------------
 * Opaque session type
 * ---------------------------------------------------------------------- */

typedef struct KarukanSession KarukanSession;

/* -------------------------------------------------------------------------
 * Special-key constants (passed to karukan_push_key)
 * ---------------------------------------------------------------------- */

typedef enum {
    KARUKAN_KEY_RETURN    = 1,
    KARUKAN_KEY_BACKSPACE = 2,
    KARUKAN_KEY_ESCAPE    = 3,
    KARUKAN_KEY_SPACE     = 4,
    KARUKAN_KEY_LEFT      = 5,
    KARUKAN_KEY_RIGHT     = 6,
    KARUKAN_KEY_UP        = 7,
    KARUKAN_KEY_DOWN      = 8,
    KARUKAN_KEY_TAB       = 9,
    KARUKAN_KEY_CONVERT_HIRAGANA  = 10,  /* Ctrl+J */
    KARUKAN_KEY_CONVERT_KATAKANA  = 11,  /* Ctrl+K */
    KARUKAN_KEY_CONVERT_ASCII     = 12,  /* Ctrl+; */
    KARUKAN_KEY_SHRINK_SEGMENT    = 13,  /* Ctrl+I / Shift+Left */
    KARUKAN_KEY_EXTEND_SEGMENT    = 14,  /* Ctrl+O / Shift+Right */
} KarukanKey;

/* -------------------------------------------------------------------------
 * Lifecycle
 * ---------------------------------------------------------------------- */

/**
 * Allocate a new KarukanSession.
 *
 * Lightweight — safe to call on the main thread.
 * Call karukan_session_init() from a background thread to load resources.
 *
 * Returns NULL on allocation failure.
 */
KarukanSession* karukan_session_new(void);

/**
 * Load resources: system dictionary, learning cache.
 *
 * Phase 3 will add model loading here.  Call from a background thread
 * (DispatchQueue.global().async) to avoid blocking the main thread.
 *
 * Returns 0 on success, -1 on error.
 * Missing resource files are silently skipped (non-fatal).
 */
int karukan_session_init(KarukanSession* session);

/**
 * Free a KarukanSession and persist the learning cache.
 *
 * Passing NULL is a no-op.
 * The pointer must not be used after this call.
 */
void karukan_session_free(KarukanSession* session);

/* -------------------------------------------------------------------------
 * Input
 * ---------------------------------------------------------------------- */

/**
 * Push a single printable character into the IME.
 *
 * `c` must be a null-terminated UTF-8 string containing exactly one Unicode
 * code point.  Control characters are rejected.
 *
 * Returns 1 if the IME consumed the character, 0 if it should be passed
 * through to the application.
 * Returns 0 if session or c is NULL.
 */
int karukan_push_char(KarukanSession* session, const char* c);

/**
 * Push a special key into the IME.
 *
 * `key` must be one of the KARUKAN_KEY_* constants above.
 *
 * Returns 1 if the IME consumed the key, 0 if the application should
 * handle it (e.g. arrow keys when the session is empty).
 * Returns 0 if session is NULL or the key is unknown.
 */
int karukan_push_key(KarukanSession* session, uint32_t key);

/* -------------------------------------------------------------------------
 * State queries
 *
 * Call these after every karukan_push_* to update the UI.
 * Returned pointers are valid until the next karukan_push_* call.
 * ---------------------------------------------------------------------- */

/**
 * Returns a pointer to the current preedit text (null-terminated UTF-8).
 *
 * Copy immediately: String(cString: karukan_get_preedit(session))
 * Returns NULL if session is NULL.
 */
const char* karukan_get_preedit(const KarukanSession* session);

/**
 * Returns the byte length of the preedit text (excluding the null terminator).
 * Returns 0 if session is NULL.
 */
uint32_t karukan_get_preedit_len(const KarukanSession* session);

/**
 * Returns the caret position within the preedit as a byte offset.
 *
 * Used by IMKInputController to position the insertion-point underline.
 * Returns 0 if session is NULL.
 */
uint32_t karukan_get_preedit_caret(const KarukanSession* session);

/**
 * Returns 1 if there is a pending commit text, 0 otherwise.
 * Returns 0 if session is NULL.
 */
int karukan_has_commit(const KarukanSession* session);

/**
 * Returns a pointer to the pending commit text (null-terminated UTF-8).
 *
 * Check karukan_has_commit() first; if 0, this points to an empty string.
 * Copy immediately before the next push call.
 * Returns NULL if session is NULL.
 */
const char* karukan_get_commit(const KarukanSession* session);

/**
 * Returns 1 if the session has no pending input, 0 if composing.
 *
 * When 1, arrow keys and function keys should be passed to the application.
 * Returns 1 (empty) if session is NULL.
 */
int karukan_is_empty(const KarukanSession* session);

/**
 * Returns 1 if the romaji converter has an unconverted consonant pending
 * (e.g. "k", "sh", "ch"), 0 otherwise.
 *
 * Swift uses this to decide whether to delay the preedit update so that
 * the bare consonant does not flicker before being resolved to kana.
 * Returns 0 if session is NULL.
 */
int karukan_is_consonant_pending(const KarukanSession* session);

/**
 * Returns the byte length of the pending romaji buffer (0 = none pending).
 * Swift uses this to apply dotted underline to the unconverted romaji portion
 * of the preedit, distinguishing it from confirmed hiragana (single underline).
 */
uint32_t karukan_get_romaji_buf_len(const KarukanSession* session);

/* -------------------------------------------------------------------------
 * Persistence
 * ---------------------------------------------------------------------- */

/**
 * Persist the learning cache to disk if it has unsaved changes.
 *
 * Call when the input context deactivates (focus change / IME switch) to
 * avoid losing recent conversions on unexpected termination.
 * No-op if session is NULL or the cache has no unsaved changes.
 */
void karukan_save_learning(KarukanSession* session);

/* -------------------------------------------------------------------------
 * Candidates
 *
 * Call these after karukan_push_key(KARUKAN_KEY_SPACE) to populate the
 * candidate panel.  Returned pointers are valid until the next karukan_push_*
 * or karukan_select_candidate call on the same session.
 * ---------------------------------------------------------------------- */

/**
 * Returns the number of conversion candidates available.
 *
 * Returns 0 if session is NULL or there is no active conversion.
 */
uint32_t karukan_get_candidate_count(const KarukanSession* session);

/**
 * Returns a pointer to the null-terminated UTF-8 text of the index-th candidate.
 *
 * Returns NULL if session is NULL or index is out of range.
 * Copy immediately: String(cString: karukan_get_candidate(session, i))
 */
const char* karukan_get_candidate(const KarukanSession* session, uint32_t index);

/**
 * Returns the index of the currently selected candidate.
 *
 * Returns 0 if session is NULL.
 */
uint32_t karukan_get_candidate_cursor(const KarukanSession* session);

/**
 * Select the candidate at index.
 *
 * Used by candidateSelected(_:) in Swift when the user clicks or presses
 * Return in the IMKCandidates panel.
 *
 * BunsetsuConversion 状態では選択文節の display を更新するのみでコミットしない
 * (karukan_has_commit() == 0 のまま)。Swift 側は has_commit を確認してから
 * insertText するかどうかを判断すること。
 *
 * Returns 1 on success, 0 if not in a conversion state or index is out of range.
 * Returns 0 if session is NULL.
 */
int karukan_select_candidate(KarukanSession* session, uint32_t index);

/* -------------------------------------------------------------------------
 * BunsetsuConversion — segment info
 *
 * 文節変換モード中にセグメント情報を取得する。
 * 文節変換モード (BunsetsuConversion) でなければ 0 を返す。
 * ---------------------------------------------------------------------- */

/**
 * BunsetsuConversion 状態の文節数を返す。
 *
 * 0 の場合は文節変換状態ではない (Composing / Empty など)。
 * Swift 側でこの値が 0 より大きければ文節ごとのアンダーライン描画を行う。
 * Returns 0 if session is NULL.
 */
uint32_t karukan_get_segment_count(const KarukanSession* session);

/**
 * BunsetsuConversion 状態の文節 index の現在表示テキストの文字数 (NSString 長) を返す。
 *
 * 日本語文字は全て BMP 範囲のため chars().count() == NSString.length。
 * Returns 0 if session is NULL, not in BunsetsuConversion, or index is out of range.
 */
uint32_t karukan_get_segment_char_count(const KarukanSession* session, uint32_t index);

/**
 * BunsetsuConversion 状態の現在選択中の文節インデックスを返す。
 *
 * Returns 0 if session is NULL or not in BunsetsuConversion.
 */
uint32_t karukan_get_selected_segment(const KarukanSession* session);

/* -------------------------------------------------------------------------
 * Live conversion
 *
 * triggerLiveConversion の非同期フロー:
 *   [main]       karukan_get_composing_hiragana() でひらがなを取得
 *   [background] karukan_convert_top1() で推論（Arc<KanaKanjiConverter> のみ使用）
 *   [main]       karukan_apply_live_candidate() で結果を適用
 * ---------------------------------------------------------------------- */

/**
 * Composing 状態のひらがなを buf にコピーする。
 *
 * バックグラウンドスレッドで推論を起動する直前に、メインスレッドから呼ぶこと。
 * Composing 状態でなければ 0 を返す（コピーなし）。
 *
 * 戻り値: コピーしたバイト数（null 終端除く）。session または buf が NULL なら 0。
 */
int karukan_get_composing_hiragana(
    const KarukanSession* session,
    char* buf,
    size_t buf_len);

/**
 * ひらがなを変換して上位1候補をヒープ確保した文字列で返す。
 *
 * Arc<KanaKanjiConverter> のみ使用するためバックグラウンドスレッドから安全に呼べる。
 * 呼び出し中に session が解放されないことを呼び出し側が保証すること（Swift では
 * クロージャ内で self を strong capture することで保証する）。
 *
 * 戻り値: null 終端 UTF-8 文字列（karukan_free_string で解放）。
 *         モデル未ロード・エラー時は NULL。
 */
char* karukan_convert_top1(
    const KarukanSession* session,
    const char* hiragana_utf8);

/**
 * karukan_convert_top1 が返したポインタを解放する。
 *
 * NULL を渡すと no-op。
 */
void karukan_free_string(char* ptr);

/**
 * バックグラウンド推論の結果を session に適用する。
 *
 * Composing 状態でなければ無視する。preedit を変換済みテキストに更新し dirty にする。
 * メインスレッドからのみ呼ぶこと。
 *
 * source_hiragana_utf8: 推論開始時の composing hiragana（karukan_get_composing_hiragana の戻り値）。
 * 現在の input_buf と異なる場合は stale として無視する（なでし→なでしこ 途中コミットバグ対策）。
 *
 * 戻り値: 1=適用成功, 0=Composing 状態でないか source が stale で無視した
 */
int karukan_apply_live_candidate(
    KarukanSession* session,
    const char* candidate_utf8,
    const char* source_hiragana_utf8);

/**
 * 長文コミット後の残余ひらがなを Composing 状態として注入する。
 *
 * karukan_push_key(KARUKAN_KEY_RETURN) の直後に呼び、文節分割で切り取った
 * 後半のひらがなを次の Composing 入力として引き継ぐ。
 * メインスレッドからのみ呼ぶこと。
 *
 * 戻り値: 1=成功, 0=hiragana_utf8 が NULL または空文字列
 */
int karukan_set_composing_hiragana(
    KarukanSession* session,
    const char* hiragana_utf8);

#ifdef __cplusplus
}
#endif

#endif /* KARUKAN_MACOS_H */
