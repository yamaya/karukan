---
paths: ["karukan-engine/**", "karukan-cli/**", "karukan-im/**"]
---

# Architecture

## karukan-engine (`karukan-engine/src/`)

- `lib.rs` — Library entry point and re-exports
- `romaji/` — Romaji-to-hiragana conversion
  - `trie.rs` — Trie data structure
  - `rules.rs` — 200+ conversion rules
  - `converter.rs` — FSM converter
- `kanji/` — Kana-kanji conversion via llama.cpp
  - `backend.rs` — Backend + KanaKanjiConverter
  - `llamacpp.rs` — GGUF inference
  - `hf_download.rs` — HuggingFace model download
  - `model_config.rs` — models.toml registry
  - `error.rs` — KanjiError type
- `dict.rs` — Double-array trie system dictionary
- `learning.rs` — Learning cache (user conversion history, TSV persistence, recency+frequency scoring)
- `kana.rs` — Hiragana/katakana utilities

## karukan-cli (`karukan-cli/src/`)

- `bin/dict.rs` — Dictionary tool: build (JSON or Mozc TSV → binary) and view (web UI + CLI search)
- `bin/sudachi_dict.rs` — Sudachi dictionary → scored JSON converter
- `bin/server.rs` — Axum HTTP API server
- `bin/ajimee_bench.rs` — AJIMEE-Bench evaluation
- `static/` — Web UI assets for server and dict-viewer

## karukan-im (`karukan-im/src/`)

- `core/engine/` — IMEEngine state machine (Empty → Composing → Conversion)
  - `mod.rs` — Main InputMethodEngine struct and core processing logic
  - `types.rs` — EngineConfig, EngineResult, EngineAction, Converters, ConversionStrategy
  - `input.rs` — Key input handling for Composing state
  - `input_buffer.rs` — Input buffer (hiragana text + cursor position)
  - `conversion.rs` — Conversion mode handling
  - `cursor.rs` — Cursor movement
  - `display.rs` — Preedit text display
  - `mode.rs` — Mode switching (katakana, alphabet, live conversion)
  - `init.rs` — Model loading, dictionary setup, learning cache init
  - `strategy.rs` — Conversion strategy determination and adaptive model selection
  - `tests.rs` — Engine unit tests
- `core/preedit.rs` — Preedit composition with cursor support
- `core/candidate.rs` — Candidate list with pagination support
- `core/keycode.rs` — Key symbol definitions and key event handling
- `core/state.rs` — Engine state definitions
- `config/settings.rs` — User settings (`~/.config/karukan-im/config.toml`)
- `ffi.rs` — C FFI for fcitx5 C++ addon
- `fcitx5-addon/src/karukan.cpp` — C++ fcitx5 wrapper
