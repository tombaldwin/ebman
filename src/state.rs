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
    fn parse_skips_empty_and_unknown_keys() {
        let s = parse("");
        assert!(s.profile.is_none());
        let s = parse("# only comment\n  \nnonsense\n");
        assert!(s.profile.is_none());
        let s = parse("unknown = value\n");
        assert!(s.profile.is_none());
    }
}
