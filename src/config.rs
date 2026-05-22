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
    /// Per-profile theme override. Key = AWS profile name, value = theme
    /// name (matches the same names `theme = …` accepts). Lets the
    /// operator pin a high-contrast / dark / light theme to a specific
    /// profile so the visual cue says "you're in prod" without reading
    /// the breadcrumb. Most prod incidents start with "I thought I was
    /// in staging."
    pub profile_themes: std::collections::HashMap<String, String>,
    /// Named accounts reachable via `sts:AssumeRole`. Key = the friendly
    /// name the operator uses with `:account NAME`; value is the full
    /// AssumeRole spec — `role_arn`, `source_profile`, optional
    /// `external_id`, optional `region` override. Lines in `config.toml`
    /// use the form `accounts.NAME.field = "value"`, mirroring the
    /// `metric.LABEL.field` shape that the rest of the config uses.
    pub accounts: std::collections::HashMap<String, AccountSpec>,
    /// Per-environment runbook URLs. Key = env name; value = a URL the
    /// operator wants surfaced during triage. Lines in `config.toml` use
    /// `runbooks.ENV = "https://…"`. Shown in the `:why` overlay.
    pub runbooks: std::collections::HashMap<String, String>,
}

/// A named `sts:AssumeRole` target. The operator typically pins one of
/// these per child account and switches between them via `:account
/// NAME`. `source_profile` carries the base creds (so chained role
/// hops still resolve), `external_id` is optional but required by some
/// trust policies, `region` is optional (falls back to the source
/// profile's / env default).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountSpec {
    pub role_arn: String,
    pub source_profile: Option<String>,
    pub external_id: Option<String>,
    pub region: Option<String>,
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
            profile_themes: std::collections::HashMap::new(),
            accounts: std::collections::HashMap::new(),
            runbooks: std::collections::HashMap::new(),
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
            "profile_themes" => {
                // Format: `prod:high-contrast,staging:dark,default:light`.
                // Whitespace around tokens is tolerated; entries without a
                // `:` separator are skipped. Empty profile / empty theme
                // are skipped so a stray trailing comma can't smuggle in
                // a `"" → ""` mapping.
                cfg.profile_themes = parse_profile_themes(&value);
            }
            other if other.starts_with("runbooks.") => {
                // `runbooks.ENV = "url"`. The key after the dot is the
                // whole env name (EB env names don't contain dots).
                let name = other.trim_start_matches("runbooks.").trim();
                if !name.is_empty() && !value.is_empty() {
                    cfg.runbooks.insert(name.to_string(), value);
                }
            }
            other if other.starts_with("accounts.") => {
                // `accounts.NAME.field = "value"`. Split on the dots so
                // multi-line specs accumulate into one HashMap entry per
                // NAME. Unknown fields are ignored so a future field
                // addition can degrade gracefully on older binaries.
                let rest = other.trim_start_matches("accounts.");
                let Some((name, field)) = rest.split_once('.') else {
                    continue;
                };
                let entry = cfg.accounts.entry(name.to_string()).or_default();
                match field.trim() {
                    "role_arn" => entry.role_arn = value,
                    "source_profile" => entry.source_profile = Some(value),
                    "external_id" => entry.external_id = Some(value),
                    "region" => entry.region = Some(value),
                    _ => {}
                }
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
/// Atomic — writes to a sibling `.tmp` and renames into place so a
/// crash mid-write can't truncate `config.toml`.
pub fn save(cfg: &Config) -> std::io::Result<()> {
    let path = config_path();
    let body = serialize(cfg);
    crate::util::write_atomic(&path, &body)
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
    if !cfg.profile_themes.is_empty() {
        // Sort entries so repeated serialize cycles don't churn the file
        // when the HashMap iteration order shuffles.
        let mut pairs: Vec<(&String, &String)> = cfg.profile_themes.iter().collect();
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        let joined = pairs
            .iter()
            .map(|(k, v)| format!("{k}:{v}"))
            .collect::<Vec<_>>()
            .join(",");
        out.push_str(&format!("profile_themes = \"{joined}\"\n"));
    }
    if !cfg.runbooks.is_empty() {
        // Sorted so repeated serialize cycles don't churn the file.
        let mut pairs: Vec<(&String, &String)> = cfg.runbooks.iter().collect();
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        for (env, url) in pairs {
            out.push_str(&format!("runbooks.{env} = \"{url}\"\n"));
        }
    }
    out
}

/// Pure: parse a `prof:theme,prof:theme` string into a map. Empty / `:`
/// -free / blank-key / blank-value tokens are skipped. Whitespace around
/// each side of the colon is trimmed so the operator can format it for
/// readability.
pub fn parse_profile_themes(raw: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for token in raw.split(',') {
        let Some((k, v)) = token.split_once(':') else {
            continue;
        };
        let key = k.trim();
        let val = v.trim();
        if key.is_empty() || val.is_empty() {
            continue;
        }
        out.insert(key.to_string(), val.to_string());
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
    fn parse_profile_themes_happy_path() {
        let map = parse_profile_themes("prod:high-contrast,staging:dark,default:light");
        assert_eq!(map.get("prod"), Some(&"high-contrast".to_string()));
        assert_eq!(map.get("staging"), Some(&"dark".to_string()));
        assert_eq!(map.get("default"), Some(&"light".to_string()));
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn parse_profile_themes_trims_whitespace_and_skips_malformed() {
        // Trailing comma, missing colon, blank key, blank value all
        // produce no entries rather than panicking or yielding ""→"".
        let map = parse_profile_themes(
            "  prod : high-contrast , noseparator , :empty-key , empty-value: , ",
        );
        assert_eq!(map.get("prod"), Some(&"high-contrast".to_string()));
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn parse_profile_themes_empty_returns_empty_map() {
        assert!(parse_profile_themes("").is_empty());
    }

    #[test]
    fn parse_accounts_collects_multiline_specs() {
        let text = r#"
accounts.prod.role_arn = "arn:aws:iam::111122223333:role/EbmanReadOnly"
accounts.prod.source_profile = "default"
accounts.prod.region = "eu-west-2"
accounts.staging.role_arn = "arn:aws:iam::555555555555:role/EbmanReadOnly"
accounts.staging.external_id = "abc-xyz"
"#;
        let cfg = parse(text);
        assert_eq!(cfg.accounts.len(), 2);
        let prod = cfg.accounts.get("prod").expect("prod entry");
        assert_eq!(
            prod.role_arn,
            "arn:aws:iam::111122223333:role/EbmanReadOnly"
        );
        assert_eq!(prod.source_profile.as_deref(), Some("default"));
        assert_eq!(prod.region.as_deref(), Some("eu-west-2"));
        assert_eq!(prod.external_id, None);
        let staging = cfg.accounts.get("staging").expect("staging entry");
        assert_eq!(staging.external_id.as_deref(), Some("abc-xyz"));
        assert_eq!(staging.source_profile, None);
    }

    #[test]
    fn parse_accounts_ignores_unknown_field() {
        // Future-compat: a field we don't recognise should be ignored
        // rather than dropping the whole entry.
        let cfg = parse(
            "accounts.prod.role_arn = \"arn:…\"\n\
             accounts.prod.future_field = \"whatever\"\n",
        );
        let prod = cfg.accounts.get("prod").expect("prod entry");
        assert_eq!(prod.role_arn, "arn:…");
    }

    #[test]
    fn parse_runbooks_maps_env_to_url() {
        let cfg = parse(
            "runbooks.uflexi-prod = \"https://wiki/runbook/prod\"\n\
             runbooks.uflexi-staging = \"https://wiki/runbook/staging\"\n",
        );
        assert_eq!(cfg.runbooks.len(), 2);
        assert_eq!(
            cfg.runbooks.get("uflexi-prod").map(String::as_str),
            Some("https://wiki/runbook/prod")
        );
        // Blank URL is skipped — a stray `runbooks.x =` can't smuggle in
        // an empty mapping.
        assert!(parse("runbooks.x = \"\"\n").runbooks.is_empty());
    }

    #[test]
    fn runbooks_round_trip_through_serialize() {
        let mut cfg = Config::default();
        cfg.runbooks
            .insert("prod".into(), "https://rb/prod".into());
        let reparsed = parse(&serialize(&cfg));
        assert_eq!(
            reparsed.runbooks.get("prod").map(String::as_str),
            Some("https://rb/prod")
        );
    }

    #[test]
    fn parse_writes_profile_themes_into_config() {
        // End-to-end check: a config file with `profile_themes = "..."`
        // ends up in cfg.profile_themes correctly.
        let cfg = parse("profile_themes = \"prod:high-contrast,staging:dark\"\n");
        assert_eq!(cfg.profile_themes.len(), 2);
        assert_eq!(
            cfg.profile_themes.get("prod"),
            Some(&"high-contrast".to_string())
        );
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
        let mut profile_themes = std::collections::HashMap::new();
        profile_themes.insert("prod".into(), "high-contrast".into());
        profile_themes.insert("staging".into(), "dark".into());
        let cfg = Config {
            refresh_interval: Duration::from_secs(45),
            extra_regions: vec!["eu-south-2".into(), "ap-southeast-4".into()],
            redact_default: Some(true),
            grouped_default: Some(false),
            theme: "high-contrast".into(),
            icons: "powerline".into(),
            notify_bell: true,
            required_tags: vec!["Owner".into(), "Env".into()],
            webhook_url: Some("https://hooks.example/abc".into()),
            profile_themes,
            accounts: std::collections::HashMap::new(),
            runbooks: std::collections::HashMap::new(),
        };

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
        assert_eq!(reparsed.profile_themes, cfg.profile_themes);
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
