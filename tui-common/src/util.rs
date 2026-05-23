//! Tiny generic helpers shared across TUI apps. Anything in here must
//! be app-neutral — no config-dir layout, no flavour-specific
//! conventions. Bigger or more opinionated helpers belong in their
//! own module.

use std::io;
use std::path::Path;

/// Parse a string into a `bool` with the conventional truthy / falsy
/// vocabulary every TUI ends up wanting: `true` / `1` / `yes` / `on`
/// (case-insensitive) → `Some(true)`; `false` / `0` / `no` / `off`
/// → `Some(false)`; anything else → `None`. Suitable for parsing
/// config-file `key = value` lines where the operator might type any
/// of the common forms.
pub fn parse_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Atomically write `contents` to `path`. Writes to a sibling `.tmp`
/// file then renames into place — on Unix, `rename` within a single
/// filesystem is atomic, so a crash mid-write leaves either the old
/// file intact or the new file complete, never a truncated/partial
/// file. Required for files that must survive SIGKILL / kernel panic
/// without dropping their contents (config / state persistence).
pub fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = match path.file_name() {
        Some(name) => {
            let mut tmp_name = name.to_owned();
            tmp_name.push(".tmp");
            path.with_file_name(tmp_name)
        }
        None => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "write_atomic: path has no file name",
            ));
        }
    };
    std::fs::write(&tmp, contents)?;
    // Rename is the atomic step. If it fails the temp file is left
    // behind; better than corrupting the target.
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::{parse_bool, write_atomic};

    #[test]
    fn write_atomic_creates_parent_and_replaces_existing() {
        let dir = std::env::temp_dir().join(format!("tui-common-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested/deep/state.toml");
        // First write creates dir hierarchy + the file.
        write_atomic(&path, "first").expect("first write");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
        // Second write replaces the file atomically; the .tmp file
        // must not linger.
        write_atomic(&path, "second").expect("second write");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
        let tmp = path.with_file_name("state.toml.tmp");
        assert!(
            !tmp.exists(),
            ".tmp file should be renamed away, not left behind"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_bool_accepts_canonical_forms() {
        for s in ["true", "1", "yes", "on", "ON", "Yes", "TRUE"] {
            assert_eq!(parse_bool(s), Some(true), "expected true for {s:?}");
        }
        for s in ["false", "0", "no", "off", "OFF", "No"] {
            assert_eq!(parse_bool(s), Some(false), "expected false for {s:?}");
        }
        for s in ["", "maybe", "2", "trueish"] {
            assert_eq!(parse_bool(s), None, "expected None for {s:?}");
        }
    }
}
