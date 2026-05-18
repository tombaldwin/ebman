/// Parse a user-supplied keys.toml that aliases keys to `:command` strings.
///
/// Format (minimal TOML subset, matched to commands.toml):
///
/// ```toml
/// [keys]
/// "F2" = "refresh"
/// "F3" = "redact on"
/// "Z" = "alarms"
/// ```
///
/// Key syntax:
///   - `F1`–`F12` — function keys
///   - A single uppercase letter (case-sensitive; `Z` ≠ `z`)
///
/// The runtime intercepts these in Normal mode and runs the command verbatim
/// via the existing `:command` dispatch.
use std::collections::BTreeMap;

use crate::util::config_file;

#[derive(Debug, Clone, Default)]
pub struct CustomKeys {
    /// Map of key spec → command body (without the leading `:`).
    pub bindings: BTreeMap<String, String>,
    /// Warnings raised while parsing — surfaced as a startup status so the
    /// user knows their config didn't fully take effect.
    pub warnings: Vec<String>,
}

pub fn load() -> CustomKeys {
    let path = config_file("keys.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return CustomKeys::default();
    };
    parse(&text)
}

pub fn parse(text: &str) -> CustomKeys {
    let mut out = CustomKeys::default();
    let mut in_section = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_section = rest.trim() == "keys";
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((key_raw, val_raw)) = line.split_once('=') else {
            continue;
        };
        let key = key_raw.trim().trim_matches('"').to_string();
        let val = val_raw.trim().trim_matches('"').to_string();
        if !is_valid_key(&key) {
            out.warnings
                .push(format!("key '{key}' unrecognised — skipped"));
            continue;
        }
        if val.is_empty() {
            out.warnings
                .push(format!("key '{key}' has empty command — skipped"));
            continue;
        }
        out.bindings.insert(key, val);
    }
    out
}

/// Accepts `F1`..`F12` and single uppercase letters A-Z. The rest of the
/// keymap (lowercase letters, control combos) is reserved for built-ins so
/// the user can't accidentally shadow them.
fn is_valid_key(key: &str) -> bool {
    if let Some(num) = key.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()) {
        return (1..=12).contains(&num);
    }
    key.len() == 1 && key.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::{is_valid_key, parse};

    #[test]
    fn parse_accepts_function_keys_and_uppercase() {
        let text = r#"
[keys]
"F2" = "refresh"
"F11" = "redact on"
"Z" = "alarms"
"#;
        let k = parse(text);
        assert_eq!(k.bindings.len(), 3);
        assert_eq!(k.bindings.get("F2").map(String::as_str), Some("refresh"));
        assert_eq!(k.bindings.get("F11").map(String::as_str), Some("redact on"));
        assert_eq!(k.bindings.get("Z").map(String::as_str), Some("alarms"));
        assert!(k.warnings.is_empty());
    }

    #[test]
    fn parse_rejects_lowercase_keys_with_warning() {
        let text = "[keys]\n\"z\" = \"alarms\"\n";
        let k = parse(text);
        assert!(k.bindings.is_empty());
        assert_eq!(k.warnings.len(), 1);
        assert!(k.warnings[0].contains("'z'"));
    }

    #[test]
    fn parse_rejects_empty_command() {
        let text = "[keys]\n\"F2\" = \"\"\n";
        let k = parse(text);
        assert!(k.bindings.is_empty());
        assert!(k.warnings.iter().any(|w| w.contains("empty")));
    }

    #[test]
    fn is_valid_key_function_range() {
        assert!(is_valid_key("F1"));
        assert!(is_valid_key("F12"));
        assert!(!is_valid_key("F0"));
        assert!(!is_valid_key("F13"));
    }
}
