---
paths: ["**/*.swift", "**/*.h"]
---

# Architecture

## Two Targets

- **KarukanIM (host app)** — LSUIElement app that runs IMKServer. Contains the full-featured `KarukanInputController` with live conversion, candidate panel, segment navigation, and menu system.
- **KarukanIMExtension (.appex)** — Input Method Extension with a minimal Phase 2 implementation. Simplified input handling only.

## Rust-Swift Bridge

C FFI via Objective-C bridging header. Each target has its own `KarukanBridge.h` that includes `karukan_macos.h`. The Rust side is the `karukan-macos` crate (cdylib).

Key FFI pattern: Rust returns `*const c_char` pointers; Swift immediately copies with `String(cString:)`. All session operations go through an `OpaquePointer` (`KarukanSession*`).

## KarukanInputController Design Patterns

- **Async initialization**: `karukan_session_new()` on main thread (lightweight), `karukan_session_init()` on background queue (loads dictionary/models). `initialized` flag gates conversion features.
- **Live conversion pipeline**: Main thread gets composing hiragana → background calls `karukan_convert_top1()` → main thread applies result. Uses generation counter to discard stale results and a semaphore to limit to 1 concurrent inference.
- **Consonant delay timer**: Suppresses display of pending consonants (e.g., "k" before "ka") to prevent flicker. Configurable delay (0–0.3s).
- **Segment mode**: After Space conversion, arrow keys navigate segments with thick/single underline distinction for selected/unselected.
- **`currentSender` tracking**: Maintains a valid client reference because IMKCandidates panel clicks invalidate `client()`.
- **`hasPreedit` flag**: Prevents sending empty `setMarkedText` when there's nothing to clear, avoiding interference with `insertText`.

## Settings (SettingStore.swift)

Shared via App Group UserDefaults (`io.github.yamaya.inputmethod.KarukanIM`):
- `liveConversionEnabled` (default: true)
- `consonantDelaySec` (default: 0.0)
- `autoCommitMaxChars` (default: 30)
