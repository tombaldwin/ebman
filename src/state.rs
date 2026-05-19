use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use crate::util::{config_file, parse_bool};

#[derive(Debug, Default, Clone)]
pub struct PersistedState {
    pub profile: Option<String>,
    pub region: Option<String>,
    pub filter: Option<String>,
    pub sort: Option<String>, // e.g. "app:asc", "health:desc"
    pub grouped: Option<bool>,
    pub redact: Option<bool>,
    pub events_visible: Option<bool>,
    pub selected_env: Option<String>,
    pub named_filters: BTreeMap<String, String>,
    pub pinned: BTreeSet<String>,
    pub aliases: BTreeMap<String, String>,
    pub saved_views: BTreeMap<String, String>,
    pub hidden_cols: BTreeSet<String>,
    /// User-defined extra metric charts for the Metrics tab. Keyed by the
    /// operator-chosen display label; value is `"namespace|name|stat"`.
    pub custom_metrics: BTreeMap<String, CustomMetricSpec>,
}

/// Parsed shape of a user-defined Metrics-tab chart. Stored line-oriented
/// in state.toml as `metric.LABEL = "namespace|name|stat"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomMetricSpec {
    pub namespace: String,
    pub name: String,
    pub stat: String,
}

impl CustomMetricSpec {
    /// Parse the `"namespace|name|stat"` form. Returns None on malformed
    /// input (wrong field count or empty parts) so the loader silently
    /// drops bad lines instead of aborting startup.
    pub fn parse(raw: &str) -> Option<Self> {
        let parts: Vec<&str> = raw.split('|').collect();
        if parts.len() != 3 {
            return None;
        }
        let ns = parts[0].trim();
        let name = parts[1].trim();
        let stat = parts[2].trim();
        if ns.is_empty() || name.is_empty() || stat.is_empty() {
            return None;
        }
        Some(Self {
            namespace: ns.into(),
            name: name.into(),
            stat: stat.into(),
        })
    }

    pub fn serialize(&self) -> String {
        format!("{}|{}|{}", self.namespace, self.name, self.stat)
    }
}

pub fn load() -> PersistedState {
    let path = state_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return PersistedState::default();
    };
    parse(&text)
}

pub fn parse(text: &str) -> PersistedState {
    let mut state = PersistedState::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, raw_val)) = line.split_once('=') else {
            continue;
        };
        let value = raw_val.trim().trim_matches('"').to_string();
        if value.is_empty() {
            continue;
        }
        let k = key.trim();
        match k {
            "profile" => state.profile = Some(value),
            "region" => state.region = Some(value),
            "filter" => state.filter = Some(value),
            "sort" => state.sort = Some(value),
            "grouped" => state.grouped = parse_bool(&value),
            "redact" => state.redact = parse_bool(&value),
            "events_visible" => state.events_visible = parse_bool(&value),
            "selected_env" => state.selected_env = Some(value),
            _ if k.starts_with("filter.") => {
                let name = k.trim_start_matches("filter.").trim().to_string();
                if !name.is_empty() {
                    state.named_filters.insert(name, value);
                }
            }
            "pinned" => {
                state.pinned = value
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            _ if k.starts_with("alias.") => {
                let name = k.trim_start_matches("alias.").trim().to_string();
                if !name.is_empty() {
                    state.aliases.insert(name, value);
                }
            }
            _ if k.starts_with("view.") => {
                let name = k.trim_start_matches("view.").trim().to_string();
                if !name.is_empty() {
                    state.saved_views.insert(name, value);
                }
            }
            _ if k.starts_with("metric.") => {
                let label = k.trim_start_matches("metric.").trim().to_string();
                if label.is_empty() {
                    continue;
                }
                if let Some(spec) = CustomMetricSpec::parse(&value) {
                    state.custom_metrics.insert(label, spec);
                }
            }
            "hidden_cols" => {
                state.hidden_cols = value
                    .split(',')
                    .map(|s| s.trim().to_uppercase())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            _ => {}
        }
    }
    state
}

pub fn save(state: &PersistedState) {
    let path = state_path();
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(error = %e, "failed to create state dir");
            return;
        }
    }
    let mut out = String::new();
    out.push_str("# ebman persisted state — managed by the app, edits will be overwritten\n");
    if let Some(p) = &state.profile {
        out.push_str(&format!("profile = \"{p}\"\n"));
    }
    if let Some(r) = &state.region {
        out.push_str(&format!("region = \"{r}\"\n"));
    }
    if let Some(f) = &state.filter {
        if !f.is_empty() {
            out.push_str(&format!("filter = \"{f}\"\n"));
        }
    }
    if let Some(s) = &state.sort {
        out.push_str(&format!("sort = \"{s}\"\n"));
    }
    if let Some(g) = state.grouped {
        out.push_str(&format!("grouped = {g}\n"));
    }
    if let Some(r) = state.redact {
        out.push_str(&format!("redact = {r}\n"));
    }
    if let Some(e) = state.events_visible {
        out.push_str(&format!("events_visible = {e}\n"));
    }
    if let Some(s) = &state.selected_env {
        out.push_str(&format!("selected_env = \"{s}\"\n"));
    }
    for (name, value) in &state.named_filters {
        out.push_str(&format!("filter.{name} = \"{value}\"\n"));
    }
    if !state.pinned.is_empty() {
        let joined: Vec<&str> = state.pinned.iter().map(String::as_str).collect();
        out.push_str(&format!("pinned = \"{}\"\n", joined.join(",")));
    }
    for (name, value) in &state.aliases {
        out.push_str(&format!("alias.{name} = \"{value}\"\n"));
    }
    for (name, value) in &state.saved_views {
        out.push_str(&format!("view.{name} = \"{value}\"\n"));
    }
    for (label, spec) in &state.custom_metrics {
        out.push_str(&format!("metric.{label} = \"{}\"\n", spec.serialize()));
    }
    if !state.hidden_cols.is_empty() {
        let joined: Vec<&str> = state.hidden_cols.iter().map(String::as_str).collect();
        out.push_str(&format!("hidden_cols = \"{}\"\n", joined.join(",")));
    }
    if let Err(e) = std::fs::write(&path, out) {
        tracing::warn!(error = %e, path = %path.display(), "failed to write state");
    }
}

fn state_path() -> PathBuf {
    config_file("state.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_fields() {
        let text = r#"
# comment
profile = "prod"
region = us-east-1
filter = "foo"
sort = "app:desc"
grouped = true
redact = off
events_visible = 1
selected_env = "my-env"
"#;
        let s = parse(text);
        assert_eq!(s.profile, Some("prod".into()));
        assert_eq!(s.region, Some("us-east-1".into()));
        assert_eq!(s.filter, Some("foo".into()));
        assert_eq!(s.sort, Some("app:desc".into()));
        assert_eq!(s.grouped, Some(true));
        assert_eq!(s.redact, Some(false));
        assert_eq!(s.events_visible, Some(true));
        assert_eq!(s.selected_env, Some("my-env".into()));
    }

    #[test]
    fn parse_named_filters() {
        let text = r#"
filter.dev = "production"
filter.prod = "live"
"#;
        let s = parse(text);
        assert_eq!(
            s.named_filters.get("dev").map(String::as_str),
            Some("production")
        );
        assert_eq!(
            s.named_filters.get("prod").map(String::as_str),
            Some("live")
        );
    }

    #[test]
    fn parse_collections() {
        let text = r#"
pinned = "prod-api,prod-worker"
alias.awseb-e-abc = "production"
alias.awseb-e-xyz = "staging"
view.dev = "filter=dev;sort=app:asc;grouped=false;scope=envs"
hidden_cols = "TREND,PLATFORM"
"#;
        let s = parse(text);
        assert!(s.pinned.contains("prod-api"));
        assert!(s.pinned.contains("prod-worker"));
        assert_eq!(
            s.aliases.get("awseb-e-abc").map(String::as_str),
            Some("production")
        );
        assert!(s.saved_views.contains_key("dev"));
        assert!(s.hidden_cols.contains("TREND"));
        assert!(s.hidden_cols.contains("PLATFORM"));
    }

    #[test]
    fn parse_custom_metrics() {
        let text = r#"
metric.cpu = "AWS/EC2|CPUUtilization|Average"
metric.disk = "AWS/EC2|DiskReadOps|Sum"
"#;
        let s = parse(text);
        let cpu = s.custom_metrics.get("cpu").expect("cpu metric");
        assert_eq!(cpu.namespace, "AWS/EC2");
        assert_eq!(cpu.name, "CPUUtilization");
        assert_eq!(cpu.stat, "Average");
        assert!(s.custom_metrics.contains_key("disk"));
    }

    #[test]
    fn parse_custom_metric_drops_malformed_value() {
        // Wrong field count: silently dropped, no panic.
        let text = "metric.bad = \"only|two\"\n";
        let s = parse(text);
        assert!(s.custom_metrics.is_empty());
        // Empty field: also dropped.
        let text = "metric.bad = \"AWS/EC2||Average\"\n";
        let s = parse(text);
        assert!(s.custom_metrics.is_empty());
    }

    #[test]
    fn custom_metric_spec_round_trips() {
        let spec = CustomMetricSpec {
            namespace: "AWS/ApplicationELB".into(),
            name: "RequestCount".into(),
            stat: "Sum".into(),
        };
        assert_eq!(
            CustomMetricSpec::parse(&spec.serialize()).as_ref(),
            Some(&spec)
        );
    }

    #[test]
    fn parse_skips_empty_and_unknown_keys() {
        let s = parse("");
        assert!(s.profile.is_none());
        let s = parse("# only comment\n  \nnonsense\n");
        assert!(s.profile.is_none());
        let s = parse("unknown = value\n");
        assert!(s.profile.is_none());
    }
}
