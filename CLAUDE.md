# CLAUDE.md

karukan is a Linux Japanese Input Method system — three Rust crates in a Cargo workspace.

## Crates

- **karukan-engine**: Core library — romaji→hiragana, neural kana-kanji (llama.cpp), system dictionary, learning cache
- **karukan-cli**: CLI tools and server — dictionary builder, Sudachi converter, dict viewer, AJIMEE-Bench, HTTP API
- **karukan-im**: fcitx5 IME addon using karukan-engine

## Universal Commands

```bash
cargo build --release       # Build all crates
cargo test --workspace      # Run all tests
cargo fmt --all             # Format
cargo clippy --workspace    # Lint
```

## Training

Model training is handled by the separate `karukan-jinen` Python project (not in this repo). Trains GPT-2 based models for kana-kanji conversion using jinen format; outputs GGUF files.

## See Also

@.claude/rules/architecture.md
@.claude/rules/design-patterns.md
@.claude/rules/build-commands.md
