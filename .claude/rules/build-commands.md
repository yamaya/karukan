---
paths: ["karukan-cli/**", "karukan-im/**", "Cargo.toml"]
---

# Crate-Specific Build Commands

## karukan-engine

```bash
cargo build -p karukan-engine --release
cargo test -p karukan-engine  # includes integration tests (model auto-downloaded on first run)
```

## karukan-cli

```bash
cargo build -p karukan-cli --release

# Start the server (auto-downloads models from HuggingFace)
cargo run --release --bin karukan-server

# Build dictionary from JSON or Mozc TSV
cargo run --release --bin karukan-dict -- build input.json -o dict.bin

# Build scored dictionary from Sudachi CSV
cargo run --release --bin sudachi-dict -- input.csv -o scored.json

# Dictionary viewer (web UI + CLI search)
cargo run --release --bin karukan-dict -- view dict.bin

# AJIMEE-Bench evaluation
cargo run --release --bin ajimee-bench -- evaluation_items.json
```

## karukan-im

```bash
cargo build -p karukan-im --release
cargo test -p karukan-im

# Build and install fcitx5 addon
cd karukan-im/fcitx5-addon

# Option A: System install (sudo required, no FCITX_ADDON_DIRS needed)
cmake -B build -DCMAKE_INSTALL_PREFIX=/usr
cmake --build build -j
sudo cmake --install build

# Option B: User-local install (no sudo, requires FCITX_ADDON_DIRS)
cmake -B build -DCMAKE_INSTALL_PREFIX=$HOME/.local
cmake --build build -j
cmake --install build
```
