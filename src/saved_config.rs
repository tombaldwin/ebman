//! Parser for the EB CLI saved-config YAML format
//! (`.elasticbeanstalk/saved_configs/*.cfg.yml`). The on-disk shape:
//!
//! ```yaml
//! EnvironmentConfigurationMetadata:
//!   Description: ""
//! OptionSettings:
//!   aws:autoscaling:asg:
//!     MaxSize: '4'
//!     MinSize: '2'
//!   aws:elasticbeanstalk:application:environment:
//!     NODE_ENV: production
//! Platform:
//!   PlatformArn: "arn:aws:..."
//! Tags: null
//! ```
//!
//! We only consume `OptionSettings` — the rest is metadata the operator
//! doesn't typically want to diff against a deployed env. The two-level
//! nested map (namespace → option-name → value) flattens into a
//! `Vec<ConfigOption>` so existing `diff_config_options` can do the
//! comparison without a parallel code path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{eyre, Result, WrapErr};
use serde::Deserialize;

use crate::aws::ConfigOption;

#[derive(Debug, Deserialize)]
struct SavedConfigFile {
    #[serde(rename = "OptionSettings", default)]
    option_settings: BTreeMap<String, BTreeMap<String, serde_yml::Value>>,
}

/// Coerce a YAML scalar value into the string form EB stores option
/// settings as. EB's `DescribeConfigurationSettings` returns every
/// value as a string regardless of the underlying type, so we match
/// that representation here. Sequences and mappings (which shouldn't
/// occur inside `OptionSettings`) are serialised back to their YAML
/// form so the diff still shows *something* rather than silently
/// dropping the row.
fn coerce_value(v: &serde_yml::Value) -> String {
    coerce_value_at(v, 0)
}

/// How deep a nested value is rendered before it is abbreviated.
///
/// `OptionSettings` values are scalars in every saved config EB writes,
/// so anything nested is already off the expected shape. Four levels is
/// far past anything meaningful and keeps a hand-edited or truncated
/// file from recursing until the stack runs out.
const MAX_VALUE_DEPTH: usize = 4;

/// Render a saved-config value as the single line a diff row shows.
///
/// The non-scalar arms used to go through `serde_yml::to_string`, which
/// is the emitter named by RUSTSEC-2025-0068: `serde_yml`'s serializer
/// can segfault, the crate is archived, and `patched = []` means no
/// version fixes it. That was the codebase's only serializer use — the
/// other five call sites only parse, which the advisory does not
/// implicate — so writing these two arms by hand removes the unsound
/// path entirely rather than waiting on a replacement crate.
///
/// It also renders better. `to_string` emitted real YAML, so a sequence
/// became `- a\n- b` and got `trim`med into a row that still contained a
/// newline; a compact `[a, b]` is what a single-line diff cell wants.
fn coerce_value_at(v: &serde_yml::Value, depth: usize) -> String {
    use serde_yml::Value;
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Sequence(_) | Value::Mapping(_) | Value::Tagged(_) if depth >= MAX_VALUE_DEPTH => {
            "…".to_string()
        }
        Value::Sequence(items) => {
            let inner: Vec<String> = items
                .iter()
                .map(|i| coerce_value_at(i, depth + 1))
                .collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Mapping(map) => {
            let inner: Vec<String> = map
                .iter()
                // 0.0.13 hands out the key as a string rather than a
                // `Value` — one of the two breaking changes it shipped
                // inside a patch release.
                .map(|(k, val)| format!("{k}: {}", coerce_value_at(val, depth + 1)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        // `!Tag value` — YAML's enum syntax. The tag itself is not
        // reachable: `TaggedValue`'s field is private in 0.0.13 and it
        // exposes no accessor, so this renders the value alone. That is
        // a real (if small) loss of fidelity — two differently-tagged
        // values would diff as identical — and it is accepted because
        // EB writes plain scalars into `OptionSettings`, so a tagged
        // value cannot occur in a file we actually read. Revisit if the
        // YAML crate is ever replaced.
        Value::Tagged(_) => format!("{v:?}"),
    }
}

/// Parse a saved-config YAML blob into the same `Vec<ConfigOption>`
/// shape `aws::fetch_env_configuration_options` returns. Fields other
/// than `namespace` / `name` / `value` are blank — they're metadata
/// that the local YAML doesn't carry. The result feeds straight into
/// `diff_config_options` so the diff renderer doesn't need a parallel
/// code path.
pub(crate) fn parse_saved_config(yaml: &str) -> Result<Vec<ConfigOption>> {
    let parsed: SavedConfigFile =
        serde_yml::from_str(yaml).wrap_err("parsing saved-config YAML")?;
    let mut out = Vec::new();
    for (namespace, options) in parsed.option_settings {
        for (name, value) in options {
            let v = coerce_value(&value);
            out.push(ConfigOption {
                namespace: namespace.clone(),
                name,
                // Empty string normalises to "unset" inside
                // diff_config_options, matching EB's convention.
                value: Some(v),
                default_value: None,
                value_type: String::new(),
                value_options: Vec::new(),
                change_severity: None,
                user_defined: Some(true),
                min_value: None,
                max_value: None,
                max_length: None,
            });
        }
    }
    // Stable sort so renderers see consistent ordering.
    out.sort_by(|a, b| a.namespace.cmp(&b.namespace).then(a.name.cmp(&b.name)));
    Ok(out)
}

/// Discover saved-config files under `<cwd>/.elasticbeanstalk/saved_configs/`.
/// Returns absolute paths sorted alphabetically. Errors propagate (a
/// missing directory is *not* an error — returns an empty Vec — but a
/// permission failure on an existing directory does).
pub(crate) fn discover_saved_configs(cwd: &Path) -> Result<Vec<PathBuf>> {
    let dir = cwd.join(".elasticbeanstalk").join("saved_configs");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)
        .wrap_err_with(|| format!("reading saved-configs directory {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        // EB CLI writes `<name>.cfg.yml`; tolerate plain `.yml` too in
        // case someone hand-rolled.
        let is_yaml = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("yml") || e.eq_ignore_ascii_case("yaml"))
            .unwrap_or(false);
        if path.is_file() && is_yaml {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

/// Strip the `.cfg.yml` (or `.yml` / `.yaml`) suffix from a saved-config
/// path to get the operator-facing name. EB CLI's `eb config save NAME`
/// writes to `NAME.cfg.yml`, so the name is the file stem minus `.cfg`.
pub(crate) fn saved_config_name(path: &Path) -> String {
    let stem = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("(unnamed)");
    stem.trim_end_matches(".yaml")
        .trim_end_matches(".yml")
        .trim_end_matches(".cfg")
        .to_string()
}

/// Resolve a saved-config file by name + cwd. Used by `:config-diff-local
/// NAME`. Returns the path on hit, or an error listing what was
/// available so the operator can re-issue with the right name.
pub(crate) fn resolve_saved_config(cwd: &Path, name: &str) -> Result<PathBuf> {
    let configs = discover_saved_configs(cwd)?;
    if configs.is_empty() {
        return Err(eyre!(
            "no .elasticbeanstalk/saved_configs/*.cfg.yml under {}",
            cwd.display()
        ));
    }
    if let Some(p) = configs.iter().find(|p| saved_config_name(p) == name) {
        return Ok(p.clone());
    }
    let names: Vec<String> = configs.iter().map(|p| saved_config_name(p)).collect();
    Err(eyre!(
        "no saved config named '{name}' — found: {}",
        names.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
EnvironmentConfigurationMetadata:
  Description: my staging config
OptionSettings:
  aws:autoscaling:asg:
    MaxSize: '4'
    MinSize: '2'
  aws:elasticbeanstalk:application:environment:
    NODE_ENV: production
    LOG_LEVEL: info
Platform:
  PlatformArn: arn:aws:elasticbeanstalk:us-east-1::platform/Node.js
Tags: null
"#;

    #[test]
    fn parse_saved_config_extracts_option_settings_and_coerces_values() {
        // Numbers may parse as either strings (when quoted in the YAML)
        // or plain integers (when unquoted); coerce_value normalises
        // both to the EB-on-the-wire string form.
        let opts = parse_saved_config(SAMPLE).expect("parses");
        assert_eq!(opts.len(), 4, "expected 4 options, got {opts:?}");
        let max = opts
            .iter()
            .find(|o| o.namespace == "aws:autoscaling:asg" && o.name == "MaxSize")
            .expect("MaxSize present");
        assert_eq!(max.value.as_deref(), Some("4"));
        let node_env = opts
            .iter()
            .find(|o| o.name == "NODE_ENV")
            .expect("NODE_ENV present");
        assert_eq!(node_env.value.as_deref(), Some("production"));
    }

    #[test]
    fn parse_saved_config_handles_unquoted_numbers_and_booleans() {
        // EB CLI exports values quoted, but a hand-rolled file might
        // leave numbers / bools unquoted. The coerce path must still
        // produce string values matching what
        // DescribeConfigurationSettings would return.
        let yaml = r#"
OptionSettings:
  aws:autoscaling:asg:
    MinSize: 2
    EnableSpot: true
"#;
        let opts = parse_saved_config(yaml).expect("parses");
        let min = opts.iter().find(|o| o.name == "MinSize").unwrap();
        assert_eq!(min.value.as_deref(), Some("2"));
        let spot = opts.iter().find(|o| o.name == "EnableSpot").unwrap();
        assert_eq!(spot.value.as_deref(), Some("true"));
    }

    #[test]
    fn parse_saved_config_missing_option_settings_yields_empty() {
        // A YAML file with no `OptionSettings` key (only metadata) is
        // valid input — produces no rows but doesn't error.
        let yaml = r#"
EnvironmentConfigurationMetadata:
  Description: empty
"#;
        let opts = parse_saved_config(yaml).expect("parses");
        assert!(opts.is_empty());
    }

    #[test]
    fn parse_saved_config_rejects_garbage_yaml() {
        // `[` is an open sequence — invalid YAML at the top level.
        let err = parse_saved_config("[ unclosed").unwrap_err();
        assert!(
            err.to_string().contains("parsing saved-config YAML"),
            "expected parse error context, got: {err}"
        );
    }

    #[test]
    fn saved_config_name_strips_cfg_yml_suffix() {
        let name = saved_config_name(Path::new("/some/path/prod.cfg.yml"));
        assert_eq!(name, "prod");
        let name = saved_config_name(Path::new("plain.yaml"));
        assert_eq!(name, "plain");
        let name = saved_config_name(Path::new("staging.cfg.yaml"));
        assert_eq!(name, "staging");
    }

    #[test]
    fn discover_saved_configs_walks_the_eb_dir() {
        use std::io::Write;
        let tmp = tempdir().expect("tempdir");
        let saved_dir = tmp.path().join(".elasticbeanstalk").join("saved_configs");
        std::fs::create_dir_all(&saved_dir).unwrap();
        // Drop two .cfg.yml files plus a non-yaml one.
        let mut f = std::fs::File::create(saved_dir.join("a.cfg.yml")).unwrap();
        writeln!(f, "OptionSettings:").unwrap();
        let mut f = std::fs::File::create(saved_dir.join("b.cfg.yml")).unwrap();
        writeln!(f, "OptionSettings:").unwrap();
        // README.md must NOT be picked up.
        std::fs::write(saved_dir.join("README.md"), "ignore me").unwrap();
        let found = discover_saved_configs(tmp.path()).expect("discovers");
        assert_eq!(found.len(), 2, "expected 2 yml files, got {found:?}");
        // Sorted: a before b.
        assert!(found[0]
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("a"));
    }

    #[test]
    fn discover_saved_configs_returns_empty_when_missing() {
        let tmp = tempdir().expect("tempdir");
        // No `.elasticbeanstalk` directory at all.
        let found = discover_saved_configs(tmp.path()).expect("discovers");
        assert!(found.is_empty());
    }

    /// Tiny in-test tempdir helper so we don't pull in the `tempfile`
    /// crate just for two filesystem tests.
    fn tempdir() -> std::io::Result<TempDir> {
        let base = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = base.join(format!(
            "ebman-saved-cfg-test-{nanos}-{:?}",
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&path)?;
        Ok(TempDir { path })
    }

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod parser_properties {
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1000))]

        /// `parse_saved_config` reads `.elasticbeanstalk/saved_configs/*.cfg.yml`
        /// — files ebman does not write and does not control. It may
        /// return an error; it may not panic.
        #[test]
        fn parse_saved_config_never_panics(s in "(?s).{0,400}") {
            let _ = super::parse_saved_config(&s);
        }

        /// Near-miss YAML: the right keys at the wrong depth, which is
        /// what a hand-edited or half-migrated file looks like.
        #[test]
        fn parse_saved_config_survives_near_miss_yaml(
            ns in "[a-z:]{0,24}",
            name in "[A-Za-z]{0,20}",
            val in "[A-Za-z0-9 ]{0,30}",
        ) {
            let _ = super::parse_saved_config(&format!(
                "OptionSettings:\n  {ns}:\n    {name}: {val}\n"
            ));
            let _ = super::parse_saved_config(&format!("OptionSettings:\n  {ns}: {val}\n"));
            let _ = super::parse_saved_config(&format!("OptionSettings: {val}\n"));
        }
    }
}

#[cfg(test)]
mod coerce_tests {
    use super::parse_saved_config;

    fn value_for(yaml: &str) -> String {
        let opts = parse_saved_config(yaml).expect("parses");
        // `ConfigOption.value` is itself an `Option`; the parser always
        // sets `Some`, so a `None` here means the option was missing.
        opts.iter()
            .find(|o| o.name == "K")
            .and_then(|o| o.value.clone())
            .unwrap_or_else(|| panic!("no option K in {opts:?}"))
    }

    #[test]
    fn scalars_render_as_the_string_eb_would_return() {
        // DescribeConfigurationSettings gives every value back as a
        // string regardless of its underlying type, so the local file
        // has to be coerced the same way or every row diffs.
        assert_eq!(value_for("OptionSettings:\n  NS:\n    K: true\n"), "true");
        assert_eq!(value_for("OptionSettings:\n  NS:\n    K: 4\n"), "4");
        assert_eq!(value_for("OptionSettings:\n  NS:\n    K: '4'\n"), "4");
        assert_eq!(value_for("OptionSettings:\n  NS:\n    K: hello\n"), "hello");
        assert_eq!(value_for("OptionSettings:\n  NS:\n    K: null\n"), "");
    }

    #[test]
    fn a_sequence_renders_on_one_line() {
        // This arm used to go through `serde_yml::to_string`, the
        // emitter named by RUSTSEC-2025-0068. It also emitted real
        // YAML — `- a\n- b` — so a `trim` left a newline sitting inside
        // what is rendered as a single diff cell.
        let v = value_for("OptionSettings:\n  NS:\n    K:\n      - a\n      - b\n");
        assert_eq!(v, "[a, b]");
        assert!(!v.contains('\n'), "a diff cell is one line");
    }

    #[test]
    fn a_mapping_renders_on_one_line() {
        let v = value_for("OptionSettings:\n  NS:\n    K:\n      a: 1\n      b: 2\n");
        assert_eq!(v, "{a: 1, b: 2}");
        assert!(!v.contains('\n'));
    }

    #[test]
    fn nesting_is_bounded_rather_than_recursing_until_the_stack_ends() {
        // EB writes scalars, so anything this deep is a hand-edited or
        // truncated file. It must abbreviate, not recurse.
        let deep = "OptionSettings:\n  NS:\n    K:\n\
                    \x20     - - - - - - - - - - a\n";
        let v = value_for(deep);
        assert!(v.contains('…'), "deep nesting should abbreviate, got {v:?}");
        assert!(!v.contains('\n'));
    }

    #[test]
    fn an_empty_sequence_is_distinguishable_from_null() {
        // `[]` is not the same as "unset": rendering both as "" would
        // diff an emptied list as unchanged.
        assert_eq!(value_for("OptionSettings:\n  NS:\n    K: []\n"), "[]");
        assert_eq!(value_for("OptionSettings:\n  NS:\n    K: null\n"), "");
    }

    #[test]
    fn an_inline_empty_mapping_fails_to_parse_at_all() {
        // Not our behaviour — `serde_yml` 0.0.13 rejects `K: {}` in this
        // position with "expected identifier, found non-scalar", and the
        // whole file fails rather than that one value. `K: []` in the
        // same position is fine.
        //
        // Pinned because it is a real limitation of `:config-diff-local`
        // (a saved config containing an inline empty mapping cannot be
        // diffed at all), and because it is one more data point on the
        // crate that RUSTSEC-2025-0068 covers: this is the version that
        // shipped two breaking changes in a patch release. If the YAML
        // dependency is ever replaced, this test should start failing —
        // and that would be an improvement, not a regression.
        assert!(
            parse_saved_config("OptionSettings:\n  NS:\n    K: {}\n").is_err(),
            "serde_yml learned to parse an inline empty mapping — good; \
             update this test and the note in deny.toml"
        );
    }
}
