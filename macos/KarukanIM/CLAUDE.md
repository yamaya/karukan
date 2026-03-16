# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

KarukanIM is the macOS Input Method (IME) host application for the karukan Japanese input system. It bridges the Rust karukan-engine to macOS via IMKit, providing romaji-to-hiragana conversion, neural kana-kanji conversion, and live (inline) conversion.

## Build & Run

```bash
# Build and install (from macos/KarukanIM/)
mise run install    # xcodebuild Release → ~/Library/Input Methods/KarukanIM.app → opens it

# Clean
mise run clean

# Manual build
xcodebuild -scheme KarukanIM -configuration Release build | xcpretty

# Restart IME after code changes
killall KarukanIMExtension
```

Build requires the Rust cdylib from `karukan-macos` crate (header at `../../karukan-macos/include/karukan_macos.h`).

## Parent Project

This is a subdirectory of the karukan workspace. See the root `CLAUDE.md` for Rust crate architecture, engine design patterns, and cargo commands.

## See Also

@.claude/rules/architecture.md
@.claude/rules/keybindings.md
