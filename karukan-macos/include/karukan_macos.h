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
 * Select the candidate at index and commit it immediately.
 *
 * Used by candidateSelected(_:) in Swift when the user clicks a candidate
 * in the IMKCandidates panel.
 * Returns 1 on success, 0 if not in Conversion state or index is out of range.
 * Returns 0 if session is NULL.
 */
int karukan_select_candidate(KarukanSession* session, uint32_t index);

#ifdef __cplusplus
}
#endif

#endif /* KARUKAN_MACOS_H */
