//! Project-level configuration: `<repo>/.ebman/ebman.toml`.
//!
//! Pinned to the repository, intended to be committed to git, so a
//! team shares the same env / profile / region preferences. Mirrors
//! the discovery pattern from the sibling `pgman` repo's
//! `.pgman/pgman.toml`.
//!
//! Walks up from the current directory looking for a `.ebman/`
//! folder so ebman can be launched from any subdirectory of the
//! project and pick up the right context.
//!
//! Schema (every field optional):
//! ```toml
//! # .ebman/ebman.toml — commit this. Passwords / credentials still
//! # come from ~/.aws/credentials, never this file.
//! profile = "prod"          # AWS profile to use
//! region  = "us-west-1"     # AWS region
//! application = "uflexi"    # filter envs to this app on launch
//! filter  = "prod-"         # pre-fill the search filter
//!
//! # Per-env runbook URLs surface in :why's triage overlay. Merged
//! # with the user-level `runbooks.ENV = …` entries from
//! # ~/.config/ebman/config.toml; project entries win on collision.
//! [runbooks]
//! uflexi-prod = "https://wiki/runbooks/uflexi-prod"
//! ```
//!
//! Pure parsing + lookup here; I/O is a thin wrapper at the bottom.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Parsed contents of a `.ebman/ebman.toml`. Every field is optional;
/// `None` means "fall back to the user-level config / AWS env defaults".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectConfig {
    /// AWS profile name to use. Overrides `AWS_PROFILE` and the
    /// `profile` from `~/.config/ebman/config.toml`.
    pub profile: Option<String>,
    /// AWS region. Overrides `AWS_REGION` / `AWS_DEFAULT_REGION` and
    /// the `region` from `state.toml`.
    pub region: Option<String>,
    /// Application name to pre-filter to. Operators in a single-app
    /// repo set this so launching ebman from that repo immediately
    /// scopes the table to the right envs.
    pub application: Option<String>,
    /// Pre-fill the search filter. Useful when an app has many envs
    /// and the repo only deals with a subset (e.g. `prod-` to scope
    /// out staging / dev).
    pub filter: Option<String>,
    /// Per-env runbook URLs. Merged with the user-level
    /// `runbooks.ENV = …` map from `config.toml`; project entries win
    /// on collision because the repo is the more-specific source.
    pub runbooks: HashMap<String, String>,
}

/// Walk from `start` toward the filesystem root looking for a
/// `.ebman/` directory. Returns the path *containing* the dir (i.e.
/// the project root). Mirrors the pgman discovery shape.
pub fn find_root(start: &Path) -> Option<PathBuf> {
    for ancestor in start.ancestors() {
        if ancestor.join(".ebman").is_dir() {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

/// Path to the project config file given a project root.
pub fn config_path(project_root: &Path) -> PathBuf {
    project_root.join(".ebman/ebman.toml")
}

/// Pure: parse a `.ebman/ebman.toml` body into a `ProjectConfig`.
/// Hand-rolled to match the existing `config.rs` / `state.rs` style —
/// swap to `serde` + `toml` once the broader TOML-parser migration on
/// the BACKLOG lands so all three files migrate together.
///
/// Tolerates: blank lines, `#` comments, surrounding `"` quotes on
/// values, leading / trailing whitespace. Unknown keys are ignored so
/// a future schema addition degrades gracefully on older binaries.
/// `runbooks.ENV = "url"` dotted-key form follows the same shape as
/// `config.toml`'s top-level `runbooks.ENV` lines.
pub fn parse(text: &str) -> ProjectConfig {
    let mut cfg = ProjectConfig::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            // Section headers are accepted-and-ignored (the dotted-key
            // `runbooks.ENV = …` shape doesn't need a `[runbooks]`
            // section, but TOML files written by hand often include
            // one and we shouldn't choke on it).
            continue;
        }
        let Some((key, raw_val)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = raw_val.trim().trim_matches('"').to_string();
        match key {
            "profile" => cfg.profile = non_empty(value),
            "region" => cfg.region = non_empty(value),
            "application" => cfg.application = non_empty(value),
            "filter" => cfg.filter = non_empty(value),
            other if other.starts_with("runbooks.") => {
                let env = other.trim_start_matches("runbooks.").trim();
                if !env.is_empty() && !value.is_empty() {
                    cfg.runbooks.insert(env.to_string(), value);
                }
            }
            _ => {}
        }
    }
    cfg
}

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Discover and load the project config starting from the current
/// working directory. Returns `None` when no `.ebman/` ancestor exists
/// or the file is unreadable / blank. I/O errors swallowed silently —
/// a corrupt file shouldn't refuse to launch ebman.
pub fn load_from_cwd() -> Option<ProjectConfig> {
    let cwd = std::env::current_dir().ok()?;
    let root = find_root(&cwd)?;
    let path = config_path(&root);
    let text = std::fs::read_to_string(&path).ok()?;
    Some(parse(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_extracts_known_keys() {
        let text = r#"
# A comment
profile = "prod"
region = "us-west-1"
application = "uflexi"
filter = "prod-"
"#;
        let cfg = parse(text);
        assert_eq!(cfg.profile.as_deref(), Some("prod"));
        assert_eq!(cfg.region.as_deref(), Some("us-west-1"));
        assert_eq!(cfg.application.as_deref(), Some("uflexi"));
        assert_eq!(cfg.filter.as_deref(), Some("prod-"));
        assert!(cfg.runbooks.is_empty());
    }

    #[test]
    fn parse_runbooks_dotted_keys() {
        let text = r#"
[runbooks]
runbooks.uflexi-prod = "https://wiki/uflexi-prod"
runbooks.uflexi-staging = "https://wiki/uflexi-staging"
"#;
        let cfg = parse(text);
        assert_eq!(
            cfg.runbooks.get("uflexi-prod").map(String::as_str),
            Some("https://wiki/uflexi-prod"),
        );
        assert_eq!(
            cfg.runbooks.get("uflexi-staging").map(String::as_str),
            Some("https://wiki/uflexi-staging"),
        );
    }

    #[test]
    fn parse_ignores_unknown_keys_and_blanks() {
        // Unknown keys + comments + blank lines + section headers
        // mustn't break parsing. Forward-compat: a future schema field
        // shouldn't blow up older binaries.
        let text = r#"
# header comment

future_setting = "whatever"
[unused_section]
profile = "prod"
"#;
        let cfg = parse(text);
        assert_eq!(cfg.profile.as_deref(), Some("prod"));
    }

    #[test]
    fn parse_empty_returns_defaults() {
        let cfg = parse("");
        assert_eq!(cfg, ProjectConfig::default());
    }

    #[test]
    fn parse_skips_empty_string_values() {
        // `profile = ""` should yield `None`, not `Some("")` — otherwise
        // an empty override would mask the user-level setting.
        let cfg = parse("profile = \"\"\nregion = \"\"\n");
        assert_eq!(cfg.profile, None);
        assert_eq!(cfg.region, None);
    }

    #[test]
    fn find_root_walks_up_to_project_marker() {
        let dir = std::env::temp_dir().join(format!("ebman-project-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let project = dir.join("project");
        let sub_a = project.join("a");
        let sub_b = sub_a.join("b");
        std::fs::create_dir_all(&sub_b).expect("mk subdirs");
        std::fs::create_dir_all(project.join(".ebman")).expect("mk .ebman marker");
        // From either subdirectory, find_root resolves to `project`.
        assert_eq!(find_root(&sub_b), Some(project.clone()));
        assert_eq!(find_root(&sub_a), Some(project.clone()));
        // From a sibling tree with no .ebman dir, returns None.
        let other = dir.join("other");
        std::fs::create_dir_all(&other).expect("mk other");
        assert_eq!(find_root(&other), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
