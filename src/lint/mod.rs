//! Rule-based diagnostic engine. Drives three surfaces:
//!
//! 1. `:lint [ENV]` TUI overlay — operator-driven on-demand check.
//! 2. `ebman lint` CLI subcommand — scriptable for git hooks /
//!    CI / monitoring tools; emits JSON when `--json` is passed.
//! 3. Confirm-modal warning lines at write time — any rule that
//!    applies against the pre-write state surfaces inline so the
//!    operator sees risk before confirming.
//!
//! Rules are pure functions over a `LintContext` snapshot. Each
//! returns at most one `Issue` (or `None` if it doesn't fire on
//! the given env state). The engine is just a registry that runs
//! the enabled rules and collects the issues, sorted by severity
//! then by rule id for deterministic output.
//!
//! Tunable per-operator via `lint.disable = ["EBL011"]` lines in
//! `~/.config/ebman/config.toml` (global) and
//! `<repo>/.ebman/ebman.toml` (project-local). Project-local
//! disables win on collision — the repo is the more-specific
//! source. Same precedence rule the existing runbook / profile /
//! region overrides use.
//!
//! Designed for an eventual LLM integration: `Issue.detail`,
//! `Issue.suggestion`, and the structured `Issue.fields` map are
//! all explicit slots that a future `ebman explain ISSUE_ID`
//! command could feed to Claude API. The rule engine ships
//! 0.13; the LLM wire-up waits until there's demand.

use std::collections::BTreeMap;

/// Severity ladder. `Info` = nice-to-know, `Warn` = look at this,
/// `Error` = will bite you. CI tooling typically gates at Warn or
/// above (`--severity warn` is the common flag). The `:lint`
/// overlay colours by severity (muted / yellow / red).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Severity {
    Info,
    Warn,
    Error,
}

impl Severity {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warn => "warn",
            Severity::Error => "error",
        }
    }

    /// Parse from CLI `--severity` flag values. Tolerant of case
    /// and the `error` / `err` shorthand. Returns `None` for
    /// unrecognised values so the caller can surface a usage
    /// error rather than silently filter to nothing.
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "info" => Some(Severity::Info),
            "warn" | "warning" => Some(Severity::Warn),
            "error" | "err" => Some(Severity::Error),
            _ => None,
        }
    }
}

/// One operator-actionable finding from a rule. The shape is
/// deliberately structured (not free-text) so the same Issue
/// can render in the TUI overlay, emit as JSON for the CLI, AND
/// feed to a future LLM explainer without a separate format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Issue {
    /// Stable identifier (e.g. `"EBL001"`). Used by CI scripts
    /// to track / suppress specific rules; survives copy-edit
    /// to the title / detail text.
    pub rule_id: String,
    pub severity: Severity,
    /// Env name this issue applies to. `None` for fleet-wide
    /// rules (none ship in v1, but the slot exists).
    pub env_name: Option<String>,
    /// One-line operator-readable summary.
    pub title: String,
    /// Longer context — typically 1-3 sentences explaining WHY
    /// the rule fired and what specifically is wrong. Wrapped at
    /// render time; don't pre-wrap.
    pub detail: String,
    /// Concrete remediation hint, when one exists. Typically a
    /// command string the operator can run directly
    /// (`":deployment-policy Rolling"`). `None` when the fix is
    /// not a single command (e.g. "rebuild the AMI").
    pub suggestion: Option<String>,
    /// Machine-readable supplementary fields — used by the
    /// `--json` output and (future) the LLM explainer. Keys are
    /// rule-specific but should stay stable across releases so
    /// downstream consumers can rely on them.
    pub fields: BTreeMap<String, String>,
}

/// Snapshot of env state the rules check against. The caller
/// (TUI / CLI / confirm modal) assembles this from already-
/// fetched data; rules don't issue AWS calls themselves. Keeps
/// the engine deterministic + cheap to run many rules at once.
///
/// Use the [`LintContext::for_env`] constructor + the `.with_*`
/// builder methods so adding a new field doesn't require editing
/// every call site:
///
/// ```ignore
/// let ctx = LintContext::for_env(&env, &options)
///     .with_newer_stack_available(newer_version)
///     .with_required_tags(&required_tags)
///     .with_dlq_depth(depth);
/// let issues = run_rules(&rules, &ctx);
/// ```
#[derive(Debug, Clone)]
pub(crate) struct LintContext<'a> {
    pub env: &'a crate::aws::Environment,
    /// Operator-set option_settings, flat `(namespace, name, value)`.
    /// Matches the shape `fetch_env_option_settings` returns.
    pub options: &'a [(String, String, String)],
    /// Newer-platform-version available signal. `Some(version)` =
    /// the caller has checked `App.latest_stacks` and confirmed
    /// the env's family has a strictly-newer version (the value).
    /// `None` = either the data isn't loaded, the family is
    /// unknown, or the env is already current. EBL008 fires
    /// straight off the `Some` — no comparison in the rule
    /// (`aws::newer_stack_version` does the version-tuple math).
    ///
    /// Pre-0.17 this was named `latest_stack_version` and held
    /// "the latest version token" — but the rule then compared
    /// version token vs full stack name, false-positiving on
    /// every env. The 0.17 patch renamed the field + moved the
    /// comparison to the populated-by-caller `newer_stack_version`
    /// helper.
    pub newer_stack_available: Option<&'a str>,
    /// Required tag keys the operator declared in `config.toml`'s
    /// `required_tags` list. EBL010 checks the env's tag set
    /// against this. Empty slice means "no requirement declared"
    /// — the rule skips rather than firing on every env.
    pub required_tags: &'a [String],
    /// Env's actual tag keys (just the keys, not values), as fetched
    /// from EB's `ListTagsForResource`.
    ///
    /// `None` means "not loaded" — the rule skips. `Some(&[])` means
    /// the fetch SUCCEEDED and the env genuinely has no tags, which
    /// fires EBL010 for every required key.
    ///
    /// It was a bare slice, so those two states were the same value:
    /// a failed `ListTagsForResource` silently disabled the rule, and
    /// an env with no tags at all — the worst case the rule exists to
    /// catch — looked identical to one whose tags hadn't loaded. Same
    /// conflation as `describe_worker_queues` returning an empty list
    /// for AccessDenied, fixed in 0.27; the neighbouring `dlq_depth`
    /// and `healthy_instance_count` already use `Option` for it.
    pub env_tag_keys: Option<&'a [String]>,
    /// SQS dead-letter-queue depth for worker envs, when
    /// `:workers on` (or equivalent) has populated it. `None`
    /// means worker-tab data isn't loaded — the corresponding
    /// rule skips.
    pub dlq_depth: Option<i64>,
    /// Healthy instance count reported by EB's environment-health
    /// endpoint, when the workers/health tab has populated it.
    /// `None` means the data isn't loaded — the corresponding
    /// rule skips. `Some(0)` is the firing signal for EBL012.
    pub healthy_instance_count: Option<i64>,
    /// Result of the `xray:PutTraceSegments` IAM simulation against
    /// the env's instance-profile role, when the caller ran it
    /// (CLI-only today — `ebman lint` probes when `XRayEnabled` is
    /// true; the TUI sites leave it `None`, same pattern as the
    /// CLI's `dlq_depth`). `Some(true)` = simulation says denied —
    /// the EBL020 firing signal. `None` = not probed; rule skips.
    pub xray_trace_denied: Option<bool>,
    /// Failure reason from a live HTTP probe of the env's
    /// health-check URL, when the caller ran one (CLI-only, behind
    /// `ebman lint --probe-live` — one HTTP round-trip per env is
    /// too slow for default lint). `Some(reason)` = probe came back
    /// non-2xx / timed out / couldn't connect — the EBL016 firing
    /// signal. `None` = not probed or probe passed; rule skips.
    pub health_probe_failure: Option<&'a str>,
    /// Result of the WAF-association probe against the env's ALB,
    /// when the caller ran it (CLI-only — `ebman lint` probes
    /// prod-named envs with `LoadBalancerType=application`; see
    /// `probe_waf_missing`). `Some(true)` = the ALB has no WebACL
    /// associated — the EBL018 firing signal. `Some(false)` = WAF
    /// present. `None` = not probed (classic/network LB, non-prod
    /// name, probe error, or TUI path); rule skips.
    pub waf_missing: Option<bool>,
}

impl<'a> LintContext<'a> {
    /// Minimal constructor: an env + its option-settings. Other
    /// fields default to "not loaded" — rules that need them
    /// skip rather than false-positive. Use the `.with_*` chain
    /// to populate as data becomes available.
    pub(crate) fn for_env(
        env: &'a crate::aws::Environment,
        options: &'a [(String, String, String)],
    ) -> Self {
        Self {
            env,
            options,
            newer_stack_available: None,
            required_tags: &[],
            env_tag_keys: None,
            dlq_depth: None,
            healthy_instance_count: None,
            xray_trace_denied: None,
            health_probe_failure: None,
            waf_missing: None,
        }
    }

    /// Attach the "newer platform version available" signal —
    /// caller has already checked `App.latest_stacks` and
    /// determined a newer version exists. Enables EBL008 (stale
    /// platform). The string is the newer version token (e.g.
    /// "6.2.0") used in the issue body.
    pub(crate) fn with_newer_stack_available(mut self, newer_stack: &'a str) -> Self {
        self.newer_stack_available = Some(newer_stack);
        self
    }

    /// Attach the operator's `required_tags` declaration. Enables
    /// EBL010 (missing required tags) when paired with
    /// [`Self::with_env_tag_keys`].
    pub(crate) fn with_required_tags(mut self, required_tags: &'a [String]) -> Self {
        self.required_tags = required_tags;
        self
    }

    /// Attach the env's actual tag keys (just keys, not values).
    /// Paired with [`Self::with_required_tags`] to fire EBL010.
    pub(crate) fn with_env_tag_keys(mut self, env_tag_keys: &'a [String]) -> Self {
        self.env_tag_keys = Some(env_tag_keys);
        self
    }

    /// Attach SQS dead-letter-queue depth for worker envs. Enables
    /// EBL011 (worker DLQ stuck consumer).
    pub(crate) fn with_dlq_depth(mut self, dlq_depth: i64) -> Self {
        self.dlq_depth = Some(dlq_depth);
        self
    }

    /// Attach the healthy instance count from EB env health.
    /// Enables EBL012 (Green-but-0-instances divergence).
    pub(crate) fn with_healthy_count(mut self, healthy_instance_count: i64) -> Self {
        self.healthy_instance_count = Some(healthy_instance_count);
        self
    }

    /// Attach the result of an `xray:PutTraceSegments` IAM
    /// simulation against the env's instance-profile role. Enables
    /// EBL020 (X-Ray enabled but traces silently denied).
    pub(crate) fn with_xray_trace_denied(mut self, denied: bool) -> Self {
        self.xray_trace_denied = Some(denied);
        self
    }

    /// Attach a live health-check probe failure reason. Enables
    /// EBL016 (`--probe-live`); pass only when the probe FAILED —
    /// a passing probe leaves the field `None`.
    pub(crate) fn with_health_probe_failure(mut self, reason: &'a str) -> Self {
        self.health_probe_failure = Some(reason);
        self
    }

    /// Attach the WAF-association probe result for the env's ALB.
    /// Enables EBL018 (prod env without WAF).
    pub(crate) fn with_waf_missing(mut self, missing: bool) -> Self {
        self.waf_missing = Some(missing);
        self
    }
}

/// Soft prod-detection for EBL018: does the env name look like a
/// production environment? Case-insensitive substring match on
/// `prod` (covers `production`) or `prd`. Deliberately loose — the
/// per-env escape hatch is `lint.disable = ["EBL018"]`.
pub(crate) fn is_prod_named(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("prod") || lower.contains("prd")
}

/// A single diagnostic rule. Implementors are pure functions
/// over `LintContext`; `applies` returns `Some(Issue)` when the
/// rule fires for the given env, `None` otherwise.
///
/// Rule trait objects live in a static-built registry rather
/// than being dynamic-dispatched per-env — the operator's
/// `lint.disable` config filters AT REGISTRY-LOAD TIME, not
/// per-invocation, so a disabled rule has zero per-env cost.
pub(crate) trait Rule: Send + Sync {
    fn id(&self) -> &'static str;
    fn severity(&self) -> Severity;
    fn applies(&self, ctx: &LintContext) -> Option<Issue>;
    /// Optional auto-fix. Rules that have an obvious correct
    /// answer return `SetOption`; rules whose right fix depends
    /// on operator context (e.g. "what's your health-check
    /// path?") return `Manual` so the CLI can print instructions
    /// rather than guess wrong. Default `None` means "no fix
    /// available, even manual" — a rule for which the operator
    /// must reason about the architecture (e.g. EBL003 "env Red
    /// >4h" — that's a state, not a config issue).
    fn fix(&self, _ctx: &LintContext) -> Option<FixAction> {
        None
    }
}

/// What `ebman lint --fix` will do for an issue. The `description`
/// is operator-facing — printed in the `--dry-run` plan and used
/// as the audit-log narrative. Audit entries carry `rule_id` so
/// the operator can correlate `ebman audit --rule EBL001` to the
/// fix dispatches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FixAction {
    /// Set one option-setting. The 0.14 v1 shape; ~80% of
    /// auto-fixable rules collapse to this.
    SetOption {
        namespace: String,
        name: String,
        value: String,
        description: String,
    },
    /// The rule knows there's an issue and what to do about it,
    /// but the right value depends on operator context (e.g.
    /// EBL002 "set a health-check URL" — we don't know which
    /// path your app exposes). The `instructions` field is what
    /// the operator should do; `--fix` prints them and moves on.
    Manual { instructions: String },
}

/// Run every rule in `rules` against `ctx`; collect non-`None`
/// returns into a sorted vec (severity desc, then rule id asc).
/// Deterministic output ordering matters for CI diff workflows
/// — operators baseline against the lint output and a stable
/// order makes "what new issue showed up?" trivial.
pub(crate) fn run_rules(rules: &[Box<dyn Rule>], ctx: &LintContext) -> Vec<Issue> {
    let mut out: Vec<Issue> = rules.iter().filter_map(|r| r.applies(ctx)).collect();
    out.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.rule_id.cmp(&b.rule_id))
    });
    out
}

/// Render `issues` as JSON for the CLI `--json` output. Hand-
/// rolled rather than via `serde_json` — the shape is small and
/// stable, and avoiding the dep keeps `ebman lint --json` fast
/// to start. The same shape is what a future LLM explainer
/// would ingest.
/// The `ebman lint --json` report: the issues, plus whether the run was
/// degraded and why.
///
/// Separate from [`render_issues_json`] on purpose. That one writes the
/// `--baseline` snapshot, which `parse_baseline` reads back and which
/// has round-trip tests — adding a field there would change a file
/// format for the benefit of a different consumer.
///
/// The gap this closes: a probe that could not run (AccessDenied on
/// `iam:SimulatePrincipalPolicy`, say) makes the rule skip rather than
/// report a false positive, which is right — but it also meant a
/// degraded run and a clean run produced byte-identical JSON. The human
/// output distinguishes them; the machine output flattened it back,
/// which is exactly the distinction `ProbeOutcome::Unknown` exists to
/// preserve.
pub(crate) fn render_report_json(issues: &[Issue], degraded_reasons: &[String]) -> String {
    let issues_json = render_issues_json(issues);
    // `render_issues_json` returns `{"issues":[…]}`; splice the extra
    // fields in before the closing brace rather than rebuilding it.
    let trimmed = issues_json
        .strip_suffix('}')
        .unwrap_or(issues_json.as_str());
    let mut out = String::from(trimmed);
    out.push_str(",\"degraded\":");
    out.push_str(if degraded_reasons.is_empty() {
        "false"
    } else {
        "true"
    });
    out.push_str(",\"degraded_reasons\":[");
    for (i, r) in degraded_reasons.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(&json_escape(r));
        out.push('"');
    }
    out.push_str("]}");
    out
}

pub(crate) fn render_issues_json(issues: &[Issue]) -> String {
    let mut out = String::from("{\"issues\":[");
    for (i, issue) in issues.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('{');
        push_kv(&mut out, "rule_id", &issue.rule_id);
        out.push(',');
        push_kv(&mut out, "severity", issue.severity.as_str());
        out.push(',');
        if let Some(env) = &issue.env_name {
            push_kv(&mut out, "env", env);
            out.push(',');
        }
        push_kv(&mut out, "title", &issue.title);
        out.push(',');
        push_kv(&mut out, "detail", &issue.detail);
        if let Some(s) = &issue.suggestion {
            out.push(',');
            push_kv(&mut out, "suggestion", s);
        }
        if !issue.fields.is_empty() {
            out.push_str(",\"fields\":{");
            for (j, (k, v)) in issue.fields.iter().enumerate() {
                if j > 0 {
                    out.push(',');
                }
                push_kv(&mut out, k, v);
            }
            out.push('}');
        }
        out.push('}');
    }
    out.push_str("]}");
    out
}

/// Stable identity hash for an issue across runs. The identity is
/// `(rule_id, env_name, sorted_fields)` — title / detail / suggestion
/// can drift across releases without changing the underlying issue.
/// Used by `ebman lint --against-baseline` to diff today's issues
/// against a saved snapshot.
///
/// 16 hex chars (64 bits) is plenty for baseline-collision use —
/// operators won't hit birthday-attack-grade scales.
pub(crate) fn issue_identity_hash(
    rule_id: &str,
    env_name: Option<&str>,
    fields: &BTreeMap<String, String>,
) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(rule_id.as_bytes());
    hasher.update(b"\0");
    if let Some(env) = env_name {
        hasher.update(env.as_bytes());
    }
    hasher.update(b"\0");
    for (k, v) in fields {
        hasher.update(k.as_bytes());
        hasher.update(b"=");
        hasher.update(v.as_bytes());
        hasher.update(b"\0");
    }
    let digest = hasher.finalize();
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Convenience: `issue_identity_hash` against an `Issue` reference.
pub(crate) fn issue_identity(issue: &Issue) -> String {
    issue_identity_hash(&issue.rule_id, issue.env_name.as_deref(), &issue.fields)
}

/// Lightweight view of a baseline issue, parsed from
/// `render_issues_json` output. Carries just enough to identify the
/// issue and label "cleared" rows; full Issue reconstruction isn't
/// needed for the diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BaselineIssue {
    pub identity: String,
    pub rule_id: String,
    pub env_name: Option<String>,
    pub title: String,
}

/// Parse a baseline JSON file (the output of `ebman lint --baseline FILE`
/// or `ebman lint --json > FILE`). Returns the list of baseline
/// issues so callers can compute set differences against the current
/// run.
///
/// A JSON parser for JSON. This went through `serde_yml` on the "JSON
/// is a YAML subset, avoids a serde_json dep" reasoning — the dep was
/// already there, and the file is CI input from a previous run, so
/// every YAML feature applied to it. Same correction as the LLM and
/// tfstate parsers.
pub(crate) fn parse_baseline(text: &str) -> Result<Vec<BaselineIssue>, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("baseline JSON parse failed: {e}"))?;
    let issues = value
        .get("issues")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "baseline JSON missing `issues` array".to_string())?;
    let mut out = Vec::with_capacity(issues.len());
    for item in issues {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let rule_id = obj
            .get("rule_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "baseline issue missing rule_id".to_string())?
            .to_string();
        let env_name = obj.get("env").and_then(|v| v.as_str()).map(String::from);
        let title = obj
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let mut fields: BTreeMap<String, String> = BTreeMap::new();
        if let Some(f) = obj.get("fields").and_then(|v| v.as_object()) {
            // JSON object keys are `&str` by construction — no
            // scalar-key case to handle the way a YAML mapping needs.
            for (k, v) in f {
                if let Some(v_str) = v.as_str() {
                    fields.insert(k.to_string(), v_str.to_string());
                }
            }
        }
        let identity = issue_identity_hash(&rule_id, env_name.as_deref(), &fields);
        out.push(BaselineIssue {
            identity,
            rule_id,
            env_name,
            title,
        });
    }
    Ok(out)
}

fn push_kv(out: &mut String, k: &str, v: &str) {
    out.push('"');
    out.push_str(&json_escape(k));
    out.push_str("\":\"");
    out.push_str(&json_escape(v));
    out.push('"');
}

// JSON-escape for the `--json` issue output. Canonical helper lives
// in `crate::util`; re-routed locally for the existing `push_kv`
// call sites to keep them unchanged.
use crate::util::json_escape;

// ─── helpers ─────────────────────────────────────────────────

/// Look up an option-setting by namespace + name. Returns the
/// value, or empty string if absent. Centralised so rules don't
/// re-implement the lookup pattern.
pub(crate) fn option_value<'a>(
    options: &'a [(String, String, String)],
    namespace: &str,
    name: &str,
) -> &'a str {
    options
        .iter()
        .find(|(n, k, _)| n == namespace && k == name)
        .map(|(_, _, v)| v.as_str())
        .unwrap_or("")
}

fn parse_i32(s: &str) -> Option<i32> {
    s.trim().parse().ok()
}

// ─── v1 rules ────────────────────────────────────────────────

/// EBL001 — `AllAtOnce` deployment policy on a multi-instance
/// env. Causes 100% capacity loss during deploys, which is
/// almost never what an operator wants on production.
pub(crate) mod rules;
pub(crate) use rules::*;

#[cfg(test)]
mod tests;
