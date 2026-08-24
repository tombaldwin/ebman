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
//! [runbooks]
//! "uflexi-prod" = "https://wiki/runbooks/uflexi-prod"
//! ```
//!
//! Parsing uses `toml` + `serde` derive; unknown fields are accepted
//! (forward-compat) so an older binary doesn't choke on a newer
//! schema field. Both the inline `runbooks.NAME = "url"` and the
//! `[runbooks]` table syntax parse — they're the same wire format
//! to the `toml` crate.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Parsed contents of a `.ebman/ebman.toml`. Every field is optional;
/// `None` means "fall back to the user-level config / AWS env defaults".
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ProjectConfig {
    /// AWS profile name to use. Overrides `AWS_PROFILE` and the
    /// `profile` from `~/.config/ebman/config.toml`.
    #[serde(deserialize_with = "deserialize_non_empty", default)]
    pub profile: Option<String>,
    /// AWS region. Overrides `AWS_REGION` / `AWS_DEFAULT_REGION` and
    /// the `region` from `state.toml`.
    #[serde(deserialize_with = "deserialize_non_empty", default)]
    pub region: Option<String>,
    /// Application name to pre-filter to. Operators in a single-app
    /// repo set this so launching ebman from that repo immediately
    /// scopes the table to the right envs.
    #[serde(deserialize_with = "deserialize_non_empty", default)]
    pub application: Option<String>,
    /// Pre-fill the search filter. Useful when an app has many envs
    /// and the repo only deals with a subset (e.g. `prod-` to scope
    /// out staging / dev).
    #[serde(deserialize_with = "deserialize_non_empty", default)]
    pub filter: Option<String>,
    /// Per-env runbook URLs. Merged with the user-level
    /// `runbooks.ENV = …` map from `config.toml`; project entries win
    /// on collision because the repo is the more-specific source.
    pub runbooks: HashMap<String, String>,
    /// Lint rules disabled for this project. Merged with the
    /// user-level `lint.disable = "..."` list from `config.toml`;
    /// project disables extend (never override) the global set.
    /// Reading from `[lint]` table syntax: `disable = ["EBL011",
    /// "EBL004"]`.
    pub lint: LintProjectConfig,
}

/// Project-level lint config. Currently just the disable list;
/// severity overrides + per-env scoping can land here later.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct LintProjectConfig {
    pub disable: Vec<String>,
    /// Rules whose auto-fix is suppressed by `ebman lint --fix`,
    /// even when the rule itself is enabled for reporting.
    /// `[lint]\nfix_disable = ["EBL004"]` in `.ebman/ebman.toml`.
    pub fix_disable: Vec<String>,
}

/// `serde` adapter that collapses empty-string values to `None`. The
/// hand-rolled parser this replaced did the same so a stray
/// `profile = ""` couldn't mask the user-level default; preserved
/// here for back-compat with any project file that already uses it.
fn deserialize_non_empty<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(opt.filter(|s| !s.is_empty()))
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
/// Returns `None` on TOML syntax / schema errors so the caller can
/// fall back to defaults — a corrupt file shouldn't refuse to launch
/// ebman. NOT silent though: a type mismatch on one key (e.g.
/// `lint.disable = "EBL001"` instead of a list) discards the WHOLE
/// file, including committed profile/region pins — the operator must
/// hear about that (tracing in the TUI, stderr in CLI runs that
/// installed no subscriber wouldn't see it, so both).
pub fn parse(text: &str) -> Option<ProjectConfig> {
    match toml::from_str(text) {
        Ok(cfg) => Some(cfg),
        Err(e) => {
            tracing::warn!(error = %e, "project config (.ebman/ebman.toml) failed to parse — IGNORING the whole file");
            // CLI runs install no tracing subscriber, so the warning
            // would be invisible exactly where it matters (a dropped
            // profile/region pin changes which account a command hits).
            // The TUI must NOT eprintln (alternate screen), so this is
            // opt-in from main's subcommand dispatch.
            if WARN_TO_STDERR.load(std::sync::atomic::Ordering::Relaxed) {
                eprintln!("warning: .ebman/ebman.toml ignored — parse error: {e}");
            }
            None
        }
    }
}

/// See [`parse`]: CLI subcommand dispatch flips this so project-config
/// parse failures reach stderr. Never set from TUI code.
static WARN_TO_STDERR: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn warnings_to_stderr() {
    WARN_TO_STDERR.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Discover and load the project config starting from the current
/// working directory. Returns `None` when no `.ebman/` ancestor exists,
/// the file is unreadable, or the TOML is malformed. I/O errors and
/// parse errors are both swallowed — a corrupt file shouldn't refuse
/// to launch ebman, just silently fall back to user-level config.
pub fn load_from_cwd() -> Option<ProjectConfig> {
    let cwd = std::env::current_dir().ok()?;
    let root = find_root(&cwd)?;
    let path = config_path(&root);
    let text = std::fs::read_to_string(&path).ok()?;
    parse(&text)
}

/// Sugar for the `ebman lint` CLI: just the project-level
/// `lint.disable` list, no other fields. Returns an empty Vec
/// when no project config exists — the caller composes it with
/// the user-level disables.
pub fn load_lint_disables_from_cwd() -> Vec<String> {
    load_from_cwd().map(|c| c.lint.disable).unwrap_or_default()
}

/// Same as [`load_lint_disables_from_cwd`] but for the auto-fix
/// opt-out list. Composed by `ebman lint --fix` with the user-level
/// `lint.fix_disable`.
pub fn load_lint_fix_disables_from_cwd() -> Vec<String> {
    load_from_cwd()
        .map(|c| c.lint.fix_disable)
        .unwrap_or_default()
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
        let cfg = parse(text).expect("ok");
        assert_eq!(cfg.profile.as_deref(), Some("prod"));
        assert_eq!(cfg.region.as_deref(), Some("us-west-1"));
        assert_eq!(cfg.application.as_deref(), Some("uflexi"));
        assert_eq!(cfg.filter.as_deref(), Some("prod-"));
        assert!(cfg.runbooks.is_empty());
    }

    #[test]
    fn parse_runbooks_inline_dotted_keys() {
        let text = r#"
runbooks."uflexi-prod" = "https://wiki/uflexi-prod"
runbooks."uflexi-staging" = "https://wiki/uflexi-staging"
"#;
        let cfg = parse(text).expect("ok");
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
    fn parse_runbooks_table_syntax() {
        // The `[runbooks]` table form should parse too — `toml`
        // treats both as the same wire format.
        let text = r#"
[runbooks]
"uflexi-prod" = "https://wiki/uflexi-prod"
"#;
        let cfg = parse(text).expect("ok");
        assert_eq!(
            cfg.runbooks.get("uflexi-prod").map(String::as_str),
            Some("https://wiki/uflexi-prod"),
        );
    }

    #[test]
    fn parse_ignores_unknown_keys() {
        // Forward-compat: a future schema field shouldn't blow up
        // older binaries. `#[serde(default)]` at the struct level
        // means missing fields are fine; unknown fields are accepted.
        let text = r#"
future_setting = "whatever"
profile = "prod"
"#;
        let cfg = parse(text).expect("ok");
        assert_eq!(cfg.profile.as_deref(), Some("prod"));
    }

    #[test]
    fn parse_empty_returns_defaults() {
        let cfg = parse("").expect("ok");
        assert_eq!(cfg, ProjectConfig::default());
    }

    #[test]
    fn parse_skips_empty_string_values() {
        // `profile = ""` should yield `None`, not `Some("")` — otherwise
        // an empty override would mask the user-level setting.
        let cfg = parse("profile = \"\"\nregion = \"\"\n").expect("ok");
        assert_eq!(cfg.profile, None);
        assert_eq!(cfg.region, None);
    }

    #[test]
    fn parse_lint_fix_disable_table_collects_into_vec() {
        let text = r#"
[lint]
fix_disable = ["EBL004"]
"#;
        let cfg = parse(text).expect("parse ok");
        assert_eq!(cfg.lint.fix_disable, vec!["EBL004"]);
        assert!(cfg.lint.disable.is_empty());
    }

    #[test]
    fn parse_lint_disable_table_collects_into_vec() {
        // Project-local `.ebman/ebman.toml` uses native TOML arrays
        // for `lint.disable` (vs the CSV-in-string form used in the
        // hand-parsed `~/.config/ebman/config.toml`). The serde
        // deserialize handles the array form natively.
        let text = r#"
[lint]
disable = ["EBL001", "EBL003"]
"#;
        let cfg = parse(text).expect("parse ok");
        assert_eq!(cfg.lint.disable, vec!["EBL001", "EBL003"]);
    }

    #[test]
    fn parse_missing_lint_section_yields_empty_disable() {
        // Common case: no `[lint]` block at all. Default to empty.
        let text = r#"profile = "prod""#;
        let cfg = parse(text).expect("parse ok");
        assert!(cfg.lint.disable.is_empty());
    }

    #[test]
    fn parse_invalid_toml_returns_none() {
        // Malformed TOML → None so the caller falls back to defaults.
        assert_eq!(parse("profile = unquoted"), None);
        assert_eq!(parse("[unterminated"), None);
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

    /// `toml` 1.x implements TOML **spec 1.1.0** — a change to the
    /// language, not just to the crate's API. The bump compiled and the
    /// suite stayed green, which is weaker evidence than it looks: a
    /// spec change can only be observed through documents, and the
    /// other tests here each poke at one key.
    ///
    /// What makes the bump safe is narrow and worth pinning rather than
    /// re-deriving. TOML 1.1 is a widening — it accepts things 1.0
    /// rejected (newlines and trailing commas in inline tables, more
    /// escapes, unicode bare keys). Widening is only harmless in one
    /// direction, and the direction that matters is that every
    /// document an operator already has on disk still parses. There is
    /// no round-trip risk to go with it because `parse` is the only
    /// `toml::` call site in the crate and ebman never serialises TOML,
    /// so it cannot emit 1.1 syntax that an older ebman would reject.
    ///
    /// So: one document, plain 1.0, exercising every field of the
    /// schema at once — the case no single-key test covers.
    #[test]
    fn a_toml_1_0_document_using_the_whole_schema_still_parses() {
        let cfg = parse(
            r#"
profile = "prod"
region = "eu-west-1"
application = "checkout"
filter = "api"

[runbooks]
red = "https://runbooks.example.com/red"
deploy = "https://runbooks.example.com/deploy"

[lint]
disable = ["EBL001", "EBL010"]
fix_disable = ["EBL003"]
"#,
        )
        .expect("a valid TOML 1.0 document must still parse under spec 1.1");

        assert_eq!(cfg.profile.as_deref(), Some("prod"));
        assert_eq!(cfg.region.as_deref(), Some("eu-west-1"));
        assert_eq!(cfg.application.as_deref(), Some("checkout"));
        assert_eq!(cfg.filter.as_deref(), Some("api"));
        assert_eq!(
            cfg.runbooks.get("red").map(String::as_str),
            Some("https://runbooks.example.com/red")
        );
        assert_eq!(cfg.runbooks.len(), 2);
        assert_eq!(cfg.lint.disable, ["EBL001", "EBL010"]);
        assert_eq!(cfg.lint.fix_disable, ["EBL003"]);
    }

    /// The other half of the contract the bump could have moved: a
    /// document that is malformed under *both* specs still returns
    /// `None` rather than panicking or half-parsing, because the
    /// caller's fallback-to-defaults path depends on it.
    #[test]
    fn malformed_toml_still_returns_none_under_spec_1_1() {
        assert!(parse("profile = ").is_none());
        assert!(parse("[lint\ndisable = []").is_none());
    }
}
