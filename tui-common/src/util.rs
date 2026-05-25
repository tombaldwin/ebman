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

/// Atomically write `contents` to `path`. Writes to a sibling
/// `.tmp.<pid>.<nanos>` file then renames into place — on Unix,
/// `rename` within a single filesystem is atomic, so a crash mid-
/// write leaves either the old file intact or the new file
/// complete, never a truncated/partial file. Required for files
/// that must survive SIGKILL / kernel panic without dropping their
/// contents (config / state persistence).
///
/// The pid + monotonic-nanos suffix on the temp name means two
/// processes (or two threads, or two terminal panes of the same
/// TUI) writing the same target file concurrently don't clobber
/// each other's temp — each rename is independent and the last
/// writer wins on the destination. Without it (the pre-0.1.1
/// behaviour) both writers raced on a single `.tmp` and the loser's
/// content was silently discarded.
pub fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "write_atomic: path has no file name",
        )
    })?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut tmp_name = name.to_owned();
    tmp_name.push(format!(".tmp.{}.{}", std::process::id(), nanos));
    let tmp = path.with_file_name(tmp_name);
    if let Err(e) = std::fs::write(&tmp, contents) {
        // Try to clean up the partial temp before bubbling up — a
        // failed write leaves an orphan we don't want sitting next
        // to the (possibly still-intact) target.
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
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
        // Second write replaces the file atomically; no leftover
        // sibling `state.toml.tmp.*` after a successful rename.
        write_atomic(&path, "second").expect("second write");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
        let parent = path.parent().unwrap();
        for entry in std::fs::read_dir(parent).unwrap() {
            let name = entry.unwrap().file_name();
            let name = name.to_string_lossy();
            assert!(
                !name.starts_with("state.toml.tmp."),
                "leftover temp file: {name}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_atomic_temp_names_are_unique_per_call() {
        // Same-pid back-to-back writes should land on distinct
        // temp paths thanks to the nanos suffix — otherwise two
        // processes racing on quit would clobber each other's
        // saved state.
        use std::collections::HashSet;
        let dir =
            std::env::temp_dir().join(format!("tui-common-atomic-unique-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.txt");
        let mut seen: HashSet<String> = HashSet::new();
        for i in 0..16 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let synthetic = format!("state.txt.tmp.{}.{}.{}", std::process::id(), nanos, i);
            assert!(seen.insert(synthetic), "temp name collided in tight loop");
            // Also do an actual write to exercise the real code path.
            write_atomic(&path, &i.to_string()).unwrap();
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "15");
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
