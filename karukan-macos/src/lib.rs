//! karukan-macos: Japanese Input Method Engine for macOS
//!
//! This crate provides a C FFI layer wrapping `karukan-engine` for use
//! with macOS InputMethodKit. It is intentionally separate from `karukan-im`
//! (which is Linux/fcitx5-specific) and depends directly on `karukan-engine`.
//!
//! # Architecture
//! ```text
//! karukan-engine  (platform-independent core)
//!     ↑
//! karukan-macos   (macOS cdylib — this crate)
//!     ↑
//! KarukanIM.appex (Swift + InputMethodKit, Phase 2)
//! ```

pub mod platform;
pub mod session;

pub(crate) mod ffi;
