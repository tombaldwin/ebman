use std::{path::PathBuf, time::Duration};

use crate::util::{config_file, parse_bool};

#[derive(Debug, Clone)]
pub struct Config {
    pub refresh_interval: Duration,
    pub extra_regions: Vec<String>,
    pub redact_default: Option<bool>,
    pub grouped_default: Option<bool>,
    pub theme: String,
    /// Glyph set: `"unicode"` (default), `"ascii"` for low-feature
    /// terminals, `"powerline"` (alias `"nerd"`) for Powerline-patched /
    /// Nerd Fonts, or `"auto"` to probe the terminal at startup and pick
    /// powerline if its support is detected, unicode otherwise.
    pub icons: String,
    pub notify_bell: bool,
    pub required_tags: Vec<String>,
    /// Optional URL to POST a small JSON payload to when an env transitions
    /// into Red health. Use anything that accepts a webhook (Slack, Discord,
    /// custom collector). Disabled when unset.
    pub webhook_url: Option<String>,
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
            webhook_url: None,
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
        let Some((key, raw_val)) = line.split_once('=') else {
            continue;
        };
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
            "webhook_url" => {
                let v = value.trim();
                cfg.webhook_url = if v.is_empty() {
                    None
                } else {
                    Some(v.to_string())
                };
            }
            _ => {}
        }
    }
    cfg
}

pub fn config_path() -> PathBuf {
    config_file("config.toml")
}

/// Serialise the config back to disk. Round-trips the parse format and
/// over-writes the user's existing file. Used by the `:settings` form.
pub fn save(cfg: &Config) -> std::io::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serialize(cfg);
    std::fs::write(&path, body)
}

/// Pure: render a `Config` into the TOML-ish line-oriented format the
/// parser reads. Used by `save` and unit tests.
pub fn serialize(cfg: &Config) -> String {
    let mut out = String::new();
    out.push_str("# ebman configuration — written by :settings; hand-edits welcome\n\n");
    out.push_str(&format!(
        "refresh_interval_secs = {}\n",
        cfg.refresh_interval.as_secs()
    ));
    out.push_str(&format!(
        "extra_regions = \"{}\"\n",
        cfg.extra_regions.join(",")
    ));
    if let Some(b) = cfg.redact_default {
        out.push_str(&format!("redact_default = {b}\n"));
    }
    if let Some(b) = cfg.grouped_default {
        out.push_str(&format!("grouped_default = {b}\n"));
    }
    out.push_str(&format!("theme = \"{}\"\n", cfg.theme));
    out.push_str(&format!("icons = \"{}\"\n", cfg.icons));
    out.push_str(&format!("notify_bell = {}\n", cfg.notify_bell));
    if !cfg.required_tags.is_empty() {
        out.push_str(&format!(
            "required_tags = \"{}\"\n",
            cfg.required_tags.join(",")
        ));
    }
    if let Some(url) = &cfg.webhook_url {
        out.push_str(&format!("webhook_url = \"{url}\"\n"));
    }
    out
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
        assert_eq!(
            cfg.extra_regions,
            vec!["us-gov-east-1".to_string(), "cn-north-1".to_string()]
        );
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

    #[test]
    fn parse_icons_auto_is_preserved() {
        let cfg = parse("icons = \"auto\"\n");
        assert_eq!(cfg.icons, "auto");
    }

    #[test]
    fn serialize_round_trips_full_config() {
        let mut cfg = Config::default();
        cfg.refresh_interval = Duration::from_secs(45);
        cfg.extra_regions = vec!["eu-south-2".into(), "ap-southeast-4".into()];
        cfg.redact_default = Some(true);
        cfg.grouped_default = Some(false);
        cfg.theme = "high-contrast".into();
        cfg.icons = "powerline".into();
        cfg.notify_bell = true;
        cfg.required_tags = vec!["Owner".into(), "Env".into()];
        cfg.webhook_url = Some("https://hooks.example/abc".into());

        let body = serialize(&cfg);
        let reparsed = parse(&body);
        assert_eq!(reparsed.refresh_interval, cfg.refresh_interval);
        assert_eq!(reparsed.extra_regions, cfg.extra_regions);
        assert_eq!(reparsed.redact_default, cfg.redact_default);
        assert_eq!(reparsed.grouped_default, cfg.grouped_default);
        assert_eq!(reparsed.theme, cfg.theme);
        assert_eq!(reparsed.icons, cfg.icons);
        assert_eq!(reparsed.notify_bell, cfg.notify_bell);
        assert_eq!(reparsed.required_tags, cfg.required_tags);
        assert_eq!(reparsed.webhook_url, cfg.webhook_url);
    }

    #[test]
    fn serialize_round_trips_default_config() {
        let cfg = Config::default();
        let body = serialize(&cfg);
        let reparsed = parse(&body);
        assert_eq!(reparsed.refresh_interval, cfg.refresh_interval);
        assert_eq!(reparsed.theme, cfg.theme);
        assert_eq!(reparsed.icons, cfg.icons);
        assert!(reparsed.extra_regions.is_empty());
        assert!(reparsed.required_tags.is_empty());
        assert!(reparsed.webhook_url.is_none());
    }
}
