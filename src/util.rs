use std::path::PathBuf;

pub fn parse_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

pub fn config_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".config/ebman");
        return p;
    }
    PathBuf::from(".")
}

pub fn cache_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".cache/ebman");
        return p;
    }
    PathBuf::from(".")
}

pub fn config_file(name: &str) -> PathBuf {
    config_dir().join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

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

