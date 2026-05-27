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

/// Escape a string per the JSON string-escape spec. Returns the
/// escaped INNER content (no surrounding `"`) so callers can embed
/// the result inside a larger hand-rolled JSON body. Pair with
/// [`json_string`] when you want the value wrapped + escaped in
/// one call.
///
/// One canonical helper for the whole crate (lib + bin). Pre-0.16
/// there were six near-identical variants scattered across
/// `audit.rs` / `cli/mod.rs` / `lint.rs` / `app.rs` / `llm.rs`;
/// they're all routed through this now.
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// Escape + wrap in `"..."` for use as a complete JSON string
/// literal. Same escape semantics as [`json_escape`]; convenience
/// wrapper that adds the surrounding quotes.
pub fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    out.push_str(&json_escape(s));
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::{json_escape, json_string};

    #[test]
    fn json_escape_escapes_quotes_backslashes_newlines_tabs() {
        assert_eq!(json_escape(""), "");
        assert_eq!(json_escape("hello"), "hello");
        assert_eq!(json_escape("with \"quotes\""), "with \\\"quotes\\\"");
        assert_eq!(json_escape("a\\b"), "a\\\\b");
        assert_eq!(json_escape("a\nb"), "a\\nb");
        assert_eq!(json_escape("a\tb"), "a\\tb");
        assert_eq!(json_escape("a\rb"), "a\\rb");
        // Sub-0x20 control chars get \uXXXX escapes.
        assert_eq!(json_escape("\x01"), "\\u0001");
        assert_eq!(json_escape("\x07"), "\\u0007");
    }

    #[test]
    fn json_string_wraps_in_quotes() {
        assert_eq!(json_string(""), "\"\"");
        assert_eq!(json_string("hello"), "\"hello\"");
        assert_eq!(json_string("with \"quotes\""), "\"with \\\"quotes\\\"\"");
    }

    #[test]
    fn json_string_round_trips_via_yaml_parser() {
        // YAML is a JSON superset; serde_yml parses both. Useful
        // cross-check that our hand-rolled escape is spec-compliant.
        let inputs = [
            "",
            "plain",
            "with \"quotes\" and \\ backslashes",
            "line1\nline2\twith tab",
            "control \x01\x02 chars",
        ];
        for input in inputs {
            let escaped = json_string(input);
            let parsed: String = serde_yml::from_str(&escaped)
                .unwrap_or_else(|e| panic!("json_string({input:?}) = {escaped} failed: {e}"));
            assert_eq!(parsed, input);
        }
    }
}
