//! App-specific path helpers for ebman. The generic bits
//! (`parse_bool`, `write_atomic`) live in `tui-common::util` and are
//! re-exported here so existing `crate::util::*` call sites keep
//! working unchanged.

use std::path::PathBuf;

pub use tui_common::util::{parse_bool, write_atomic};

/// XDG-style user config directory for ebman: `~/.config/ebman/`.
/// Falls back to the current working directory when `$HOME` is
/// unset (rare; mostly affects sandboxed test environments).
pub fn config_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".config/ebman");
        return p;
    }
    PathBuf::from(".")
}

/// XDG-style user cache directory for ebman: `~/.cache/ebman/`.
/// Used for the application log, audit log, crash reports, and the
/// cost-explorer cache. Same fallback shape as `config_dir`.
pub fn cache_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".cache/ebman");
        return p;
    }
    PathBuf::from(".")
}

/// Convenience: `config_dir().join(name)`.
pub fn config_file(name: &str) -> PathBuf {
    config_dir().join(name)
}
