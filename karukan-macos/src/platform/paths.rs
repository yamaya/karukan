//! macOS data-directory resolution.
//!
//! # Production (IME process)
//! Returns `~/Library/Application Support/Karukan/`.
//!
//! The IME process is launched by the macOS system and does **not** inherit
//! the user's shell environment, so XDG variables (`$XDG_DATA_HOME` etc.)
//! are unavailable.  `~/Library/Application Support/` is the correct macOS
//! standard location and is also the path that macOS App Sandbox redirects to
//! for the App Extension container.
//!
//! # Testing / CI
//! Set `KARUKAN_DATA_DIR` to any writable directory to override the default.
//! This avoids polluting `~/Library/Application Support/` during test runs.
//!
//! ```bash
//! KARUKAN_DATA_DIR=/tmp/karukan-test cargo test -p karukan-macos
//! ```

use std::path::PathBuf;

/// Returns the application data directory.
///
/// Precedence:
/// 1. `$KARUKAN_DATA_DIR` — test / CI override
/// 2. `~/Library/Application Support/Karukan/` — macOS standard
/// 3. `$HOME/Library/Application Support/Karukan/` — fallback if `dirs` fails
pub fn app_support_dir() -> PathBuf {
    // 1. Test / CI override
    if let Ok(p) = std::env::var("KARUKAN_DATA_DIR") {
        return PathBuf::from(p);
    }

    // 2. macOS standard: dirs::data_local_dir() → ~/Library/Application Support/
    dirs::data_local_dir()
        .unwrap_or_else(|| {
            // 3. Fallback: construct from $HOME
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
        })
        .join("Karukan")
}

/// Directory where GGUF model files are stored.
pub fn models_dir() -> PathBuf {
    app_support_dir().join("models")
}

/// Path to the learning cache TSV file.
pub fn learning_cache_path() -> PathBuf {
    app_support_dir().join("learning.tsv")
}

/// Directory for user-provided dictionaries.
pub fn user_dict_dir() -> PathBuf {
    app_support_dir().join("user_dicts")
}

/// Path to the compiled system dictionary.
pub fn system_dict_path() -> PathBuf {
    app_support_dir().join("dict.bin")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_karukan_data_dir_override() {
        // SAFETY: test-only; single-threaded environment assumed.
        unsafe { std::env::set_var("KARUKAN_DATA_DIR", "/tmp/karukan-test-override") };
        let dir = app_support_dir();
        assert_eq!(dir, PathBuf::from("/tmp/karukan-test-override"));
        // Clean up so other tests are not affected.
        unsafe { std::env::remove_var("KARUKAN_DATA_DIR") };
    }

    #[test]
    fn test_sub_paths_are_children_of_app_support() {
        // SAFETY: test-only; single-threaded environment assumed.
        unsafe { std::env::set_var("KARUKAN_DATA_DIR", "/tmp/karukan-test-paths") };
        let base = app_support_dir();
        assert_eq!(models_dir(), base.join("models"));
        assert_eq!(learning_cache_path(), base.join("learning.tsv"));
        assert_eq!(user_dict_dir(), base.join("user_dicts"));
        assert_eq!(system_dict_path(), base.join("dict.bin"));
        unsafe { std::env::remove_var("KARUKAN_DATA_DIR") };
    }
}
