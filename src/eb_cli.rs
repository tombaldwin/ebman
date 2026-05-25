//! EB CLI compatibility: read `<repo>/.elasticbeanstalk/config.yml`
//! for default profile / region / application.
//!
//! Most EB CLI users already maintain this file; reading it lets
//! ebman pick up their working context without an explicit copy
//! step into `.ebman/ebman.toml`. The `.ebman/` file (when present)
//! still wins on collision — it's the more explicit, ebman-native
//! source.
//!
//! The format ebman cares about is a small subset of the EB CLI's
//! own schema:
//!
//! ```yaml
//! global:
//!   application_name: my-app
//!   default_region: us-east-1
//!   profile: my-aws-profile
//! ```
//!
//! A `branch-defaults:` block (default env per git branch) is
//! tolerated when present in real EB CLI files but not currently
//! read — ebman's table-filter UX is a different model from the
//! EB CLI's single-env default.
//!
//! Discovery walks the cwd's ancestors for the directory marker —
//! same shape as `project::find_root` but for `.elasticbeanstalk/`.
//! Parsing is best-effort: YAML errors / missing keys / unknown
//! keys never refuse to launch ebman; we fall back to defaults.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Subset of the EB CLI `config.yml` that ebman reads. Every field
/// is optional; unknown keys are ignored so an EB CLI schema bump
/// doesn't break us. Both `application_name` and `default_region`
/// match the EB CLI's own field names so YAML on disk needs no
/// rewriting.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct EbCliConfig {
    /// AWS profile to use. Maps to the EB CLI's `global.profile`.
    pub profile: Option<String>,
    /// AWS region. Maps to the EB CLI's `global.default_region`.
    pub region: Option<String>,
    /// Application name. Maps to the EB CLI's `global.application_name`.
    pub application: Option<String>,
}

/// Raw YAML shape — closer to the on-disk schema. We keep this
/// separate from the public `EbCliConfig` so consumers can build
/// against a small flat struct rather than the nested EB CLI tree.
#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    #[serde(default)]
    global: Option<RawGlobal>,
}

#[derive(Debug, Default, Deserialize)]
struct RawGlobal {
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    default_region: Option<String>,
    #[serde(default)]
    application_name: Option<String>,
}

/// Walk from `start` toward the filesystem root looking for an
/// `.elasticbeanstalk/` directory. Mirrors `project::find_root`.
pub fn find_root(start: &Path) -> Option<PathBuf> {
    for ancestor in start.ancestors() {
        if ancestor.join(".elasticbeanstalk").is_dir() {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

/// Path to the config file given a project root.
pub fn config_path(project_root: &Path) -> PathBuf {
    project_root.join(".elasticbeanstalk/config.yml")
}

/// Pure: parse a `config.yml` body into an `EbCliConfig`. Returns
/// `None` on YAML syntax errors so the caller can fall back
/// silently — a corrupt config shouldn't refuse to launch ebman.
/// Empty / `null` fields collapse to `None`.
pub fn parse(text: &str) -> Option<EbCliConfig> {
    let raw: RawConfig = serde_yml::from_str(text).ok()?;
    let global = raw.global.unwrap_or_default();
    Some(EbCliConfig {
        profile: global.profile.filter(|s| !s.is_empty()),
        region: global.default_region.filter(|s| !s.is_empty()),
        application: global.application_name.filter(|s| !s.is_empty()),
    })
}

/// Discover and load the EB CLI config starting from the current
/// working directory. Same swallowing-on-error contract as
/// `project::load_from_cwd`.
pub fn load_from_cwd() -> Option<EbCliConfig> {
    let cwd = std::env::current_dir().ok()?;
    let root = find_root(&cwd)?;
    let path = config_path(&root);
    let text = std::fs::read_to_string(&path).ok()?;
    parse(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_extracts_global_fields() {
        let body = "\
global:
  profile: my-aws-profile
  default_region: us-west-2
  application_name: my-app
";
        let cfg = parse(body).expect("parse ok");
        assert_eq!(cfg.profile.as_deref(), Some("my-aws-profile"));
        assert_eq!(cfg.region.as_deref(), Some("us-west-2"));
        assert_eq!(cfg.application.as_deref(), Some("my-app"));
    }

    #[test]
    fn parse_ignores_branch_defaults_block() {
        // EB CLI files often have a `branch-defaults` section we
        // don't currently use; the parser must not choke on it.
        let body = "\
branch-defaults:
  default:
    environment: prod-env-1
global:
  application_name: my-app
";
        let cfg = parse(body).expect("parse ok");
        assert_eq!(cfg.application.as_deref(), Some("my-app"));
        assert!(cfg.profile.is_none());
        assert!(cfg.region.is_none());
    }

    #[test]
    fn parse_empty_yaml_yields_all_none() {
        // EB CLI writes an empty config sometimes — should not panic.
        let cfg = parse("").expect("parse ok");
        assert_eq!(cfg, EbCliConfig::default());
    }

    #[test]
    fn parse_unknown_keys_are_ignored() {
        // Forward-compat: a newer EB CLI schema field shouldn't
        // crash our parse.
        let body = "\
global:
  profile: prod
  some_new_field: 42
  default_region: eu-west-1
new_top_level:
  foo: bar
";
        let cfg = parse(body).expect("parse ok");
        assert_eq!(cfg.profile.as_deref(), Some("prod"));
        assert_eq!(cfg.region.as_deref(), Some("eu-west-1"));
    }

    #[test]
    fn parse_null_and_empty_string_values_collapse_to_none() {
        // EB CLI writes `null` for unset keys; we treat both null
        // and empty string as "not set" so a stray `profile: ""`
        // doesn't mask the user's actual profile.
        let body = "\
global:
  profile: \"\"
  default_region: null
  application_name: my-app
";
        let cfg = parse(body).expect("parse ok");
        assert!(cfg.profile.is_none());
        assert!(cfg.region.is_none());
        assert_eq!(cfg.application.as_deref(), Some("my-app"));
    }

    #[test]
    fn parse_truly_malformed_yaml_returns_none() {
        // Unbalanced braces — every YAML driver should refuse.
        // Caller falls back to defaults silently; contract here is
        // "returns None rather than panicking or returning garbage".
        assert!(parse("global: { profile: prod, default_region:").is_none());
    }

    #[test]
    fn parse_type_mismatched_field_drops_to_none_for_that_field_or_whole() {
        // `default_region` declared as Option<String>; a nested
        // map in that slot is a deserialize error. Whether that
        // produces a whole-config None or a partial config depends
        // on the YAML driver — both are acceptable; "no panic" is
        // the real contract. The other fields, if any survive, must
        // still be readable through the public `parse` surface.
        let body = "global:\n  profile: prod\n  default_region:\n    nested: bad\n";
        let maybe = parse(body);
        if let Some(cfg) = maybe {
            // Driver accepted partial — profile must still come through.
            assert_eq!(cfg.profile.as_deref(), Some("prod"));
        }
    }

    #[test]
    fn find_root_walks_up_to_marker_directory() {
        // Same temp-dir pattern as `project::find_root`'s tests —
        // avoids pulling in a new dev dependency.
        let dir = std::env::temp_dir().join(format!("ebman-eb-cli-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let project = dir.join("project");
        let nested = project.join("nested/deeper");
        std::fs::create_dir_all(&nested).expect("mk nested");
        std::fs::create_dir_all(project.join(".elasticbeanstalk")).expect("mk marker");
        // From the nested dir, we walk up and find the project root.
        assert_eq!(find_root(&nested), Some(project.clone()));
        // From a sibling tree with no marker, returns None.
        let other = dir.join("other");
        std::fs::create_dir_all(&other).expect("mk other");
        assert_eq!(find_root(&other), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
