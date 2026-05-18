use std::{path::PathBuf, time::Duration};

use crate::util::{config_file, parse_bool};

#[derive(Debug, Clone)]
pub struct Config {
    pub refresh_interval: Duration,
    pub extra_regions: Vec<String>,
    pub redact_default: Option<bool>,
    pub grouped_default: Option<bool>,
    pub theme: String,
    pub icons: String, // "unicode" | "ascii"
    pub notify_bell: bool,
    pub required_tags: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            refresh_interval: Duration::from_secs(15),
            extra_regions: Vec::new(),
            redact_default: None,
            grouped_default: None,
            theme: "dark".into(),
            icons: "unicode".into(),
            notify_bell: false,
            required_tags: Vec::new(),
        }
    }
}

pub fn load() -> Config {
    let path = config_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Config::default();
    };
    parse(&text)
}

pub fn parse(text: &str) -> Config {
    let mut cfg = Config::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, raw_val)) = line.split_once('=') else { continue };
        let key = key.trim();
        let value = raw_val.trim().trim_matches('"').to_string();
        match key {
            "refresh_interval_secs" => {
                if let Ok(n) = value.parse::<u64>() {
                    if n > 0 {
                        cfg.refresh_interval = Duration::from_secs(n);
                    }
                }
            }
            "extra_regions" => {
                cfg.extra_regions = value
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            "redact_default" => cfg.redact_default = parse_bool(&value),
            "grouped_default" => cfg.grouped_default = parse_bool(&value),
            "theme" => cfg.theme = value,
            "icons" => cfg.icons = value,
            "notify_bell" => {
                if let Some(b) = parse_bool(&value) {
                    cfg.notify_bell = b;
                }
            }
            "required_tags" => {
                cfg.required_tags = value
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            _ => {}
        }
    }
    cfg
}

fn config_path() -> PathBuf {
    config_file("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_overrides_defaults() {
        let text = r#"
refresh_interval_secs = 30
extra_regions = "us-gov-east-1, cn-north-1"
redact_default = true
grouped_default = false
"#;
        let cfg = parse(text);
        assert_eq!(cfg.refresh_interval, Duration::from_secs(30));
        assert_eq!(cfg.extra_regions, vec!["us-gov-east-1".to_string(), "cn-north-1".to_string()]);
        assert_eq!(cfg.redact_default, Some(true));
        assert_eq!(cfg.grouped_default, Some(false));
    }

    #[test]
    fn parse_ignores_zero_interval() {
        let cfg = parse("refresh_interval_secs = 0\n");
        assert_eq!(cfg.refresh_interval, Duration::from_secs(15));
    }

    #[test]
    fn parse_empty_returns_defaults() {
        let cfg = parse("");
        assert_eq!(cfg.refresh_interval, Duration::from_secs(15));
        assert!(cfg.extra_regions.is_empty());
        assert!(cfg.redact_default.is_none());
    }
}
