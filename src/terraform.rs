//! Terraform integration: discover `terraform.tfstate`, parse it
//! for `aws_elastic_beanstalk_environment` resources, and compute
//! drift between the operator's tf-declared intent and the live
//! EB state. Drives three surfaces in 0.13:
//!
//! 1. `ⓣ` badge in the env-name column for tf-managed envs.
//! 2. `:drift` TUI overlay — one-shot drift report.
//! 3. `ebman drift` CLI subcommand — scriptable for CI gates
//!    (`ebman drift --exit-code` fails the pipeline if drift
//!    detected before a `terraform apply`).
//!
//! Reads tfstate JSON directly (no shell-out to the `terraform`
//! binary needed). That's the resolved state, no init/auth
//! required. Walks `resources[*].instances[*].attributes`
//! filtering on `type == "aws_elastic_beanstalk_environment"`.
//!
//! Remote backends (S3 / Terraform Cloud / etc.) write a local
//! `.terraform/terraform.tfstate` after `terraform init`; we
//! read that. Operators on a fresh checkout without `init` get
//! a clear "no tfstate found" status — better than silently
//! reporting "no drift" against an empty state.
//!
//! Refresh: lazy read on `:drift` open + manual `R` keybind in
//! the overlay (TUI) / always re-read (CLI) — tfstate doesn't
//! change without a `terraform apply`, but the operator might
//! run that mid-session.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// One `aws_elastic_beanstalk_environment` resource extracted
/// from tfstate. Only the fields ebman compares against live
/// state are pulled out — the parser ignores everything else,
/// keeping it tolerant of tfstate schema additions in newer
/// Terraform versions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TfEnv {
    /// The `name` attribute — must match `Environment.name` in
    /// ebman's cached fleet for the env to be considered tf-
    /// managed. Case-sensitive.
    pub name: String,
    pub application: String,
    /// Empty string when tfstate doesn't pin a version (operator
    /// uses `aws_elastic_beanstalk_application_version` + a deploy
    /// pipeline). Drift detection skips version_label in that case
    /// to avoid false-positives.
    pub version_label: String,
    /// Operator-set option_settings only — NOT `all_settings`
    /// (which includes computed defaults). Same shape EB's
    /// `fetch_env_option_settings` returns.
    pub options: Vec<(String, String, String)>,
    /// Tag map.
    pub tags: std::collections::BTreeMap<String, String>,
}

/// Parsed tfstate, narrowed to the envs ebman cares about. Other
/// resource types (security groups, RDS, etc.) are walked past.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TfState {
    pub envs: Vec<TfEnv>,
}

impl TfState {
    /// Lookup by env name. Returns the FIRST match — duplicate
    /// `name` attributes across resources are theoretically
    /// possible but operationally meaningless (EB env names are
    /// unique per region anyway).
    pub fn env_by_name(&self, name: &str) -> Option<&TfEnv> {
        self.envs.iter().find(|e| e.name == name)
    }

    /// Set of tf-managed env names. Used by the table-render
    /// badge — `HashSet` lookup is O(1) per row, which matters
    /// when the operator has 50+ envs.
    pub fn managed_names(&self) -> std::collections::HashSet<String> {
        self.envs.iter().map(|e| e.name.clone()).collect()
    }
}

// ─── Raw deserialize shape ───────────────────────────────────
// Loose intermediate types matching the tfstate v4 JSON. We
// deserialise into these, then walk + extract into the public
// `TfEnv` shape.

#[derive(Deserialize)]
struct RawTfState {
    #[serde(default)]
    resources: Vec<RawResource>,
}

#[derive(Deserialize)]
struct RawResource {
    #[serde(rename = "type")]
    type_: String,
    #[serde(default)]
    instances: Vec<RawInstance>,
}

#[derive(Deserialize)]
struct RawInstance {
    #[serde(default)]
    attributes: serde_yml::Value,
}

#[derive(Deserialize)]
struct RawSetting {
    namespace: String,
    name: String,
    #[serde(default)]
    value: String,
}

// ─── Discovery ───────────────────────────────────────────────

/// Walk from `start` toward the filesystem root looking for a
/// tfstate file. Checks two paths per ancestor:
///
/// 1. `<dir>/.terraform/terraform.tfstate` — the post-`init`
///    location for projects using a remote backend.
/// 2. `<dir>/terraform.tfstate` — local backend or checked-in
///    state file.
///
/// Returns the FIRST match. Mirrors `project::find_root` and
/// `eb_cli::find_root` shape so the discovery story is the
/// same across .ebman/, .elasticbeanstalk/, and .terraform/.
pub fn find_tfstate(start: &Path) -> Option<PathBuf> {
    for ancestor in start.ancestors() {
        let backend = ancestor.join(".terraform").join("terraform.tfstate");
        if backend.is_file() {
            return Some(backend);
        }
        let local = ancestor.join("terraform.tfstate");
        if local.is_file() {
            return Some(local);
        }
    }
    None
}

/// Pure: parse tfstate JSON into a `TfState`. Returns `None` on
/// any parse error so the caller falls back silently — a
/// corrupt or non-tfstate JSON file at the discovery path
/// shouldn't refuse to launch ebman.
pub fn parse(text: &str) -> Option<TfState> {
    // serde_yml handles JSON as a strict subset of YAML — no
    // need for a separate serde_json dep. Fast enough on 10MB
    // tfstates for an interactive operation.
    let raw: RawTfState = serde_yml::from_str(text).ok()?;
    let mut envs: Vec<TfEnv> = Vec::new();
    for resource in raw.resources {
        if resource.type_ != "aws_elastic_beanstalk_environment" {
            continue;
        }
        for instance in resource.instances {
            if let Some(env) = extract_env(&instance.attributes) {
                envs.push(env);
            }
        }
    }
    Some(TfState { envs })
}

/// Pull the fields ebman compares from a single instance's
/// `attributes` blob. Tolerant of missing fields — anything we
/// can't extract falls back to the default value.
fn extract_env(attrs: &serde_yml::Value) -> Option<TfEnv> {
    let name = attrs.get("name")?.as_str()?.to_string();
    let application = attrs
        .get("application")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let version_label = attrs
        .get("version_label")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let options = extract_settings(attrs.get("setting"));
    let tags = extract_tags(attrs.get("tags"));
    Some(TfEnv {
        name,
        application,
        version_label,
        options,
        tags,
    })
}

fn extract_settings(v: Option<&serde_yml::Value>) -> Vec<(String, String, String)> {
    let Some(arr) = v.and_then(|v| v.as_sequence()) else {
        return Vec::new();
    };
    let mut out: Vec<(String, String, String)> = Vec::with_capacity(arr.len());
    for entry in arr {
        // serde_yml's `from_value` clones — fine for a one-shot
        // parse, and avoids hand-rolling the field lookups.
        let raw: Result<RawSetting, _> = serde_yml::from_value(entry.clone());
        if let Ok(s) = raw {
            out.push((s.namespace, s.name, s.value));
        }
    }
    out
}

fn extract_tags(v: Option<&serde_yml::Value>) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let Some(map) = v.and_then(|v| v.as_mapping()) else {
        return out;
    };
    for (k, val) in map {
        if let (Some(k), Some(v)) = (k.as_str(), val.as_str()) {
            out.insert(k.to_string(), v.to_string());
        }
    }
    out
}

/// Discover and load tfstate from cwd. Returns `None` when no
/// tfstate ancestor exists, the file is unreadable, or the JSON
/// is malformed. Same swallowing contract as `project::load_from_cwd`.
pub fn load_from_cwd() -> Option<TfState> {
    let cwd = std::env::current_dir().ok()?;
    let path = find_tfstate(&cwd)?;
    let text = std::fs::read_to_string(&path).ok()?;
    parse(&text)
}

/// As above but takes an explicit `--tfstate PATH` override
/// (CLI flag). Skips discovery; reads the named file directly.
pub fn load_from_path(path: &Path) -> Option<TfState> {
    let text = std::fs::read_to_string(path).ok()?;
    parse(&text)
}

// ─── Drift detection ─────────────────────────────────────────

/// One difference between tf-declared intent and live EB state.
/// Each operator-actionable, structured so the CLI can emit it
/// as JSON and the TUI overlay can render it as a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftField {
    /// Stable kind discriminator: `"version_label"`,
    /// `"option_setting"`, `"tag"`. Used by JSON consumers to
    /// route on category.
    pub kind: String,
    /// Set for `kind == "option_setting"`.
    pub namespace: Option<String>,
    /// Set for `kind == "option_setting"` (the option name) and
    /// for `kind == "tag"` (the tag key).
    pub name: Option<String>,
    pub tf_value: String,
    pub live_value: String,
}

/// Pure: compute drift between a tf-declared env and its live
/// EB counterpart. Returns the set of fields where the two
/// differ, in a stable order (version_label first, then
/// option_settings sorted by ns+name, then tags sorted by key)
/// so repeated calls produce identical output for CI diffs.
///
/// **Semantics:**
/// - `version_label`: skipped when tf doesn't pin one (empty
///   string) — operators using a deploy pipeline with
///   `aws_elastic_beanstalk_application_version` don't want
///   "drift" alerts every deploy.
/// - `option_settings`: compared only on (namespace, name)
///   pairs PRESENT IN TF. Live-only settings aren't drift —
///   they're either EB defaults or operator-set additions the
///   operator hasn't pinned in tf. (Future enhancement: a
///   `--strict` mode that flags both directions.)
/// - `tags`: same direction-aware semantics — tf is the
///   declared set; live tags absent from tf aren't drift.
pub fn compute_drift(
    tf: &TfEnv,
    live_env: &crate::aws::Environment,
    live_options: &[(String, String, String)],
) -> Vec<DriftField> {
    let mut out: Vec<DriftField> = Vec::new();

    // version_label
    if !tf.version_label.is_empty() && tf.version_label != live_env.version_label {
        out.push(DriftField {
            kind: "version_label".into(),
            namespace: None,
            name: None,
            tf_value: tf.version_label.clone(),
            live_value: live_env.version_label.clone(),
        });
    }

    // option_settings — only flag pairs tf pins. Live-only is
    // not drift.
    let mut option_drift: Vec<DriftField> = Vec::new();
    for (ns, name, tf_value) in &tf.options {
        let live_value = live_options
            .iter()
            .find(|(n, k, _)| n == ns && k == name)
            .map(|(_, _, v)| v.as_str())
            .unwrap_or("");
        if tf_value != live_value {
            option_drift.push(DriftField {
                kind: "option_setting".into(),
                namespace: Some(ns.clone()),
                name: Some(name.clone()),
                tf_value: tf_value.clone(),
                live_value: live_value.to_string(),
            });
        }
    }
    option_drift.sort_by(|a, b| {
        a.namespace
            .cmp(&b.namespace)
            .then_with(|| a.name.cmp(&b.name))
    });
    out.extend(option_drift);

    // Tags — same direction-aware semantics. Live-only tags
    // aren't flagged (could be EB-managed or third-party).
    // We don't currently have live tags on Environment; the
    // caller would need to fetch them via ListTagsForResource.
    // For now, tag drift detection is a no-op slot — the
    // structure is in place for a follow-on that adds the
    // tags fetch.
    //
    // (Intentional: shipping the version_label + option_settings
    // drift in this commit, layering tags in a follow-on keeps
    // each scope reviewable.)
    let _ = &tf.tags;

    out
}

/// Render a drift report as JSON for the `ebman drift --json`
/// CLI surface. Hand-rolled (no serde_json dep) — same approach
/// as `lint::render_issues_json`. Shape:
/// ```json
/// {"tfstate": "<path>", "envs": [
///   {"name": "prod-api", "tf_managed": true, "drift": [
///     {"kind": "option_setting", "namespace": "...", "name": "...",
///      "tf": "...", "live": "..."}
///   ]}
/// ]}
/// ```
pub fn render_drift_json(
    tfstate_path: Option<&Path>,
    reports: &[(String, bool, Vec<DriftField>)],
) -> String {
    let mut out = String::from("{");
    out.push_str("\"tfstate\":");
    match tfstate_path {
        Some(p) => {
            out.push('"');
            push_escaped(&mut out, &p.display().to_string());
            out.push('"');
        }
        None => out.push_str("null"),
    }
    out.push_str(",\"envs\":[");
    for (i, (env, tf_managed, drift)) in reports.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"name\":\"");
        push_escaped(&mut out, env);
        out.push_str("\",\"tf_managed\":");
        out.push_str(if *tf_managed { "true" } else { "false" });
        out.push_str(",\"drift\":[");
        for (j, field) in drift.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            out.push_str("{\"kind\":\"");
            push_escaped(&mut out, &field.kind);
            out.push('"');
            if let Some(ns) = &field.namespace {
                out.push_str(",\"namespace\":\"");
                push_escaped(&mut out, ns);
                out.push('"');
            }
            if let Some(name) = &field.name {
                out.push_str(",\"name\":\"");
                push_escaped(&mut out, name);
                out.push('"');
            }
            out.push_str(",\"tf\":\"");
            push_escaped(&mut out, &field.tf_value);
            out.push_str("\",\"live\":\"");
            push_escaped(&mut out, &field.live_value);
            out.push_str("\"}");
        }
        out.push_str("]}");
    }
    out.push_str("]}");
    out
}

fn push_escaped(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
}

/// Render a drift report as human-readable text — what the
/// TUI overlay shows and the CLI emits by default (without
/// `--json`). Stable column-formatted output an operator can
/// eyeball quickly.
pub fn render_drift_text(env_name: &str, tf_managed: bool, drift: &[DriftField]) -> String {
    if !tf_managed {
        return format!(
            "drift — {env_name}\n\n\
             Env is not managed by terraform (no matching resource in tfstate).\n\n\
             esc / q to close"
        );
    }
    if drift.is_empty() {
        return format!(
            "drift — {env_name}\n\n\
             ✓ No drift detected. Live state matches tfstate.\n\n\
             esc / q to close"
        );
    }
    let mut out = format!("drift — {env_name}\n\n");
    out.push_str(&format!(
        "{} drifted field{}:\n\n",
        drift.len(),
        if drift.len() == 1 { "" } else { "s" }
    ));
    for d in drift {
        match d.kind.as_str() {
            "version_label" => {
                out.push_str(&format!(
                    "version_label\n    tf:   {}\n    live: {}\n\n",
                    d.tf_value, d.live_value
                ));
            }
            "option_setting" => {
                let ns = d.namespace.as_deref().unwrap_or("?");
                let name = d.name.as_deref().unwrap_or("?");
                out.push_str(&format!(
                    "{ns}/{name}\n    tf:   {}\n    live: {}\n\n",
                    d.tf_value, d.live_value
                ));
            }
            "tag" => {
                let name = d.name.as_deref().unwrap_or("?");
                out.push_str(&format!(
                    "tag {name}\n    tf:   {}\n    live: {}\n\n",
                    d.tf_value, d.live_value
                ));
            }
            other => {
                out.push_str(&format!(
                    "{other}\n    tf:   {}\n    live: {}\n\n",
                    d.tf_value, d.live_value
                ));
            }
        }
    }
    out.push_str("esc / q to close");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aws::Environment;

    fn mk_env(name: &str, version_label: &str) -> Environment {
        Environment {
            name: name.into(),
            application: "shop".into(),
            status: "Ready".into(),
            health: "Green".into(),
            platform: "Java 17".into(),
            solution_stack: String::new(),
            tier: "Web".into(),
            cname: format!("{name}.example.com"),
            version_label: version_label.into(),
            arn: Some(format!("arn:aws:eb:us-east-1:0:env/{name}")),
            updated: None,
            id: None,
            region: None,
        }
    }

    const SAMPLE_TFSTATE: &str = r#"{
  "version": 4,
  "terraform_version": "1.5.0",
  "resources": [
    {
      "mode": "managed",
      "type": "aws_elastic_beanstalk_environment",
      "name": "prod_api",
      "provider": "provider[\"registry.terraform.io/hashicorp/aws\"]",
      "instances": [
        {
          "schema_version": 0,
          "attributes": {
            "name": "prod-api",
            "application": "shop",
            "version_label": "build-820",
            "cname": "prod-api.example.com",
            "tier": "WebServer",
            "setting": [
              {"namespace": "aws:autoscaling:asg", "name": "MinSize", "value": "2", "resource": ""},
              {"namespace": "aws:autoscaling:asg", "name": "MaxSize", "value": "4", "resource": ""},
              {"namespace": "aws:elasticbeanstalk:command", "name": "DeploymentPolicy", "value": "Rolling", "resource": ""}
            ],
            "tags": {"Owner": "ops", "Env": "prod"}
          }
        }
      ]
    },
    {
      "mode": "managed",
      "type": "aws_security_group",
      "name": "noisy_neighbour",
      "instances": [{"attributes": {"name": "sg-noise"}}]
    }
  ]
}"#;

    #[test]
    fn parse_extracts_eb_env_resources_only() {
        let state = parse(SAMPLE_TFSTATE).expect("parse ok");
        assert_eq!(state.envs.len(), 1);
        let env = &state.envs[0];
        assert_eq!(env.name, "prod-api");
        assert_eq!(env.application, "shop");
        assert_eq!(env.version_label, "build-820");
    }

    #[test]
    fn parse_pulls_option_settings_in_order() {
        let state = parse(SAMPLE_TFSTATE).expect("parse ok");
        let env = &state.envs[0];
        assert_eq!(env.options.len(), 3);
        assert_eq!(env.options[0].0, "aws:autoscaling:asg");
        assert_eq!(env.options[0].1, "MinSize");
        assert_eq!(env.options[0].2, "2");
    }

    #[test]
    fn parse_pulls_tags() {
        let state = parse(SAMPLE_TFSTATE).expect("parse ok");
        let env = &state.envs[0];
        assert_eq!(env.tags.get("Owner").map(String::as_str), Some("ops"));
        assert_eq!(env.tags.get("Env").map(String::as_str), Some("prod"));
    }

    #[test]
    fn parse_ignores_non_eb_resources() {
        // The sample includes an aws_security_group; we should
        // skip past it without parsing as an env.
        let state = parse(SAMPLE_TFSTATE).expect("parse ok");
        assert!(state.envs.iter().all(|e| e.name != "sg-noise"));
    }

    #[test]
    fn parse_empty_tfstate_is_empty_envs() {
        let state = parse(r#"{"version": 4, "resources": []}"#).expect("parse ok");
        assert!(state.envs.is_empty());
    }

    #[test]
    fn parse_malformed_returns_none() {
        assert!(parse("not json {").is_none());
        // Empty string is technically valid YAML (null document)
        // and serde_yml deserialises that into RawTfState with
        // `resources: []` via #[serde(default)]. We accept that
        // as "no envs" rather than refusing — a tfstate file
        // that's been truncated mid-flight reads the same way,
        // and "no envs" is the safe degraded behavior.
        assert!(parse("").is_some_and(|s| s.envs.is_empty()));
        // Bracket / brace mismatch: real syntax error → None.
        assert!(parse("{\"resources\": [").is_none());
    }

    #[test]
    fn parse_tolerates_unknown_attribute_fields() {
        // tfstate schema additions in newer Terraform shouldn't
        // break us. Only the fields we extract should matter.
        let text = r#"{
          "version": 4,
          "resources": [{
            "type": "aws_elastic_beanstalk_environment",
            "instances": [{
              "attributes": {
                "name": "future-env",
                "application": "shop",
                "version_label": "build-1",
                "setting": [],
                "tags": {},
                "future_field_42": "something",
                "yet_another": {"nested": [1, 2, 3]}
              }
            }]
          }]
        }"#;
        let state = parse(text).expect("parse ok");
        assert_eq!(state.envs.len(), 1);
        assert_eq!(state.envs[0].name, "future-env");
    }

    #[test]
    fn env_by_name_finds_match_or_returns_none() {
        let state = parse(SAMPLE_TFSTATE).expect("parse ok");
        assert!(state.env_by_name("prod-api").is_some());
        assert!(state.env_by_name("nope").is_none());
        // Case-sensitive — EB env names are exact-match.
        assert!(state.env_by_name("PROD-API").is_none());
    }

    #[test]
    fn managed_names_returns_set_for_o1_lookup() {
        let state = parse(SAMPLE_TFSTATE).expect("parse ok");
        let names = state.managed_names();
        assert!(names.contains("prod-api"));
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn compute_drift_no_drift_when_states_match() {
        let state = parse(SAMPLE_TFSTATE).expect("parse ok");
        let tf = state.env_by_name("prod-api").unwrap();
        let live = mk_env("prod-api", "build-820");
        let live_options = vec![
            ("aws:autoscaling:asg".into(), "MinSize".into(), "2".into()),
            ("aws:autoscaling:asg".into(), "MaxSize".into(), "4".into()),
            (
                "aws:elasticbeanstalk:command".into(),
                "DeploymentPolicy".into(),
                "Rolling".into(),
            ),
        ];
        let drift = compute_drift(tf, &live, &live_options);
        assert!(drift.is_empty());
    }

    #[test]
    fn compute_drift_detects_version_label_mismatch() {
        let state = parse(SAMPLE_TFSTATE).expect("parse ok");
        let tf = state.env_by_name("prod-api").unwrap();
        // Live env is on build-900; tf pins build-820.
        let live = mk_env("prod-api", "build-900");
        let live_options = vec![
            ("aws:autoscaling:asg".into(), "MinSize".into(), "2".into()),
            ("aws:autoscaling:asg".into(), "MaxSize".into(), "4".into()),
            (
                "aws:elasticbeanstalk:command".into(),
                "DeploymentPolicy".into(),
                "Rolling".into(),
            ),
        ];
        let drift = compute_drift(tf, &live, &live_options);
        assert_eq!(drift.len(), 1);
        assert_eq!(drift[0].kind, "version_label");
        assert_eq!(drift[0].tf_value, "build-820");
        assert_eq!(drift[0].live_value, "build-900");
    }

    #[test]
    fn compute_drift_skips_version_label_when_tf_unpins_it() {
        // Operator uses a deploy pipeline that owns the version;
        // tf doesn't pin one (empty string). Should NOT report
        // drift on every deploy.
        let tf = TfEnv {
            name: "prod-api".into(),
            application: "shop".into(),
            version_label: String::new(),
            options: Vec::new(),
            tags: Default::default(),
        };
        let live = mk_env("prod-api", "build-900");
        let drift = compute_drift(&tf, &live, &[]);
        assert!(drift.is_empty());
    }

    #[test]
    fn compute_drift_detects_option_setting_diff() {
        let state = parse(SAMPLE_TFSTATE).expect("parse ok");
        let tf = state.env_by_name("prod-api").unwrap();
        // Live MaxSize was bumped to 8; tf still says 4.
        let live = mk_env("prod-api", "build-820");
        let live_options = vec![
            ("aws:autoscaling:asg".into(), "MinSize".into(), "2".into()),
            ("aws:autoscaling:asg".into(), "MaxSize".into(), "8".into()),
            (
                "aws:elasticbeanstalk:command".into(),
                "DeploymentPolicy".into(),
                "Rolling".into(),
            ),
        ];
        let drift = compute_drift(tf, &live, &live_options);
        assert_eq!(drift.len(), 1);
        assert_eq!(drift[0].kind, "option_setting");
        assert_eq!(drift[0].namespace.as_deref(), Some("aws:autoscaling:asg"));
        assert_eq!(drift[0].name.as_deref(), Some("MaxSize"));
        assert_eq!(drift[0].tf_value, "4");
        assert_eq!(drift[0].live_value, "8");
    }

    #[test]
    fn compute_drift_ignores_live_only_settings() {
        // Live has an extra setting tf doesn't pin — not drift
        // (could be an EB default or operator-set addition).
        let tf = TfEnv {
            name: "prod-api".into(),
            application: "shop".into(),
            version_label: "build-820".into(),
            options: vec![("aws:autoscaling:asg".into(), "MaxSize".into(), "4".into())],
            tags: Default::default(),
        };
        let live = mk_env("prod-api", "build-820");
        let live_options = vec![
            ("aws:autoscaling:asg".into(), "MaxSize".into(), "4".into()),
            // Live-only: not in tf
            (
                "aws:elasticbeanstalk:command".into(),
                "DeploymentPolicy".into(),
                "Rolling".into(),
            ),
        ];
        let drift = compute_drift(&tf, &live, &live_options);
        assert!(drift.is_empty(), "live-only settings shouldn't be drift");
    }

    #[test]
    fn compute_drift_treats_missing_live_value_as_empty() {
        // tf pins a value live doesn't have at all. That's drift
        // (someone ran `terraform apply` then deleted the setting
        // via the EB console).
        let tf = TfEnv {
            name: "prod-api".into(),
            application: "shop".into(),
            version_label: String::new(),
            options: vec![(
                "aws:elasticbeanstalk:application".into(),
                "Application Healthcheck URL".into(),
                "/health".into(),
            )],
            tags: Default::default(),
        };
        let live = mk_env("prod-api", "");
        let drift = compute_drift(&tf, &live, &[]);
        assert_eq!(drift.len(), 1);
        assert_eq!(drift[0].live_value, "");
        assert_eq!(drift[0].tf_value, "/health");
    }

    #[test]
    fn compute_drift_sorts_option_drift_by_namespace_then_name() {
        // Multiple option drifts should sort deterministically
        // so CI diff workflows can baseline against the output.
        let tf = TfEnv {
            name: "prod-api".into(),
            application: "shop".into(),
            version_label: String::new(),
            options: vec![
                (
                    "aws:elasticbeanstalk:command".into(),
                    "BatchSize".into(),
                    "1".into(),
                ),
                ("aws:autoscaling:asg".into(), "MaxSize".into(), "4".into()),
                ("aws:autoscaling:asg".into(), "MinSize".into(), "2".into()),
            ],
            tags: Default::default(),
        };
        let live = mk_env("prod-api", "");
        let drift = compute_drift(&tf, &live, &[]);
        assert_eq!(drift.len(), 3);
        // Sort: aws:autoscaling:asg/MaxSize, aws:autoscaling:asg/MinSize,
        //       aws:elasticbeanstalk:command/BatchSize
        assert_eq!(drift[0].namespace.as_deref(), Some("aws:autoscaling:asg"));
        assert_eq!(drift[0].name.as_deref(), Some("MaxSize"));
        assert_eq!(drift[1].namespace.as_deref(), Some("aws:autoscaling:asg"));
        assert_eq!(drift[1].name.as_deref(), Some("MinSize"));
        assert_eq!(
            drift[2].namespace.as_deref(),
            Some("aws:elasticbeanstalk:command")
        );
    }

    #[test]
    fn find_tfstate_walks_up_to_terraform_dir() {
        let dir = std::env::temp_dir().join(format!("ebman-tf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let project = dir.join("project");
        let nested = project.join("a/b/c");
        std::fs::create_dir_all(&nested).expect("mk nested");
        let tf_dir = project.join(".terraform");
        std::fs::create_dir_all(&tf_dir).expect("mk .terraform");
        let tfstate = tf_dir.join("terraform.tfstate");
        std::fs::write(&tfstate, "{}").expect("write tfstate");
        // From any nested cwd, we should find the tfstate.
        assert_eq!(find_tfstate(&nested), Some(tfstate.clone()));
        assert_eq!(find_tfstate(&project), Some(tfstate));
        // From a sibling tree, nothing.
        let other = dir.join("other");
        std::fs::create_dir_all(&other).expect("mk other");
        assert_eq!(find_tfstate(&other), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_tfstate_prefers_dot_terraform_over_local_file() {
        // When BOTH `.terraform/terraform.tfstate` and a top-
        // level `terraform.tfstate` exist, prefer the .terraform/
        // one — that's the post-init location and matches what
        // `terraform plan` reads.
        let dir = std::env::temp_dir().join(format!("ebman-tf-pref-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".terraform")).expect("mk .terraform");
        let backend = dir.join(".terraform/terraform.tfstate");
        std::fs::write(&backend, "{}").expect("write");
        std::fs::write(dir.join("terraform.tfstate"), "{}").expect("write local");
        assert_eq!(find_tfstate(&dir), Some(backend));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_drift_text_clean_state_shows_check() {
        let body = render_drift_text("prod-api", true, &[]);
        assert!(body.contains("✓ No drift detected"));
        assert!(body.contains("prod-api"));
    }

    #[test]
    fn render_drift_text_non_managed_says_so() {
        let body = render_drift_text("loose-env", false, &[]);
        assert!(body.contains("not managed by terraform"));
    }

    #[test]
    fn render_drift_text_with_fields_groups_per_kind() {
        let drift = vec![
            DriftField {
                kind: "version_label".into(),
                namespace: None,
                name: None,
                tf_value: "build-820".into(),
                live_value: "build-900".into(),
            },
            DriftField {
                kind: "option_setting".into(),
                namespace: Some("aws:autoscaling:asg".into()),
                name: Some("MaxSize".into()),
                tf_value: "4".into(),
                live_value: "8".into(),
            },
        ];
        let body = render_drift_text("prod-api", true, &drift);
        assert!(body.contains("2 drifted fields"));
        assert!(body.contains("version_label"));
        assert!(body.contains("aws:autoscaling:asg/MaxSize"));
        assert!(body.contains("tf:   build-820"));
        assert!(body.contains("live: build-900"));
    }

    #[test]
    fn render_drift_json_emits_well_formed_structure() {
        let reports = vec![(
            "prod-api".to_string(),
            true,
            vec![DriftField {
                kind: "option_setting".into(),
                namespace: Some("aws:autoscaling:asg".into()),
                name: Some("MaxSize".into()),
                tf_value: "4".into(),
                live_value: "8".into(),
            }],
        )];
        let json = render_drift_json(Some(Path::new("./terraform.tfstate")), &reports);
        // Round-trip through the YAML-superset parser to confirm
        // it's valid JSON.
        let parsed: serde_yml::Value =
            serde_yml::from_str(&json).expect("rendered output must be valid JSON");
        // Spot-check fields.
        assert!(json.contains("\"tfstate\":\"./terraform.tfstate\""));
        assert!(json.contains("\"name\":\"prod-api\""));
        assert!(json.contains("\"tf_managed\":true"));
        assert!(json.contains("\"namespace\":\"aws:autoscaling:asg\""));
        assert!(json.contains("\"tf\":\"4\""));
        assert!(json.contains("\"live\":\"8\""));
        // The top-level structure is an object with envs array.
        assert!(parsed.is_mapping());
    }

    #[test]
    fn render_drift_json_null_path_when_no_tfstate_discovered() {
        let json = render_drift_json(None, &[]);
        assert!(json.contains("\"tfstate\":null"));
    }
}
