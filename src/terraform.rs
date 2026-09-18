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
pub(crate) struct TfEnv {
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
pub(crate) struct TfState {
    pub envs: Vec<TfEnv>,
    /// Terraform's own `serial`, incremented on every state write.
    ///
    /// Surfaced because a pulled state file goes stale silently: the
    /// drift report against a six-day-old `state.json` looks exactly
    /// like one against current state, and says the fleet matches
    /// intent when intent has moved. ebman cannot know whether this
    /// serial is the latest — it does not talk to the backend — so the
    /// honest thing is to show which one was compared and let the
    /// reader judge.
    pub serial: Option<u64>,
    /// Terraform's `lineage` — identifies the state's ancestry. A
    /// different lineage means a DIFFERENT state file, not an older
    /// one: pointing `terraform.state_path` at the wrong workspace
    /// produces a confident report about the wrong fleet.
    pub lineage: Option<String>,
}

impl TfState {
    /// Lookup by env name. Returns the FIRST match — duplicate
    /// `name` attributes across resources are theoretically
    /// possible but operationally meaningless (EB env names are
    /// unique per region anyway).
    pub(crate) fn env_by_name(&self, name: &str) -> Option<&TfEnv> {
        self.envs.iter().find(|e| e.name == name)
    }

    /// Set of tf-managed env names. Used by the table-render
    /// badge — `HashSet` lookup is O(1) per row, which matters
    /// when the operator has 50+ envs.
    pub(crate) fn managed_names(&self) -> std::collections::HashSet<String> {
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
    #[serde(default)]
    serial: Option<u64>,
    #[serde(default)]
    lineage: Option<String>,
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
    attributes: serde_json::Value,
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
pub(crate) fn find_tfstate(start: &Path) -> Option<PathBuf> {
    for ancestor in start.ancestors() {
        let backend = ancestor.join(".terraform").join("terraform.tfstate");
        if backend.is_file() && !file_is_backend_pointer(&backend) {
            return Some(backend);
        }
        let local = ancestor.join("terraform.tfstate");
        if local.is_file() {
            return Some(local);
        }
    }
    None
}

fn file_is_backend_pointer(path: &Path) -> bool {
    match std::fs::read_to_string(path) {
        Ok(text) => is_backend_pointer(&text),
        Err(_) => false,
    }
}

/// Apply the shared option-redaction policy to drift fields in
/// place: tf configs routinely pin env-var secrets, and a drifted
/// secret would otherwise print both its tf and live values. The
/// drifted/not-drifted signal survives. Used by the MCP drift tool,
/// `ebman drift` (opt out via `--no-redact`), and the TUI `:drift`
/// overlay (always on — the TUI has richer, deliberately-gated paths
/// for reading real values).
pub(crate) fn redact_drift_fields(fields: &mut [DriftField]) {
    for f in fields.iter_mut() {
        if f.kind != "option_setting" {
            continue;
        }
        let ns = f.namespace.as_deref().unwrap_or("");
        let name = f.name.as_deref().unwrap_or("");
        f.tf_value = crate::util::redact_option_value(ns, name, &f.tf_value, true);
        f.live_value = crate::util::redact_option_value(ns, name, &f.live_value, true);
    }
}

/// Since Terraform 0.9, `.terraform/terraform.tfstate` for a REMOTE
/// backend holds backend *configuration* (`{"backend": ...}`), not
/// resource state. `RawTfState`'s `#[serde(default)] resources` made
/// it parse "successfully" as zero envs — every env reported
/// not-tf-managed, drift badges vanished, and `ebman drift
/// --exit-code` passed green in CI: the exact silent failure this
/// module's docstring promises to avoid. It also SHADOWED a genuine
/// root-level terraform.tfstate when both existed. A pointer file is
/// one with a `backend` key and no resources; unparseable
/// backend-keyed files count as pointers too (skip, keep walking).
pub(crate) fn is_backend_pointer(text: &str) -> bool {
    if !text.contains("\"backend\"") {
        return false;
    }
    match serde_json::from_str::<RawTfState>(text) {
        Ok(raw) => raw.resources.is_empty(),
        Err(_) => true,
    }
}

/// Pure: parse tfstate JSON into a `TfState`. Returns `None` on
/// any parse error so the caller falls back silently — a
/// corrupt or non-tfstate JSON file at the discovery path
/// shouldn't refuse to launch ebman.
pub(crate) fn parse(text: &str) -> Option<TfState> {
    // A JSON parser for JSON: tfstate is JSON, and routing it through
    // a YAML parser meant YAML's anchor/alias expansion applied to a
    // file ebman discovers by walking up from cwd. `serde_json` is
    // already a direct dependency. The old note read:
    // the old note read: serde_yml handles JSON as a subset of YAML — no
    // need for a separate serde_json dep. Fast enough on 10MB
    // tfstates for an interactive operation.
    let raw: RawTfState = serde_json::from_str(text).ok()?;
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
    Some(TfState {
        envs,
        serial: raw.serial,
        lineage: raw.lineage,
    })
}

/// Pull the fields ebman compares from a single instance's
/// `attributes` blob. Tolerant of missing fields — anything we
/// can't extract falls back to the default value.
fn extract_env(attrs: &serde_json::Value) -> Option<TfEnv> {
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

fn extract_settings(v: Option<&serde_json::Value>) -> Vec<(String, String, String)> {
    let Some(arr) = v.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out: Vec<(String, String, String)> = Vec::with_capacity(arr.len());
    for entry in arr {
        // `from_value` clones — fine for a one-shot
        // parse, and avoids hand-rolling the field lookups.
        let raw: Result<RawSetting, _> = serde_json::from_value(entry.clone());
        if let Ok(s) = raw {
            out.push((s.namespace, s.name, s.value));
        }
    }
    out
}

fn extract_tags(v: Option<&serde_json::Value>) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let Some(map) = v.and_then(|v| v.as_object()) else {
        return out;
    };
    // JSON object keys are already `&str`; the YAML shape needed a
    // `k.as_str()` because a YAML key can be any scalar.
    for (k, val) in map {
        if let Some(v) = val.as_str() {
            out.insert(k.to_string(), v.to_string());
        }
    }
    out
}

/// Where a drift comparison's Terraform state came from, and how old
/// the file is.
///
/// A pulled `state.json` goes stale silently. The drift report against
/// a six-day-old file looks exactly like one against current state, and
/// says the fleet matches intent when intent has moved — the same shape
/// as a version string that is right when written and wrong afterwards.
///
/// ebman cannot tell whether `serial` is the LATEST: it reads state
/// files and does not talk to backends. So it shows which state was
/// compared and when the file was written, and leaves the judgement
/// where the information is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StateProvenance {
    pub serial: Option<u64>,
    pub lineage: Option<String>,
    /// The state FILE's mtime — when it was pulled, not when Terraform
    /// last wrote the state. For a human this is the more actionable of
    /// the two: "pulled six days ago" lands faster than a serial.
    pub pulled_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl StateProvenance {
    pub(crate) fn of(state: &TfState, path: Option<&Path>) -> Self {
        let pulled_at = path
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok())
            .map(chrono::DateTime::<chrono::Utc>::from);
        Self {
            serial: state.serial,
            lineage: state.lineage.clone(),
            pulled_at,
        }
    }
}

/// Where to read tfstate from, in precedence order.
///
/// Explicit path (flag or tool argument), then `terraform.state_path`
/// from config, then discovery by walking up from cwd.
///
/// The config rung is what makes `drift` usable at all on a fleet whose
/// state lives in a remote backend. Discovery only ever finds a LOCAL
/// file, so a team on HCP or S3 — which is most teams running this in
/// anger — had no drift, and the one incident that would have been
/// caught by it (a Terraform change silently blanking `JVM Options`,
/// a worker running in UTC for months) happened on exactly such a
/// fleet.
///
/// ebman deliberately does not talk to those backends and holds no
/// token for them: `terraform state pull > state.json` is one command,
/// works for every backend, and keeps credentials where they already
/// are.
pub(crate) fn resolve_state_path(
    explicit: Option<&Path>,
    configured: Option<&str>,
    start: &Path,
) -> Option<std::path::PathBuf> {
    if let Some(p) = explicit {
        return Some(p.to_path_buf());
    }
    if let Some(p) = configured.filter(|p| !p.is_empty()) {
        // Tilde-expanded: this value is hand-written in config.toml and
        // the documented example is `~/.config/ebman/poly.tfstate`.
        // Without this, following the documentation verbatim exits 2
        // with "could not read or parse tfstate at ~/…" — a config key
        // whose own example does not work.
        return Some(std::path::PathBuf::from(crate::app::expand_tilde(p)));
    }
    find_tfstate(start)
}

/// What to tell an operator when no tfstate could be found.
///
/// Names the remote-backend case explicitly. The previous message said
/// only "pass --tfstate", which is actionable if you HAVE a file and
/// useless if your state is in HCP — the reader is left thinking ebman
/// cannot do this, when one `terraform state pull` away it can.
pub(crate) fn no_state_hint(flag: &str) -> String {
    format!(
        "no terraform.tfstate found walking up from the current directory. \
         Pass {flag}, or set `terraform.state_path` in config.toml. If your \
         state is in a remote backend (HCP, S3, Consul), run `terraform \
         state pull > tfstate.json` and point at that — ebman reads state \
         files, it does not talk to backends."
    )
}

/// Discover and load tfstate from cwd. Returns `None` when no
/// tfstate ancestor exists, the file is unreadable, or the JSON
/// is malformed. Same swallowing contract as `project::load_from_cwd`.
pub(crate) fn load_from_cwd() -> Option<TfState> {
    let cwd = std::env::current_dir().ok()?;
    let path = find_tfstate(&cwd)?;
    let text = std::fs::read_to_string(&path).ok()?;
    parse(&text)
}

/// As above but takes an explicit `--tfstate PATH` override
/// (CLI flag). Skips discovery; reads the named file directly.
pub(crate) fn load_from_path(path: &Path) -> Option<TfState> {
    let text = std::fs::read_to_string(path).ok()?;
    parse(&text)
}

// ─── Drift detection ─────────────────────────────────────────

/// One difference between tf-declared intent and live EB state.
/// Each operator-actionable, structured so the CLI can emit it
/// as JSON and the TUI overlay can render it as a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DriftField {
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
pub(crate) fn compute_drift(
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
pub(crate) fn render_drift_json(
    tfstate_path: Option<&Path>,
    provenance: Option<&StateProvenance>,
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
    // Provenance next to the verdict, not appended after it: the
    // report reads as authoritative and this is what qualifies it.
    out.push_str(",\"state\":");
    match provenance {
        Some(p) => {
            out.push_str("{\"serial\":");
            match p.serial {
                Some(n) => out.push_str(&n.to_string()),
                None => out.push_str("null"),
            }
            out.push_str(",\"lineage\":");
            match &p.lineage {
                Some(l) => {
                    out.push('"');
                    push_escaped(&mut out, l);
                    out.push('"');
                }
                None => out.push_str("null"),
            }
            out.push_str(",\"pulled_at\":");
            match p.pulled_at {
                Some(t) => {
                    out.push('"');
                    push_escaped(&mut out, &t.to_rfc3339());
                    out.push('"');
                }
                None => out.push_str("null"),
            }
            out.push('}');
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
pub(crate) fn render_drift_text(env_name: &str, tf_managed: bool, drift: &[DriftField]) -> String {
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
        // An empty file is NOT valid JSON, so it is a parse failure
        // rather than a null document deserialising to
        // `resources: []` — which is what the YAML parser did before
        // 0.30, and what let an empty or truncated tfstate read as
        // "no envs" and pass `drift --exit-code` green. The caller
        // now reports "no terraform.tfstate found", which is a claim
        // an operator can act on. Same reasoning as the 0.27 fix for
        // backend pointers parsing as zero envs.
        assert!(parse("").is_none());
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
    fn find_tfstate_skips_remote_backend_pointer() {
        // Remote-backend projects: .terraform/terraform.tfstate is a
        // backend-config pointer with no resources — it must not
        // shadow the real root-level state (or, alone, produce a
        // zero-env "all clean" drift report).
        let dir = std::env::temp_dir().join(format!("ebman-tf-ptr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".terraform")).expect("mk .terraform");
        std::fs::write(
            dir.join(".terraform/terraform.tfstate"),
            r#"{"version": 3, "backend": {"type": "s3", "config": {}}}"#,
        )
        .expect("write pointer");
        let local = dir.join("terraform.tfstate");
        std::fs::write(&local, r#"{"resources": []}"#).expect("write local");
        assert_eq!(find_tfstate(&dir), Some(local));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backend_pointer_detection() {
        assert!(is_backend_pointer(
            r#"{"version": 3, "backend": {"type": "s3"}}"#
        ));
        assert!(!is_backend_pointer(r#"{"resources": [{"type": "x"}]}"#));
        // Real state that happens to mention backend but has resources.
        assert!(!is_backend_pointer(
            r#"{"backend": {"type": "s3"}, "resources": [{"type": "aws_elastic_beanstalk_environment", "instances": []}]}"#
        ));
        assert!(!is_backend_pointer("{}"));
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
        let json = render_drift_json(Some(Path::new("./terraform.tfstate")), None, &reports);
        // Round-trip through the YAML-superset parser to confirm
        // it's valid JSON.
        // A JSON parser, so "valid JSON" is what this actually
        // asserts — the YAML one accepted output JSON would reject.
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("rendered output must be valid JSON");
        // Spot-check fields.
        assert!(json.contains("\"tfstate\":\"./terraform.tfstate\""));
        assert!(json.contains("\"name\":\"prod-api\""));
        assert!(json.contains("\"tf_managed\":true"));
        assert!(json.contains("\"namespace\":\"aws:autoscaling:asg\""));
        assert!(json.contains("\"tf\":\"4\""));
        assert!(json.contains("\"live\":\"8\""));
        // The top-level structure is an object with envs array.
        assert!(parsed.is_object());
    }

    #[test]
    fn render_drift_json_null_path_when_no_tfstate_discovered() {
        let json = render_drift_json(None, None, &[]);
        assert!(json.contains("\"tfstate\":null"));
    }

    #[test]
    fn every_drift_surface_redacts_by_default() {
        // The three consumers of `compute_drift` — `ebman drift` (both
        // text and --json), the MCP drift tool, and the TUI `:drift`
        // overlay — must all pass through `redact_drift_fields`.
        // Redaction started MCP-only, and the gap meant a drifted
        // env-var secret landed in CI logs verbatim. This pins the
        // call sites so a fourth consumer can't quietly skip it.
        let sources = [
            ("cli/drift.rs", include_str!("cli/drift.rs")),
            ("cli/mcp/tools.rs", include_str!("cli/mcp/tools.rs")),
            ("app/cmd_misc.rs", include_str!("app/cmd_misc.rs")),
        ];
        for (name, src) in sources {
            assert!(
                src.contains("redact_drift_fields"),
                "{name} renders drift without redacting it"
            );
        }

        // And the function actually blanks a secret rather than just
        // being called.
        let mut fields = vec![
            super::DriftField {
                kind: "option_setting".into(),
                namespace: Some("aws:elasticbeanstalk:application:environment".into()),
                name: Some("DATABASE_URL".into()),
                tf_value: "postgres://user:hunter2@db/prod".into(),
                live_value: "postgres://user:hunter2@db/staging".into(),
            },
            super::DriftField {
                kind: "option_setting".into(),
                namespace: Some("aws:autoscaling:asg".into()),
                name: Some("MinSize".into()),
                tf_value: "2".into(),
                live_value: "4".into(),
            },
        ];
        super::redact_drift_fields(&mut fields);
        assert!(
            !fields[0].tf_value.contains("hunter2") && !fields[0].live_value.contains("hunter2"),
            "an env-var secret survived redaction: {:?}",
            fields[0]
        );
        assert_eq!(
            (fields[1].tf_value.as_str(), fields[1].live_value.as_str()),
            ("2", "4"),
            "a non-secret must stay readable — the drift signal is the point"
        );
    }
}

#[cfg(test)]
mod state_path_tests {
    use super::{no_state_hint, resolve_state_path};
    use std::path::{Path, PathBuf};

    /// Explicit beats config beats discovery.
    ///
    /// The config rung is the one that matters: discovery only ever
    /// finds a LOCAL file, so a fleet whose state is in a remote
    /// backend had no drift at all — and drift is the tool that would
    /// have caught a Terraform change silently blanking `JVM Options`
    /// on a live worker.
    #[test]
    fn the_state_path_precedence_is_explicit_then_config_then_discovery() {
        let explicit = PathBuf::from("/tmp/explicit.json");
        let nowhere = Path::new("/nonexistent-for-this-test");

        assert_eq!(
            resolve_state_path(Some(&explicit), Some("/tmp/configured.json"), nowhere),
            Some(explicit.clone()),
            "an explicit flag must win over config"
        );
        assert_eq!(
            resolve_state_path(None, Some("/tmp/configured.json"), nowhere),
            Some(PathBuf::from("/tmp/configured.json")),
            "config must be used when no flag is given"
        );
        assert_eq!(
            resolve_state_path(None, None, nowhere),
            None,
            "and with neither, discovery over a path with no tfstate finds nothing"
        );
        // An empty config value is not a path — it would resolve to ""
        // and fail with a confusing "could not parse tfstate at ''".
        assert_eq!(
            resolve_state_path(None, Some(""), nowhere),
            None,
            "an empty config value must fall through to discovery"
        );
    }

    /// The hint must name the remote-backend case.
    ///
    /// "pass --tfstate" is actionable if you have a file and useless if
    /// your state is in HCP — the reader concludes ebman cannot do this,
    /// when one `terraform state pull` away it can.
    #[test]
    fn the_no_state_hint_names_the_remote_backend_case() {
        let h = no_state_hint("--tfstate PATH");
        assert!(h.contains("--tfstate PATH"), "{h}");
        assert!(h.contains("terraform.state_path"), "{h}");
        assert!(
            h.contains("terraform state pull"),
            "the remote-backend workflow is the whole point of the hint: {h}"
        );
        assert!(
            h.contains("does not talk to backends"),
            "and it must be clear ebman reads files rather than fetching: {h}"
        );
    }

    /// The documented config example must actually work.
    ///
    /// `configuration.md` shows `terraform.state_path =
    /// "~/.config/ebman/poly.tfstate"`. Without expansion, following
    /// the documentation verbatim exits 2 with "could not read or parse
    /// tfstate at ~/…" — a config key whose own example fails.
    #[test]
    fn a_configured_state_path_expands_a_leading_tilde() {
        let nowhere = Path::new("/nonexistent-for-this-test");
        let resolved = resolve_state_path(None, Some("~/.config/ebman/poly.tfstate"), nowhere)
            .expect("the config rung resolves");
        assert!(
            !resolved.to_string_lossy().starts_with('~'),
            "a leading tilde must be expanded, not passed to the filesystem: {}",
            resolved.display()
        );
        assert!(
            resolved
                .to_string_lossy()
                .ends_with(".config/ebman/poly.tfstate"),
            "and the rest of the path must survive: {}",
            resolved.display()
        );

        // A path with no tilde is untouched — expansion must not
        // rewrite an absolute path someone deliberately gave.
        assert_eq!(
            resolve_state_path(None, Some("/srv/state.json"), nowhere),
            Some(PathBuf::from("/srv/state.json"))
        );
    }

    /// Discovery must be able to walk UP.
    ///
    /// `Path::new(".").ancestors()` yields exactly `"."` and `""` — two
    /// entries, neither of them a parent — so a relative start makes
    /// `find_tfstate` check one directory and stop, while every caller's
    /// documentation says it walks up. Pinned on the ancestor count
    /// rather than on a filesystem hit, so the test needs no fixture
    /// tree and still fails for a relative start.
    #[test]
    fn discovery_from_a_relative_start_cannot_walk_up() {
        let relative: Vec<_> = Path::new(".").ancestors().collect();
        assert_eq!(
            relative.len(),
            2,
            "a relative start has no parents to walk: {relative:?}"
        );

        let absolute = std::env::current_dir().expect("cwd");
        assert!(
            absolute.ancestors().count() > 2,
            "an absolute start does — which is why every caller must \
             canonicalise before discovery: {}",
            absolute.display()
        );
    }
}

#[cfg(test)]
mod provenance_tests {
    use super::{parse, render_drift_json, StateProvenance};

    const STATE: &str = r#"{
        "version": 4,
        "serial": 86,
        "lineage": "1f2e3d4c-5b6a-7890-abcd-ef1234567890",
        "resources": []
    }"#;

    /// `serial` and `lineage` must survive parsing.
    ///
    /// A pulled state file goes stale silently: the drift report against
    /// a six-day-old `state.json` is indistinguishable from one against
    /// current state, and says the fleet matches intent when intent has
    /// moved. ebman cannot know whether a serial is the latest — it
    /// reads files and does not talk to backends — so showing which one
    /// was compared is the honest maximum.
    #[test]
    fn the_state_identity_survives_parsing() {
        let st = parse(STATE).expect("valid tfstate");
        assert_eq!(st.serial, Some(86));
        assert_eq!(
            st.lineage.as_deref(),
            Some("1f2e3d4c-5b6a-7890-abcd-ef1234567890"),
            "lineage identifies a DIFFERENT state, not an older one — \
             pointing at the wrong workspace reports confidently on the \
             wrong fleet"
        );
    }

    /// A state file without them still parses. Older Terraform versions
    /// and hand-made fixtures omit both, and drift must not refuse to
    /// run over a missing provenance field.
    #[test]
    fn a_state_without_identity_still_parses() {
        let st = parse(r#"{"version":4,"resources":[]}"#).expect("valid");
        assert_eq!(st.serial, None);
        assert_eq!(st.lineage, None);
    }

    /// The drift report must CARRY the provenance, beside the verdict.
    #[test]
    fn the_drift_report_carries_the_state_it_compared() {
        let st = parse(STATE).expect("valid");
        // No path: `pulled_at` is unknowable without a file, and must
        // be null rather than invented.
        let prov = StateProvenance::of(&st, None);
        assert_eq!(prov.serial, Some(86));
        assert_eq!(prov.pulled_at, None);

        let json = render_drift_json(None, Some(&prov), &[]);
        assert!(json.contains("\"serial\":86"), "{json}");
        assert!(
            json.contains("1f2e3d4c-5b6a-7890-abcd-ef1234567890"),
            "{json}"
        );
        assert!(json.contains("\"pulled_at\":null"), "{json}");

        // And with no provenance at all the field is null, not absent —
        // a missing key and a null one read differently to a consumer.
        let bare = render_drift_json(None, None, &[]);
        assert!(bare.contains("\"state\":null"), "{bare}");
    }
}
