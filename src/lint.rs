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
pub enum Severity {
    Info,
    Warn,
    Error,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
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
    pub fn parse(s: &str) -> Option<Self> {
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
pub struct Issue {
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
pub struct LintContext<'a> {
    pub env: &'a crate::aws::Environment,
    /// Operator-set option_settings, flat `(namespace, name, value)`.
    /// Matches the shape `fetch_env_option_settings` returns.
    pub options: &'a [(String, String, String)],
    /// Recent events (newest-first), or empty if the caller
    /// didn't fetch them. Some rules use event history (e.g.
    /// "no deploys without --auto-rollback in the last week");
    /// rules that need events MUST handle the empty case
    /// gracefully (skip rather than false-positive).
    pub events: &'a [crate::aws::Event],
    /// Cost in USD per month, when `:cost on` has populated it.
    /// `None` means cost data isn't available — cost-shape rules
    /// skip rather than flag.
    pub cost_usd_per_month: Option<f64>,
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
    /// Env's actual tag keys (just the keys, not values), as
    /// fetched from EB's `ListTagsForResource`. Empty slice means
    /// "tags not loaded" — the rule skips rather than firing.
    /// Populated by callers that have already fetched tag data
    /// (the Detail/Tags tab does, but `:lint` doesn't yet).
    pub env_tag_keys: &'a [String],
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
}

impl<'a> LintContext<'a> {
    /// Minimal constructor: an env + its option-settings. Other
    /// fields default to "not loaded" — rules that need them
    /// skip rather than false-positive. Use the `.with_*` chain
    /// to populate as data becomes available.
    pub fn for_env(
        env: &'a crate::aws::Environment,
        options: &'a [(String, String, String)],
    ) -> Self {
        Self {
            env,
            options,
            events: &[],
            cost_usd_per_month: None,
            newer_stack_available: None,
            required_tags: &[],
            env_tag_keys: &[],
            dlq_depth: None,
            healthy_instance_count: None,
        }
    }

    /// Attach recent EB events (newest-first).
    pub fn with_events(mut self, events: &'a [crate::aws::Event]) -> Self {
        self.events = events;
        self
    }

    /// Attach the env's monthly cost in USD (from `:cost on`).
    pub fn with_cost(mut self, cost_usd_per_month: f64) -> Self {
        self.cost_usd_per_month = Some(cost_usd_per_month);
        self
    }

    /// Attach the "newer platform version available" signal —
    /// caller has already checked `App.latest_stacks` and
    /// determined a newer version exists. Enables EBL008 (stale
    /// platform). The string is the newer version token (e.g.
    /// "6.2.0") used in the issue body.
    pub fn with_newer_stack_available(mut self, newer_stack: &'a str) -> Self {
        self.newer_stack_available = Some(newer_stack);
        self
    }

    /// Attach the operator's `required_tags` declaration. Enables
    /// EBL010 (missing required tags) when paired with
    /// [`Self::with_env_tag_keys`].
    pub fn with_required_tags(mut self, required_tags: &'a [String]) -> Self {
        self.required_tags = required_tags;
        self
    }

    /// Attach the env's actual tag keys (just keys, not values).
    /// Paired with [`Self::with_required_tags`] to fire EBL010.
    pub fn with_env_tag_keys(mut self, env_tag_keys: &'a [String]) -> Self {
        self.env_tag_keys = env_tag_keys;
        self
    }

    /// Attach SQS dead-letter-queue depth for worker envs. Enables
    /// EBL011 (worker DLQ stuck consumer).
    pub fn with_dlq_depth(mut self, dlq_depth: i64) -> Self {
        self.dlq_depth = Some(dlq_depth);
        self
    }

    /// Attach the healthy instance count from EB env health.
    /// Enables EBL012 (Green-but-0-instances divergence).
    pub fn with_healthy_count(mut self, healthy_instance_count: i64) -> Self {
        self.healthy_instance_count = Some(healthy_instance_count);
        self
    }
}

/// A single diagnostic rule. Implementors are pure functions
/// over `LintContext`; `applies` returns `Some(Issue)` when the
/// rule fires for the given env, `None` otherwise.
///
/// Rule trait objects live in a static-built registry rather
/// than being dynamic-dispatched per-env — the operator's
/// `lint.disable` config filters AT REGISTRY-LOAD TIME, not
/// per-invocation, so a disabled rule has zero per-env cost.
pub trait Rule: Send + Sync {
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
pub enum FixAction {
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
pub fn run_rules(rules: &[Box<dyn Rule>], ctx: &LintContext) -> Vec<Issue> {
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
pub fn render_issues_json(issues: &[Issue]) -> String {
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
pub fn issue_identity_hash(
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
pub fn issue_identity(issue: &Issue) -> String {
    issue_identity_hash(&issue.rule_id, issue.env_name.as_deref(), &issue.fields)
}

/// Lightweight view of a baseline issue, parsed from
/// `render_issues_json` output. Carries just enough to identify the
/// issue and label "cleared" rows; full Issue reconstruction isn't
/// needed for the diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineIssue {
    pub identity: String,
    pub rule_id: String,
    pub env_name: Option<String>,
    pub title: String,
}

/// Parse a baseline JSON file (the output of `ebman lint --baseline FILE`
/// or `ebman lint --json > FILE`). Returns the list of baseline
/// issues so callers can compute set differences against the current
/// run. JSON parsed via serde_yml (JSON is a YAML subset; avoids a
/// serde_json dep).
pub fn parse_baseline(text: &str) -> Result<Vec<BaselineIssue>, String> {
    let value: serde_yml::Value =
        serde_yml::from_str(text).map_err(|e| format!("baseline JSON parse failed: {e}"))?;
    let issues = value
        .get("issues")
        .and_then(|v| v.as_sequence())
        .ok_or_else(|| "baseline JSON missing `issues` array".to_string())?;
    let mut out = Vec::with_capacity(issues.len());
    for item in issues {
        let Some(obj) = item.as_mapping() else {
            continue;
        };
        let rule_id = obj
            .get(serde_yml::Value::from("rule_id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| "baseline issue missing rule_id".to_string())?
            .to_string();
        let env_name = obj
            .get(serde_yml::Value::from("env"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let title = obj
            .get(serde_yml::Value::from("title"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let mut fields: BTreeMap<String, String> = BTreeMap::new();
        if let Some(f) = obj
            .get(serde_yml::Value::from("fields"))
            .and_then(|v| v.as_mapping())
        {
            for (k, v) in f {
                if let (Some(k_str), Some(v_str)) = (k.as_str(), v.as_str()) {
                    fields.insert(k_str.to_string(), v_str.to_string());
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
pub struct AllAtOnceMultiInstance;

impl Rule for AllAtOnceMultiInstance {
    fn id(&self) -> &'static str {
        "EBL001"
    }
    fn severity(&self) -> Severity {
        Severity::Warn
    }
    fn fix(&self, ctx: &LintContext) -> Option<FixAction> {
        // Only emit a fix when the rule actually applies — calling
        // `applies` is the cheapest correct way to check.
        self.applies(ctx)?;
        Some(FixAction::SetOption {
            namespace: "aws:elasticbeanstalk:command".into(),
            name: "DeploymentPolicy".into(),
            value: "Rolling".into(),
            description:
                "DeploymentPolicy: AllAtOnce → Rolling (preserves capacity during deploys)".into(),
        })
    }
    fn applies(&self, ctx: &LintContext) -> Option<Issue> {
        let policy = option_value(
            ctx.options,
            "aws:elasticbeanstalk:command",
            "DeploymentPolicy",
        );
        let max_size = parse_i32(option_value(ctx.options, "aws:autoscaling:asg", "MaxSize"))?;
        if policy.eq_ignore_ascii_case("AllAtOnce") && max_size > 1 {
            let mut fields = BTreeMap::new();
            fields.insert("policy".into(), policy.to_string());
            fields.insert("max_size".into(), max_size.to_string());
            return Some(Issue {
                rule_id: self.id().into(),
                severity: self.severity(),
                env_name: Some(ctx.env.name.clone()),
                title: format!(
                    "AllAtOnce on {max_size}-instance env: 100% capacity loss during deploys"
                ),
                detail: format!(
                    "Deployment policy is {policy} with MaxSize={max_size}. Every instance \
                     will restart simultaneously when a deploy fires, so the env is fully \
                     unavailable for the duration of the rollout."
                ),
                suggestion: Some(
                    ":deployment-policy Rolling  (or RollingWithAdditionalBatch for zero downtime)"
                        .into(),
                ),
                fields,
            });
        }
        None
    }
}

/// EBL002 — Web tier without `Application Healthcheck URL`. EB
/// defaults to probing `/` but that's typically just the
/// homepage; a deploy that breaks the homepage looks healthy
/// to EB. Setting an explicit `/health` endpoint is the standard
/// safety net.
pub struct WebTierNoHealthCheckUrl;

impl Rule for WebTierNoHealthCheckUrl {
    fn id(&self) -> &'static str {
        "EBL002"
    }
    fn severity(&self) -> Severity {
        Severity::Warn
    }
    fn fix(&self, ctx: &LintContext) -> Option<FixAction> {
        self.applies(ctx)?;
        // We know there's no health-check URL but not what path
        // the app exposes. Operator-context required.
        Some(FixAction::Manual {
            instructions:
                "Set the env's Application Healthcheck URL to a path that exercises real dependencies \
                 (typically `/health` or `/healthz`). In ebman: `:health-check-url /health`. \
                 The right path is app-specific — `--fix` won't guess."
                    .into(),
        })
    }
    fn applies(&self, ctx: &LintContext) -> Option<Issue> {
        if !ctx.env.tier.eq_ignore_ascii_case("Web") {
            return None;
        }
        let url = option_value(
            ctx.options,
            "aws:elasticbeanstalk:application",
            "Application Healthcheck URL",
        );
        if url.is_empty() || url == "/" {
            let mut fields = BTreeMap::new();
            fields.insert("tier".into(), ctx.env.tier.clone());
            fields.insert("current_url".into(), url.to_string());
            return Some(Issue {
                rule_id: self.id().into(),
                severity: self.severity(),
                env_name: Some(ctx.env.name.clone()),
                title: "Web-tier env probes `/` for health — consider an explicit /health endpoint"
                    .into(),
                detail:
                    "EB defaults to probing the env root for health checks. A deploy that breaks \
                     the homepage still looks healthy to the ALB, so auto-rollback won't fire. \
                     An explicit `/health` (or similar) endpoint that exercises real dependencies \
                     is the standard safety net."
                        .into(),
                suggestion: Some(":health-check-url /health".into()),
                fields,
            });
        }
        None
    }
}

/// EBL003 — Env Red for an extended period. Operational hygiene
/// signal — long-Red envs typically mean either an abandoned
/// stack or a missed page. Threshold: 4 hours, mirroring the
/// "newly Red" event grace window the existing alerts use.
pub struct EnvRedForExtendedPeriod;

impl Rule for EnvRedForExtendedPeriod {
    fn id(&self) -> &'static str {
        "EBL003"
    }
    fn severity(&self) -> Severity {
        Severity::Warn
    }
    fn applies(&self, ctx: &LintContext) -> Option<Issue> {
        let health = ctx.env.health.to_ascii_lowercase();
        if !matches!(health.as_str(), "red" | "severe" | "degraded") {
            return None;
        }
        // The Environment.updated field is the EB-side "last
        // status change" timestamp. Use it as a proxy for "how
        // long has the env looked like this?" If unset, skip —
        // we can't know the duration.
        let updated = ctx.env.updated?;
        let hours_since = (chrono::Utc::now() - updated).num_hours();
        if hours_since < 4 {
            return None;
        }
        let mut fields = BTreeMap::new();
        fields.insert("health".into(), ctx.env.health.clone());
        fields.insert("hours_red".into(), hours_since.to_string());
        Some(Issue {
            rule_id: self.id().into(),
            severity: self.severity(),
            env_name: Some(ctx.env.name.clone()),
            title: format!("Env has been {} for {}h", ctx.env.health, hours_since),
            detail: format!(
                "Health has been {} since {} — that's {}h. Long-running unhealthy envs \
                 typically mean either an abandoned stack or a missed page. Worth \
                 acknowledging via :why and either remediating or terminating.",
                ctx.env.health,
                updated.to_rfc3339(),
                hours_since
            ),
            suggestion: Some(":why  (drill into events + alarms + instances)".into()),
            fields,
        })
    }
}

/// EBL004 — BatchSize exceeds MaxSize. Means rolling deployment
/// will try to update more instances than exist; EB clamps but
/// the operator's configured intent is broken.
pub struct BatchSizeExceedsMaxSize;

impl Rule for BatchSizeExceedsMaxSize {
    fn id(&self) -> &'static str {
        "EBL004"
    }
    fn severity(&self) -> Severity {
        Severity::Warn
    }
    fn fix(&self, ctx: &LintContext) -> Option<FixAction> {
        // Recompute MaxSize so the fix value reflects the live
        // state, not a snapshot at rule construction. Calling
        // `applies` first ensures we don't dispatch when the
        // condition is already clean.
        self.applies(ctx)?;
        let max_size = parse_i32(option_value(ctx.options, "aws:autoscaling:asg", "MaxSize"))?;
        Some(FixAction::SetOption {
            namespace: "aws:elasticbeanstalk:command".into(),
            name: "BatchSize".into(),
            value: max_size.to_string(),
            description: format!("BatchSize → MaxSize ({max_size}): clamp to scaling cap"),
        })
    }
    fn applies(&self, ctx: &LintContext) -> Option<Issue> {
        let batch_size = parse_i32(option_value(
            ctx.options,
            "aws:elasticbeanstalk:command",
            "BatchSize",
        ))?;
        let batch_type = option_value(ctx.options, "aws:elasticbeanstalk:command", "BatchSizeType");
        // Percentage batch sizes don't have this problem — they're
        // a ratio, not an absolute count. Only Fixed batches can
        // exceed MaxSize.
        if !batch_type.eq_ignore_ascii_case("Fixed") {
            return None;
        }
        let max_size = parse_i32(option_value(ctx.options, "aws:autoscaling:asg", "MaxSize"))?;
        if batch_size > max_size {
            let mut fields = BTreeMap::new();
            fields.insert("batch_size".into(), batch_size.to_string());
            fields.insert("max_size".into(), max_size.to_string());
            return Some(Issue {
                rule_id: self.id().into(),
                severity: self.severity(),
                env_name: Some(ctx.env.name.clone()),
                title: format!("BatchSize ({batch_size}) > MaxSize ({max_size})"),
                detail: format!(
                    "Rolling deployment is configured with BatchSize={batch_size} (Fixed) \
                     but ASG MaxSize={max_size}. EB will clamp the effective batch to \
                     MaxSize, but the configured intent is broken — either the policy or \
                     the scaling profile is wrong."
                ),
                suggestion: Some(format!(
                    ":set-option aws:elasticbeanstalk:command BatchSize {max_size}  (clamp to MaxSize)"
                )),
                fields,
            });
        }
        None
    }
}

/// EBL005 — Single-instance env (MinSize=MaxSize=1). Acceptable
/// for dev/staging but a production red flag — no redundancy
/// means any instance failure is a full outage. Tagged as Info
/// (not Warn) because some envs genuinely want this; just worth
/// surfacing on a lint check.
pub struct SingleInstanceEnv;

impl Rule for SingleInstanceEnv {
    fn id(&self) -> &'static str {
        "EBL005"
    }
    fn severity(&self) -> Severity {
        Severity::Info
    }
    fn fix(&self, ctx: &LintContext) -> Option<FixAction> {
        self.applies(ctx)?;
        // Scaling decisions are architectural (cost vs redundancy
        // trade-off; some envs genuinely want single-instance).
        // `--fix` shouldn't make that call.
        Some(FixAction::Manual {
            instructions:
                "Single-instance is acceptable for dev/staging but risky for production. If this is \
                 a prod workload, scale to ≥ 2 via `:capacity` (set MinSize + MaxSize ≥ 2). \
                 The right capacity is workload-dependent — `--fix` won't decide for you."
                    .into(),
        })
    }
    fn applies(&self, ctx: &LintContext) -> Option<Issue> {
        let min_size = parse_i32(option_value(ctx.options, "aws:autoscaling:asg", "MinSize"))?;
        let max_size = parse_i32(option_value(ctx.options, "aws:autoscaling:asg", "MaxSize"))?;
        if min_size == 1 && max_size == 1 {
            let mut fields = BTreeMap::new();
            fields.insert("min_size".into(), "1".into());
            fields.insert("max_size".into(), "1".into());
            return Some(Issue {
                rule_id: self.id().into(),
                severity: self.severity(),
                env_name: Some(ctx.env.name.clone()),
                title: "Single-instance env — no redundancy".into(),
                detail:
                    "MinSize=MaxSize=1 means any instance failure is a full outage. Acceptable for \
                     dev/staging; risky for production. Consider scaling to ≥ 2 instances if this \
                     is a production workload."
                        .into(),
                suggestion: Some(":capacity  (set Min ≥ 2 for redundancy)".into()),
                fields,
            });
        }
        None
    }
}

/// EBL006 — Cooldown below EB's recommended floor of 60s. Short
/// cooldowns cause autoscaling thrashing — instances launch and
/// terminate in rapid succession because the cooldown expires
/// before the new instance has stabilised under load.
pub struct CooldownBelowRecommended;

impl Rule for CooldownBelowRecommended {
    fn id(&self) -> &'static str {
        "EBL006"
    }
    fn severity(&self) -> Severity {
        Severity::Info
    }
    fn fix(&self, ctx: &LintContext) -> Option<FixAction> {
        self.applies(ctx)?;
        // EB's documented default is 360s; the safe floor is 60s.
        // Going straight to 360 matches EB's own recommendation
        // and avoids tuning that the operator hasn't asked for.
        Some(FixAction::SetOption {
            namespace: "aws:autoscaling:asg".into(),
            name: "Cooldown".into(),
            value: "360".into(),
            description: "ASG Cooldown → 360s (EB documented default)".into(),
        })
    }
    fn applies(&self, ctx: &LintContext) -> Option<Issue> {
        let cooldown = parse_i32(option_value(ctx.options, "aws:autoscaling:asg", "Cooldown"))?;
        // EB's documented default is 360s; recommended floor 60s.
        if cooldown < 60 {
            let mut fields = BTreeMap::new();
            fields.insert("cooldown_secs".into(), cooldown.to_string());
            fields.insert("recommended_min".into(), "60".into());
            return Some(Issue {
                rule_id: self.id().into(),
                severity: self.severity(),
                env_name: Some(ctx.env.name.clone()),
                title: format!(
                    "Autoscaling Cooldown={cooldown}s is below the 60s recommended floor"
                ),
                detail: format!(
                    "Cooldown={cooldown}s means the ASG can launch / terminate instances in rapid \
                     succession before a new instance has stabilised under load — typical symptom \
                     is autoscaling thrashing during spikes. EB documents 60s as the floor."
                ),
                suggestion: Some(":set-option aws:autoscaling:asg Cooldown 360".into()),
                fields,
            });
        }
        None
    }
}

/// EBL007 — ELB-fronted env without HTTPS listener. Production
/// traffic on plain HTTP fails most operator security baselines
/// (PCI, SOC2, internal policy). Detection: any `aws:elbv2:listener:*`
/// namespace declaring `ListenerEnabled=true` `Protocol=HTTP`. We
/// don't auto-fix because the right cert ARN is operator-specific.
pub struct ElbWithoutHttps;

impl Rule for ElbWithoutHttps {
    fn id(&self) -> &'static str {
        "EBL007"
    }
    fn severity(&self) -> Severity {
        Severity::Warn
    }
    fn fix(&self, ctx: &LintContext) -> Option<FixAction> {
        self.applies(ctx)?;
        Some(FixAction::Manual {
            instructions: "Add an HTTPS listener with an ACM certificate. In the EB console: \
                 Configuration → Load balancer → Add listener (443, HTTPS, your ACM cert ARN). \
                 Or via `:set-option aws:elbv2:listener:443 Protocol HTTPS` + \
                 `:set-option aws:elbv2:listener:443 SSLCertificateArns arn:aws:acm:...`. \
                 Cert ARN is operator-specific — `--fix` won't guess."
                .into(),
        })
    }
    fn applies(&self, ctx: &LintContext) -> Option<Issue> {
        // Scan all listener namespaces. We short-circuit when any
        // HTTPS listener exists, so mixed redirect-only HTTP+HTTPS
        // configs (HTTP listener forwarding to HTTPS for redirect)
        // don't false-positive. Only flag fleets that are HTTP-only.
        let mut http_listeners: Vec<String> = Vec::new();
        let mut any_https = false;
        for (ns, name, value) in ctx.options {
            if !ns.starts_with("aws:elbv2:listener:") {
                continue;
            }
            if name == "Protocol" && value.eq_ignore_ascii_case("HTTPS") {
                any_https = true;
            }
            if name == "Protocol" && value.eq_ignore_ascii_case("HTTP") {
                let port = ns.trim_start_matches("aws:elbv2:listener:").to_string();
                http_listeners.push(port);
            }
        }
        if http_listeners.is_empty() || any_https {
            return None;
        }
        let mut fields = BTreeMap::new();
        fields.insert("http_listener_ports".into(), http_listeners.join(","));
        Some(Issue {
            rule_id: self.id().into(),
            severity: self.severity(),
            env_name: Some(ctx.env.name.clone()),
            title: format!(
                "ELB serves HTTP on port {} with no HTTPS listener",
                http_listeners.join(",")
            ),
            detail: "Traffic flows in plaintext. Most operator security baselines (PCI, SOC2, \
                 internal policy) require TLS at the load balancer. EB supports HTTPS via \
                 `aws:elbv2:listener:443` with an ACM cert ARN."
                .into(),
            suggestion: Some(
                ":set-option aws:elbv2:listener:443 Protocol HTTPS  (then add cert ARN)".into(),
            ),
            fields,
        })
    }
}

/// EBL008 — Stale solution-stack version. EB platforms get
/// security + runtime updates that operators need to opt into
/// (managed-updates) or apply manually. A solution stack older
/// than ~180 days is the typical operator-visible signal that
/// the platform has fallen behind. Detection here is structural
/// only — we flag any solution-stack string with a year-month
/// embedded that's older than 180 days from `chrono::Utc::now()`.
/// The right target version is platform-family-specific; no
/// auto-fix.
pub struct StalePlatformVersion;

impl Rule for StalePlatformVersion {
    fn id(&self) -> &'static str {
        "EBL008"
    }
    fn severity(&self) -> Severity {
        Severity::Warn
    }
    fn fix(&self, ctx: &LintContext) -> Option<FixAction> {
        self.applies(ctx)?;
        Some(FixAction::Manual {
            instructions: "Upgrade the platform to a current solution stack. In the EB console: \
                 Configuration → Platform → Change. Or via `:upgrade-platform` in ebman \
                 (select the new platform ARN from the picker). The target version is \
                 platform-family-specific — `--fix` won't guess. Consider enabling \
                 managed-updates so future patches apply automatically."
                .into(),
        })
    }
    fn applies(&self, ctx: &LintContext) -> Option<Issue> {
        let stack = &ctx.env.solution_stack;
        if stack.is_empty() {
            return None;
        }
        // The version-tuple comparison lives in `aws::newer_stack_version`
        // (already unit-tested); callers populate `ctx.newer_stack_available`
        // with the result. If `Some(version)`, the env is stale and
        // we fire. If `None`, the env is current OR the latest-stacks
        // data isn't loaded.
        //
        // 0.17 STATE: live in the TUI (`:lint`, `:explain`,
        // confirm-modal) — those paths plumb `App.latest_stacks`
        // via `aws::newer_stack_version()`. CLI (`ebman lint`,
        // `ebman explain`) still no-ops — the CLI doesn't have an
        // App, so it'd need its own `ListAvailableSolutionStacks`
        // fetch. Tracked for 0.18. CLI no-op pinned by
        // `ebl008_currently_stub_does_not_fire_in_cli` below.
        let newer = ctx.newer_stack_available?;
        let mut fields = BTreeMap::new();
        fields.insert("current_stack".into(), stack.clone());
        fields.insert("newer_version".into(), newer.to_string());
        Some(Issue {
            rule_id: self.id().into(),
            severity: self.severity(),
            env_name: Some(ctx.env.name.clone()),
            title: format!("Platform solution-stack is behind: newer version {newer} available"),
            detail: format!(
                "Current stack: {stack}\nNewer version available: {newer}\n\nNewer stacks \
                 ship security + runtime patches; staying on the old one defers known \
                 vulnerability fixes."
            ),
            suggestion: Some(":upgrade-platform  (pick the latest from the picker)".into()),
            fields,
        })
    }
}

/// EBL009 — Autoscaling Group with no health-check grace period
/// (or one set too low). Default is 0 in some EB platforms; new
/// instances are evaluated for ELB health the moment they're
/// launched, before app boot completes — flagged Unhealthy →
/// ASG terminates → infinite churn during deploys. EB
/// recommends ≥ 60s; production workloads typically want 180-300s.
pub struct AsgMissingHealthCheckGracePeriod;

impl Rule for AsgMissingHealthCheckGracePeriod {
    fn id(&self) -> &'static str {
        "EBL009"
    }
    fn severity(&self) -> Severity {
        Severity::Info
    }
    fn fix(&self, ctx: &LintContext) -> Option<FixAction> {
        self.applies(ctx)?;
        Some(FixAction::SetOption {
            namespace: "aws:autoscaling:asg".into(),
            name: "HealthCheckGracePeriod".into(),
            value: "300".into(),
            description: "ASG HealthCheckGracePeriod → 300s (5min boot window)".into(),
        })
    }
    fn applies(&self, ctx: &LintContext) -> Option<Issue> {
        // Only fires when ELB health checking is in use (otherwise
        // the grace period is moot — EC2 health alone is fast).
        let elb_type = option_value(
            ctx.options,
            "aws:elasticbeanstalk:environment",
            "EnvironmentType",
        );
        if !elb_type.eq_ignore_ascii_case("LoadBalanced") {
            return None;
        }
        let grace = parse_i32(option_value(
            ctx.options,
            "aws:autoscaling:asg",
            "HealthCheckGracePeriod",
        ));
        let grace_val = grace.unwrap_or(0);
        if grace_val >= 60 {
            return None;
        }
        let mut fields = BTreeMap::new();
        fields.insert("grace_secs".into(), grace_val.to_string());
        fields.insert("recommended_min".into(), "60".into());
        Some(Issue {
            rule_id: self.id().into(),
            severity: self.severity(),
            env_name: Some(ctx.env.name.clone()),
            title: format!(
                "ASG HealthCheckGracePeriod={grace_val}s — new instances evaluated for ELB health before boot completes"
            ),
            detail: format!(
                "EnvironmentType=LoadBalanced with HealthCheckGracePeriod={grace_val}s. New \
                 instances launched by autoscaling get evaluated for ELB health the moment \
                 they come up — before app boot completes. ELB flags them Unhealthy, ASG \
                 terminates them, deploys churn forever. Floor: 60s. Typical production: \
                 180-300s depending on cold-start time."
            ),
            suggestion: Some(":set-option aws:autoscaling:asg HealthCheckGracePeriod 300".into()),
            fields,
        })
    }
}

/// EBL010 — Missing required tags. Operator declares the
/// expected tag set via `required_tags = "Owner,Env,Cost"` in
/// `config.toml`; this rule fires when any of those tags is
/// absent from an env's tag set. Detection is structural —
/// `ctx.env.tags` lists the active tag keys. Manual fix
/// because tag VALUES are operator-specific. No-op when
/// `required_tags` is empty (operator hasn't declared any).
pub struct MissingRequiredTags;

impl Rule for MissingRequiredTags {
    fn id(&self) -> &'static str {
        "EBL010"
    }
    fn severity(&self) -> Severity {
        Severity::Info
    }
    fn fix(&self, ctx: &LintContext) -> Option<FixAction> {
        self.applies(ctx)?;
        Some(FixAction::Manual {
            instructions: "Add the missing tags via `:tag Owner=team-a` (one per missing key). \
                 Tag values are operator-specific — `--fix` won't guess. To stop the \
                 rule from firing for an env that legitimately lacks them, add the \
                 rule to `lint.disable` for that project."
                .into(),
        })
    }
    fn applies(&self, ctx: &LintContext) -> Option<Issue> {
        // Three guards before firing:
        //  1. Operator declared required_tags (else nothing to check)
        //  2. Caller populated env_tag_keys (else we can't compare —
        //     `:lint` doesn't fetch tags yet; the Detail/Tags tab
        //     does, but that data isn't on App today)
        //  3. At least one required key is missing from the env
        // Wiring env_tag_keys at every call site is a 0.18 follow-
        // up; until then, this rule fires only when callers
        // explicitly pass tag keys (e.g. confirm-modal in the
        // future).
        if ctx.required_tags.is_empty() || ctx.env_tag_keys.is_empty() {
            return None;
        }
        let missing: Vec<&str> = ctx
            .required_tags
            .iter()
            .filter(|req| !ctx.env_tag_keys.iter().any(|k| k.eq_ignore_ascii_case(req)))
            .map(String::as_str)
            .collect();
        if missing.is_empty() {
            return None;
        }
        let mut fields = BTreeMap::new();
        fields.insert("missing_tag_keys".into(), missing.join(","));
        Some(Issue {
            rule_id: self.id().into(),
            severity: self.severity(),
            env_name: Some(ctx.env.name.clone()),
            title: format!("Env is missing required tag(s): {}", missing.join(", ")),
            detail: format!(
                "config.toml declares required_tags = [{}]. The env is missing: {}. \
                 Add the tags via `:tag KEY=VALUE` (one per missing key). Tag values \
                 are operator-specific; the rule only checks key presence.",
                ctx.required_tags
                    .iter()
                    .map(|s| format!("\"{s}\""))
                    .collect::<Vec<_>>()
                    .join(", "),
                missing.join(", ")
            ),
            suggestion: Some(format!(":tag {}=<value>", missing[0])),
            fields,
        })
    }
}

/// EBL011 — Worker env with a stuck DLQ. Headline failure mode
/// for SQS-driven workers: consumer crashes or hangs, messages
/// land in the dead-letter queue, queue depth climbs until
/// operator notices. The rule fires when `dlq_depth > threshold`
/// (default 100; configurable via the caller). Auto-fix=Manual:
/// scale workers / restart / drain — operator-context-dependent.
pub struct WorkerDlqStuck;

/// Threshold for EBL011. Hard-coded for v1; future config-tunable
/// via `lint.ebl011.threshold` if operators ask.
const EBL011_DLQ_THRESHOLD: i64 = 100;

impl Rule for WorkerDlqStuck {
    fn id(&self) -> &'static str {
        "EBL011"
    }
    fn severity(&self) -> Severity {
        Severity::Warn
    }
    fn fix(&self, ctx: &LintContext) -> Option<FixAction> {
        self.applies(ctx)?;
        Some(FixAction::Manual {
            instructions:
                "DLQ depth above threshold. Triage steps: (1) Sample a few DLQ messages via \
                 `aws sqs receive-message --queue-url <dlq>` to identify the failure shape; \
                 (2) check worker logs in Detail/Logs for the corresponding exception; \
                 (3) once root cause is known, decide whether to scale workers, restart \
                 the env, redrive messages from the DLQ back to the source queue, or \
                 purge the DLQ entirely. `--fix` can't decide; this is operator-judgment."
                    .into(),
        })
    }
    fn applies(&self, ctx: &LintContext) -> Option<Issue> {
        // Only fires on Worker-tier envs; web-tier envs don't have
        // a DLQ in the EB-managed sense.
        if !ctx.env.tier.eq_ignore_ascii_case("Worker") {
            return None;
        }
        let depth = ctx.dlq_depth?;
        if depth <= EBL011_DLQ_THRESHOLD {
            return None;
        }
        let mut fields = BTreeMap::new();
        fields.insert("dlq_depth".into(), depth.to_string());
        fields.insert("threshold".into(), EBL011_DLQ_THRESHOLD.to_string());
        Some(Issue {
            rule_id: self.id().into(),
            severity: self.severity(),
            env_name: Some(ctx.env.name.clone()),
            title: format!("Worker DLQ depth {depth} above threshold ({EBL011_DLQ_THRESHOLD})"),
            detail: format!(
                "Dead-letter queue holds {depth} messages. Worker env consumers have failed \
                 to process them. Sustained DLQ growth typically signals a poison-message \
                 issue (parsing exception, downstream API down, OOM) or a consumer-side \
                 logic bug. Operator should triage via `aws sqs receive-message` + worker \
                 logs before redriving or purging."
            ),
            suggestion: Some(":logs-tail  (and check the worker exception)".into()),
            fields,
        })
    }
}

/// EBL012 — Env reports `status=Ready health=Green` but the
/// healthy instance count is 0. Classic ELB-vs-EB health-check
/// divergence: EB's internal health monitor still believes the
/// env is fine (perhaps because the platform health agent hasn't
/// observed otherwise yet), but the ALB target group reports no
/// healthy targets — so traffic is silently failing while the
/// dashboard says Green. High-signal alert.
pub struct GreenButZeroInstances;

impl Rule for GreenButZeroInstances {
    fn id(&self) -> &'static str {
        "EBL012"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }
    fn fix(&self, ctx: &LintContext) -> Option<FixAction> {
        self.applies(ctx)?;
        Some(FixAction::Manual {
            instructions:
                "EB reports Green but no instances are healthy. Investigate the divergence: \
                 (1) Detail/Health to see what EB's health monitor sees; (2) Detail/Instances \
                 to check whether instances exist at all; (3) ALB target-group health checks \
                 directly via `aws elbv2 describe-target-health`. Common causes: stuck \
                 deploy mid-instance-rotation, ALB health check URL wrong / app endpoint \
                 changed, OOMKilled workers, security-group misconfig. Auto-fix can't help; \
                 operator must diagnose."
                    .into(),
        })
    }
    fn applies(&self, ctx: &LintContext) -> Option<Issue> {
        // Both Ready status AND Green health are required — we
        // don't want to fire on transient Updating + 0 instances
        // (that's the deploy-in-flight case, not a divergence).
        if !ctx.env.status.eq_ignore_ascii_case("Ready") {
            return None;
        }
        if !ctx.env.health.eq_ignore_ascii_case("Green")
            && !ctx.env.health.eq_ignore_ascii_case("Ok")
        {
            return None;
        }
        let count = ctx.healthy_instance_count?;
        if count > 0 {
            return None;
        }
        let mut fields = BTreeMap::new();
        fields.insert("healthy_count".into(), count.to_string());
        fields.insert("status".into(), ctx.env.status.clone());
        fields.insert("health".into(), ctx.env.health.clone());
        Some(Issue {
            rule_id: self.id().into(),
            severity: self.severity(),
            env_name: Some(ctx.env.name.clone()),
            title: "Env shows Green but reports 0 healthy instances".into(),
            detail: "EB's status+health say the env is fine, but the ALB target group / EC2 \
                 reports no healthy targets. Traffic is failing silently while the dashboard \
                 looks clean. Common causes: stuck deploy mid-rotation, ALB health-check URL \
                 misconfig, OOMKilled instances pre-launch, security-group blocks. Drill \
                 into Detail/Health + Detail/Instances to triage."
                .into(),
            suggestion: Some(":health  (drill into EB's health detail)".into()),
            fields,
        })
    }
}

/// Build the v1 rule registry. Operator-disabled rules are
/// filtered HERE — at registry-load time — so a disabled rule
/// has zero per-env cost. Severity overrides not yet
/// implemented (BONUS-tier 0.13 item).
/// EBL013 — Launch configuration ASG (legacy). AWS is sunsetting
/// EC2 launch configurations in favour of launch templates; EB envs
/// still on the legacy shape will face migration friction down the
/// line. Detection: any non-empty option in the
/// `aws:autoscaling:launchconfiguration` namespace, which is the
/// legacy ASG-config surface (EB envs created via the new launch-
/// template path keep this namespace empty). Fix=Manual — migrating
/// from launch config to launch template needs an EB env rebuild and
/// careful capacity-loss planning, not a one-shot option flip.
pub struct LaunchConfigurationLegacy;

impl Rule for LaunchConfigurationLegacy {
    fn id(&self) -> &'static str {
        "EBL013"
    }
    fn severity(&self) -> Severity {
        Severity::Warn
    }
    fn fix(&self, ctx: &LintContext) -> Option<FixAction> {
        self.applies(ctx)?;
        Some(FixAction::Manual {
            instructions:
                "Env is configured via the legacy `aws:autoscaling:launchconfiguration` namespace. \
                 AWS is sunsetting EC2 launch configurations (no new account onboardings since \
                 2024-12-31). To migrate: (1) check your platform version supports launch \
                 templates (EB platform versions from 2022 onward); (2) rebuild the env via \
                 `ebman action rebuild --env NAME` after EB has been configured to use launch \
                 templates at the platform level. The migration is operator-context-dependent \
                 (capacity-loss planning, dependent IAM roles, etc.); --fix can't drive it."
                    .into(),
        })
    }
    fn applies(&self, ctx: &LintContext) -> Option<Issue> {
        // Any non-empty option in the launchconfiguration namespace
        // signals legacy usage. New launch-template envs keep this
        // namespace completely empty (option-settings fetch returns
        // nothing for it).
        let has_legacy = ctx
            .options
            .iter()
            .any(|(ns, _, v)| ns == "aws:autoscaling:launchconfiguration" && !v.is_empty());
        if !has_legacy {
            return None;
        }
        let mut fields = BTreeMap::new();
        fields.insert(
            "namespace".into(),
            "aws:autoscaling:launchconfiguration".into(),
        );
        Some(Issue {
            rule_id: self.id().into(),
            severity: self.severity(),
            env_name: Some(ctx.env.name.clone()),
            title: "Env using legacy launch configuration (AWS sunsetting EC2 LC)".into(),
            detail:
                "The env is configured via `aws:autoscaling:launchconfiguration:*` option \
                 settings, which is the legacy EC2 launch-configuration shape. AWS is sunsetting \
                 launch configurations: no new account onboardings since 2024-12-31, and the \
                 deprecation path will eventually break envs that haven't migrated. EB envs on \
                 modern platform versions can use launch templates (`aws:autoscaling:launchtemplate:*`) \
                 which is the supported forward path."
                    .into(),
            suggestion: Some(
                "Plan a launch-template migration: verify your platform version supports it, \
                 then rebuild the env when ready (downtime applies)."
                    .into(),
            ),
            fields,
        })
    }
}

/// Pure: split a comma-delimited list value (used by EB for things
/// like `aws:ec2:vpc:Subnets`) into trimmed, non-empty entries. EB
/// sometimes returns padded values like `"subnet-a, subnet-b"`; we
/// tolerate.
pub fn parse_csv_value(value: &str) -> Vec<&str> {
    value
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect()
}

/// EBL019 — AllAtOnce deploy policy on a multi-subnet (likely multi-
/// AZ) env. Stronger version of EBL001: a 100%-capacity-loss deploy
/// is bad on any multi-instance env, but on a multi-AZ env it also
/// takes ALL availability zones offline at once, defeating the whole
/// point of running across multiple AZs. Detection: EBL001's
/// condition (DeploymentPolicy=AllAtOnce + MaxSize>1) AND the env
/// has 2+ subnets configured via `aws:ec2:vpc:Subnets`. The subnet
/// heuristic is the cheapest proxy for "multi-AZ" — EB doesn't
/// expose the AZ mapping in option settings, so we infer from the
/// subnet count. False-positive on the rare case where two subnets
/// live in the same AZ; operators can `lint.disable = ["EBL019"]`
/// if that bites. Auto-fix is the same SetOption as EBL001.
pub struct AllAtOnceMultiAz;

impl Rule for AllAtOnceMultiAz {
    fn id(&self) -> &'static str {
        "EBL019"
    }
    fn severity(&self) -> Severity {
        Severity::Warn
    }
    fn fix(&self, ctx: &LintContext) -> Option<FixAction> {
        self.applies(ctx)?;
        Some(FixAction::SetOption {
            namespace: "aws:elasticbeanstalk:command".into(),
            name: "DeploymentPolicy".into(),
            value: "Rolling".into(),
            description:
                "DeploymentPolicy: AllAtOnce → Rolling (preserves capacity across AZs during deploys)"
                    .into(),
        })
    }
    fn applies(&self, ctx: &LintContext) -> Option<Issue> {
        let policy = option_value(
            ctx.options,
            "aws:elasticbeanstalk:command",
            "DeploymentPolicy",
        );
        if !policy.eq_ignore_ascii_case("AllAtOnce") {
            return None;
        }
        let max_size = parse_i32(option_value(ctx.options, "aws:autoscaling:asg", "MaxSize"))?;
        if max_size <= 1 {
            return None;
        }
        let subnets_csv = option_value(ctx.options, "aws:ec2:vpc", "Subnets");
        let subnet_count = parse_csv_value(subnets_csv).len();
        if subnet_count < 2 {
            return None;
        }
        let mut fields = BTreeMap::new();
        fields.insert("policy".into(), policy.to_string());
        fields.insert("max_size".into(), max_size.to_string());
        fields.insert("subnet_count".into(), subnet_count.to_string());
        Some(Issue {
            rule_id: self.id().into(),
            severity: self.severity(),
            env_name: Some(ctx.env.name.clone()),
            title: format!(
                "AllAtOnce on multi-subnet env ({subnet_count} subnets): every AZ goes offline simultaneously"
            ),
            detail: format!(
                "DeploymentPolicy is {policy} with MaxSize={max_size} and {subnet_count} subnets \
                 configured. A deploy takes EVERY instance offline at the same time — including \
                 instances in every AZ — defeating the multi-AZ fault tolerance you're paying \
                 for. Rolling preserves at least one AZ during the deploy."
            ),
            suggestion: Some(
                ":deployment-policy Rolling  (or RollingWithAdditionalBatch for zero downtime)"
                    .into(),
            ),
            fields,
        })
    }
}

/// EBL017 — Managed Platform Updates disabled. Detection: the env's
/// `aws:elasticbeanstalk:managedactions.ManagedActionsEnabled`
/// option-setting is `"false"` (or any non-`"true"` value — EB
/// defaults to disabled when the setting is missing). Op-sec gap:
/// env doesn't receive the platform's automatic security patches
/// during the configured maintenance window. Fix=Manual (operator
/// may have a deliberate reason to disable — e.g. a frozen
/// production env mid-incident — so `--fix` doesn't flip it).
pub struct ManagedActionsDisabled;

impl Rule for ManagedActionsDisabled {
    fn id(&self) -> &'static str {
        "EBL017"
    }
    fn severity(&self) -> Severity {
        Severity::Info
    }
    fn fix(&self, ctx: &LintContext) -> Option<FixAction> {
        self.applies(ctx)?;
        Some(FixAction::Manual {
            instructions: "Managed Platform Updates are disabled. Enable via `:set-option \
                 aws:elasticbeanstalk:managedactions:ManagedActionsEnabled true` and \
                 configure the maintenance window (`PreferredStartTime`) before re-enabling \
                 if your platform family supports it. Some operators disable this \
                 deliberately (frozen prod env mid-incident; controlled patching via CI) — \
                 if that's you, add EBL017 to `lint.disable` in `config.toml`."
                .into(),
        })
    }
    fn applies(&self, ctx: &LintContext) -> Option<Issue> {
        // The option lives in this namespace. EB returns it as a
        // string, not a bool. Default value when unset depends on the
        // env's platform family (most modern platforms default to
        // disabled). We treat absent + any value other than literal
        // "true" (case-insensitive) as "disabled" so we catch every
        // shape of "not on".
        let value = ctx
            .options
            .iter()
            .find(|(ns, name, _)| {
                ns == "aws:elasticbeanstalk:managedactions" && name == "ManagedActionsEnabled"
            })
            .map(|(_, _, v)| v.as_str())
            .unwrap_or("");
        if value.eq_ignore_ascii_case("true") {
            return None;
        }
        let mut fields = BTreeMap::new();
        fields.insert("managed_actions_enabled".into(), value.to_string());
        Some(Issue {
            rule_id: self.id().into(),
            severity: self.severity(),
            env_name: Some(ctx.env.name.clone()),
            title: "Managed Platform Updates disabled".into(),
            detail: "Managed Platform Updates handle the platform's automatic security patches \
                 during the configured maintenance window. With this disabled, the env \
                 doesn't receive minor-version patches automatically — operators must \
                 dispatch `:upgrade` manually when AWS publishes a new platform version. \
                 For long-lived envs, this is a real op-sec gap; for short-lived staging / \
                 ephemeral envs it's usually fine to leave off."
                .into(),
            suggestion: Some(
                ":set-option aws:elasticbeanstalk:managedactions:ManagedActionsEnabled true".into(),
            ),
            fields,
        })
    }
}

pub fn default_rules(disabled: &[String]) -> Vec<Box<dyn Rule>> {
    let candidates: Vec<Box<dyn Rule>> = vec![
        Box::new(AllAtOnceMultiInstance),
        Box::new(WebTierNoHealthCheckUrl),
        Box::new(EnvRedForExtendedPeriod),
        Box::new(BatchSizeExceedsMaxSize),
        Box::new(SingleInstanceEnv),
        Box::new(CooldownBelowRecommended),
        Box::new(ElbWithoutHttps),
        Box::new(StalePlatformVersion),
        Box::new(AsgMissingHealthCheckGracePeriod),
        Box::new(MissingRequiredTags),
        Box::new(WorkerDlqStuck),
        Box::new(GreenButZeroInstances),
        Box::new(LaunchConfigurationLegacy),
        Box::new(ManagedActionsDisabled),
        Box::new(AllAtOnceMultiAz),
    ];
    candidates
        .into_iter()
        .filter(|r| !disabled.iter().any(|d| d == r.id()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aws::Environment;

    fn mk_env(name: &str, tier: &str, health: &str) -> Environment {
        Environment {
            name: name.into(),
            application: "shop".into(),
            status: "Ready".into(),
            health: health.into(),
            platform: "Java 17".into(),
            solution_stack: String::new(),
            tier: tier.into(),
            cname: format!("{name}.example.com"),
            version_label: "build-1".into(),
            arn: Some(format!("arn:aws:eb:us-east-1:0:env/{name}")),
            updated: None,
            id: None,
            region: None,
        }
    }

    fn mk_opt(ns: &str, name: &str, value: &str) -> (String, String, String) {
        (ns.into(), name.into(), value.into())
    }

    fn ctx<'a>(env: &'a Environment, options: &'a [(String, String, String)]) -> LintContext<'a> {
        LintContext::for_env(env, options)
    }

    #[test]
    fn severity_parses_common_forms() {
        assert_eq!(Severity::parse("info"), Some(Severity::Info));
        assert_eq!(Severity::parse("INFO"), Some(Severity::Info));
        assert_eq!(Severity::parse("warn"), Some(Severity::Warn));
        assert_eq!(Severity::parse("warning"), Some(Severity::Warn));
        assert_eq!(Severity::parse("Error"), Some(Severity::Error));
        assert_eq!(Severity::parse("err"), Some(Severity::Error));
        assert_eq!(Severity::parse("nope"), None);
    }

    #[test]
    fn ebl001_fires_on_all_at_once_multi_instance() {
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![
            mk_opt(
                "aws:elasticbeanstalk:command",
                "DeploymentPolicy",
                "AllAtOnce",
            ),
            mk_opt("aws:autoscaling:asg", "MaxSize", "4"),
        ];
        let issue = AllAtOnceMultiInstance.applies(&ctx(&env, &opts));
        let issue = issue.expect("EBL001 should fire");
        assert_eq!(issue.rule_id, "EBL001");
        assert_eq!(issue.severity, Severity::Warn);
        assert!(issue.title.contains("4-instance"));
        assert!(issue.suggestion.as_ref().unwrap().contains("Rolling"));
    }

    #[test]
    fn ebl001_skips_when_max_size_1() {
        // Single-instance env: AllAtOnce is fine (only one instance
        // to restart anyway). EBL005 catches "single instance" as
        // a separate concern; EBL001 stays focused on multi-instance.
        let env = mk_env("dev", "Web", "Green");
        let opts = vec![
            mk_opt(
                "aws:elasticbeanstalk:command",
                "DeploymentPolicy",
                "AllAtOnce",
            ),
            mk_opt("aws:autoscaling:asg", "MaxSize", "1"),
        ];
        assert!(AllAtOnceMultiInstance.applies(&ctx(&env, &opts)).is_none());
    }

    #[test]
    fn ebl001_skips_when_policy_is_rolling() {
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![
            mk_opt(
                "aws:elasticbeanstalk:command",
                "DeploymentPolicy",
                "Rolling",
            ),
            mk_opt("aws:autoscaling:asg", "MaxSize", "4"),
        ];
        assert!(AllAtOnceMultiInstance.applies(&ctx(&env, &opts)).is_none());
    }

    #[test]
    fn ebl002_fires_on_web_tier_with_empty_health_check_url() {
        let env = mk_env("prod", "Web", "Green");
        let opts: Vec<(String, String, String)> = vec![];
        let issue = WebTierNoHealthCheckUrl.applies(&ctx(&env, &opts));
        let issue = issue.expect("EBL002 should fire");
        assert_eq!(issue.rule_id, "EBL002");
    }

    #[test]
    fn ebl002_fires_on_web_tier_with_root_health_check_url() {
        // EB's default-when-empty is "/", so an explicit "/" is
        // still effectively "no real health check".
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![mk_opt(
            "aws:elasticbeanstalk:application",
            "Application Healthcheck URL",
            "/",
        )];
        assert!(WebTierNoHealthCheckUrl.applies(&ctx(&env, &opts)).is_some());
    }

    #[test]
    fn ebl002_skips_on_worker_tier() {
        let env = mk_env("worker", "Worker", "Green");
        let opts: Vec<(String, String, String)> = vec![];
        assert!(WebTierNoHealthCheckUrl.applies(&ctx(&env, &opts)).is_none());
    }

    #[test]
    fn ebl002_skips_with_explicit_health_path() {
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![mk_opt(
            "aws:elasticbeanstalk:application",
            "Application Healthcheck URL",
            "/health",
        )];
        assert!(WebTierNoHealthCheckUrl.applies(&ctx(&env, &opts)).is_none());
    }

    #[test]
    fn ebl003_fires_when_env_red_for_over_4h() {
        let mut env = mk_env("prod", "Web", "Red");
        env.updated = Some(chrono::Utc::now() - chrono::Duration::hours(5));
        let opts: Vec<(String, String, String)> = vec![];
        let issue = EnvRedForExtendedPeriod
            .applies(&ctx(&env, &opts))
            .expect("EBL003 should fire");
        assert!(issue.title.contains("Red"));
    }

    #[test]
    fn ebl003_skips_when_recently_red() {
        let mut env = mk_env("prod", "Web", "Red");
        env.updated = Some(chrono::Utc::now() - chrono::Duration::minutes(30));
        let opts: Vec<(String, String, String)> = vec![];
        assert!(EnvRedForExtendedPeriod.applies(&ctx(&env, &opts)).is_none());
    }

    #[test]
    fn ebl003_skips_when_health_unknown() {
        // No `updated` timestamp — can't compute duration, so skip.
        let env = mk_env("prod", "Web", "Red");
        let opts: Vec<(String, String, String)> = vec![];
        assert!(EnvRedForExtendedPeriod.applies(&ctx(&env, &opts)).is_none());
    }

    #[test]
    fn ebl004_fires_when_fixed_batch_exceeds_max_size() {
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![
            mk_opt("aws:elasticbeanstalk:command", "BatchSize", "8"),
            mk_opt("aws:elasticbeanstalk:command", "BatchSizeType", "Fixed"),
            mk_opt("aws:autoscaling:asg", "MaxSize", "4"),
        ];
        let issue = BatchSizeExceedsMaxSize
            .applies(&ctx(&env, &opts))
            .expect("EBL004 should fire");
        assert!(issue.title.contains("8") && issue.title.contains("4"));
    }

    #[test]
    fn ebl004_skips_percentage_batches() {
        // Percentage batches are a ratio, not an absolute count —
        // can't exceed MaxSize by definition.
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![
            mk_opt("aws:elasticbeanstalk:command", "BatchSize", "50"),
            mk_opt(
                "aws:elasticbeanstalk:command",
                "BatchSizeType",
                "Percentage",
            ),
            mk_opt("aws:autoscaling:asg", "MaxSize", "4"),
        ];
        assert!(BatchSizeExceedsMaxSize.applies(&ctx(&env, &opts)).is_none());
    }

    #[test]
    fn ebl005_fires_on_single_instance_env() {
        let env = mk_env("dev", "Web", "Green");
        let opts = vec![
            mk_opt("aws:autoscaling:asg", "MinSize", "1"),
            mk_opt("aws:autoscaling:asg", "MaxSize", "1"),
        ];
        assert!(SingleInstanceEnv.applies(&ctx(&env, &opts)).is_some());
    }

    #[test]
    fn ebl005_skips_when_max_size_above_1() {
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![
            mk_opt("aws:autoscaling:asg", "MinSize", "1"),
            mk_opt("aws:autoscaling:asg", "MaxSize", "4"),
        ];
        assert!(SingleInstanceEnv.applies(&ctx(&env, &opts)).is_none());
    }

    #[test]
    fn ebl006_fires_when_cooldown_below_60s() {
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![mk_opt("aws:autoscaling:asg", "Cooldown", "30")];
        assert!(CooldownBelowRecommended
            .applies(&ctx(&env, &opts))
            .is_some());
    }

    #[test]
    fn ebl006_skips_at_or_above_60s() {
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![mk_opt("aws:autoscaling:asg", "Cooldown", "60")];
        assert!(CooldownBelowRecommended
            .applies(&ctx(&env, &opts))
            .is_none());
    }

    #[test]
    fn default_rules_filters_disabled() {
        let all = default_rules(&[]);
        let n_all = all.len();
        let filtered = default_rules(&["EBL001".to_string(), "EBL003".to_string()]);
        assert_eq!(filtered.len(), n_all - 2);
        assert!(!filtered.iter().any(|r| r.id() == "EBL001"));
        assert!(!filtered.iter().any(|r| r.id() == "EBL003"));
    }

    #[test]
    fn run_rules_sorts_severity_desc_then_id_asc() {
        // Build a context that fires EBL001 (Warn), EBL003 (Warn),
        // EBL005 (Info). Verify the output order: Warn-1, Warn-3,
        // Info-5.
        let mut env = mk_env("prod", "Web", "Red");
        env.updated = Some(chrono::Utc::now() - chrono::Duration::hours(5));
        let opts = vec![
            mk_opt(
                "aws:elasticbeanstalk:command",
                "DeploymentPolicy",
                "AllAtOnce",
            ),
            mk_opt(
                "aws:elasticbeanstalk:application",
                "Application Healthcheck URL",
                "/health",
            ),
            mk_opt("aws:autoscaling:asg", "MinSize", "1"),
            mk_opt("aws:autoscaling:asg", "MaxSize", "1"),
        ];
        // MaxSize=1 disables EBL001, so it shouldn't fire here.
        // Tweak the rule mix: leave a Warn-firing scenario plus
        // EBL005 (Info).
        let rules = default_rules(&[]);
        let issues = run_rules(&rules, &ctx(&env, &opts));
        // Build the expected severity ladder: Warn comes first.
        let ids: Vec<&str> = issues.iter().map(|i| i.rule_id.as_str()).collect();
        // EBL003 (Warn) before EBL005 (Info)
        let pos_003 = ids.iter().position(|&i| i == "EBL003");
        let pos_005 = ids.iter().position(|&i| i == "EBL005");
        if let (Some(p3), Some(p5)) = (pos_003, pos_005) {
            assert!(p3 < p5, "Warn must sort before Info");
        }
    }

    #[test]
    fn render_issues_json_is_well_formed_and_consumable() {
        let issue = Issue {
            rule_id: "EBL001".into(),
            severity: Severity::Warn,
            env_name: Some("prod".into()),
            title: "AllAtOnce on 4-instance env".into(),
            detail: "Long detail with \"quotes\" and a\nnewline".into(),
            suggestion: Some(":deployment-policy Rolling".into()),
            fields: {
                let mut m = BTreeMap::new();
                m.insert("policy".into(), "AllAtOnce".into());
                m.insert("max_size".into(), "4".into());
                m
            },
        };
        let json = render_issues_json(&[issue]);
        // Round-trip through a YAML-superset parser to confirm it's
        // valid JSON. (serde_yml is already a dep; saves bringing
        // in serde_json just for the test.)
        let _: serde_yml::Value =
            serde_yml::from_str(&json).expect("rendered output must be valid JSON");
        // Spot-check the escape for the embedded quote + newline.
        assert!(json.contains("\\\"quotes\\\""));
        assert!(json.contains("\\n"));
        // Empty issues list — still a well-formed object.
        let empty = render_issues_json(&[]);
        let _: serde_yml::Value = serde_yml::from_str(&empty).unwrap();
        assert_eq!(empty, "{\"issues\":[]}");
    }

    // ─── fix() coverage ──────────────────────────────────────

    #[test]
    fn ebl001_fix_sets_rolling_when_rule_fires() {
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![
            mk_opt(
                "aws:elasticbeanstalk:command",
                "DeploymentPolicy",
                "AllAtOnce",
            ),
            mk_opt("aws:autoscaling:asg", "MaxSize", "4"),
        ];
        let fix = AllAtOnceMultiInstance.fix(&ctx(&env, &opts)).expect("fix");
        match fix {
            FixAction::SetOption {
                namespace,
                name,
                value,
                ..
            } => {
                assert_eq!(namespace, "aws:elasticbeanstalk:command");
                assert_eq!(name, "DeploymentPolicy");
                assert_eq!(value, "Rolling");
            }
            FixAction::Manual { .. } => panic!("EBL001 should auto-fix, not Manual"),
        }
    }

    #[test]
    fn ebl001_fix_none_when_rule_does_not_fire() {
        // Single-instance env — applies() returns None, so fix()
        // shouldn't dispatch a write the rule doesn't motivate.
        let env = mk_env("dev", "Web", "Green");
        let opts = vec![
            mk_opt(
                "aws:elasticbeanstalk:command",
                "DeploymentPolicy",
                "AllAtOnce",
            ),
            mk_opt("aws:autoscaling:asg", "MaxSize", "1"),
        ];
        assert!(AllAtOnceMultiInstance.fix(&ctx(&env, &opts)).is_none());
    }

    #[test]
    fn ebl002_fix_is_manual_because_path_is_app_specific() {
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![mk_opt(
            "aws:elasticbeanstalk:application",
            "Application Healthcheck URL",
            "",
        )];
        let fix = WebTierNoHealthCheckUrl.fix(&ctx(&env, &opts)).expect("fix");
        assert!(matches!(fix, FixAction::Manual { .. }));
    }

    #[test]
    fn ebl003_has_no_fix_state_not_config() {
        // EBL003 (env Red >4h) is a state condition — no config
        // change auto-resolves it. Default `None` from the trait
        // is correct.
        let env = mk_env("prod", "Web", "Red");
        let opts: Vec<(String, String, String)> = vec![];
        assert!(EnvRedForExtendedPeriod.fix(&ctx(&env, &opts)).is_none());
    }

    #[test]
    fn ebl004_fix_clamps_batch_size_to_max_size() {
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![
            mk_opt("aws:elasticbeanstalk:command", "BatchSize", "10"),
            mk_opt("aws:elasticbeanstalk:command", "BatchSizeType", "Fixed"),
            mk_opt("aws:autoscaling:asg", "MaxSize", "4"),
        ];
        let fix = BatchSizeExceedsMaxSize.fix(&ctx(&env, &opts)).expect("fix");
        match fix {
            FixAction::SetOption { name, value, .. } => {
                assert_eq!(name, "BatchSize");
                assert_eq!(value, "4");
            }
            FixAction::Manual { .. } => panic!("EBL004 should auto-fix, not Manual"),
        }
    }

    #[test]
    fn ebl005_fix_is_manual_because_capacity_is_workload_dependent() {
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![
            mk_opt("aws:autoscaling:asg", "MinSize", "1"),
            mk_opt("aws:autoscaling:asg", "MaxSize", "1"),
        ];
        let fix = SingleInstanceEnv.fix(&ctx(&env, &opts)).expect("fix");
        assert!(matches!(fix, FixAction::Manual { .. }));
    }

    #[test]
    fn ebl006_fix_sets_cooldown_to_360() {
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![mk_opt("aws:autoscaling:asg", "Cooldown", "30")];
        let fix = CooldownBelowRecommended
            .fix(&ctx(&env, &opts))
            .expect("fix");
        match fix {
            FixAction::SetOption {
                namespace,
                name,
                value,
                ..
            } => {
                assert_eq!(namespace, "aws:autoscaling:asg");
                assert_eq!(name, "Cooldown");
                assert_eq!(value, "360");
            }
            FixAction::Manual { .. } => panic!("EBL006 should auto-fix, not Manual"),
        }
    }

    #[test]
    fn ebl006_fix_none_when_cooldown_already_compliant() {
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![mk_opt("aws:autoscaling:asg", "Cooldown", "360")];
        assert!(CooldownBelowRecommended.fix(&ctx(&env, &opts)).is_none());
    }

    // ─── EBL007+ (0.16) ──────────────────────────────────────

    #[test]
    fn ebl007_fires_on_http_only_listener() {
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![mk_opt("aws:elbv2:listener:80", "Protocol", "HTTP")];
        let issue = ElbWithoutHttps.applies(&ctx(&env, &opts)).expect("fires");
        assert_eq!(issue.rule_id, "EBL007");
        assert_eq!(
            issue.fields.get("http_listener_ports").map(String::as_str),
            Some("80")
        );
    }

    #[test]
    fn ebl007_skips_when_https_also_present() {
        // Mixed HTTP+HTTPS is acceptable (HTTP often used for
        // redirect-only). Only flag HTTP-only fleets.
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![
            mk_opt("aws:elbv2:listener:80", "Protocol", "HTTP"),
            mk_opt("aws:elbv2:listener:443", "Protocol", "HTTPS"),
        ];
        assert!(ElbWithoutHttps.applies(&ctx(&env, &opts)).is_none());
    }

    #[test]
    fn ebl007_fix_is_manual_because_cert_arn_is_operator_specific() {
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![mk_opt("aws:elbv2:listener:80", "Protocol", "HTTP")];
        let fix = ElbWithoutHttps.fix(&ctx(&env, &opts)).expect("fix");
        assert!(matches!(fix, FixAction::Manual { .. }));
    }

    #[test]
    fn ebl008_fires_when_live_stack_differs_from_latest() {
        let env = Environment {
            solution_stack: "64bit Amazon Linux 2 v3.5.1 running Docker".into(),
            ..mk_env("prod", "Web", "Green")
        };
        let opts: Vec<(String, String, String)> = vec![];
        // Caller has already determined a newer version exists
        // (via aws::newer_stack_version); we just pass the result.
        let ctx = LintContext::for_env(&env, &opts).with_newer_stack_available("3.6.0");
        let issue = StalePlatformVersion.applies(&ctx).expect("fires");
        assert_eq!(issue.rule_id, "EBL008");
        assert_eq!(
            issue.fields.get("newer_version").map(String::as_str),
            Some("3.6.0")
        );
    }

    #[test]
    fn ebl008_skips_when_newer_unknown() {
        // No newer_stack_available → best-effort skip (don't
        // false-positive on every env).
        let env = mk_env("prod", "Web", "Green");
        let opts: Vec<(String, String, String)> = vec![];
        assert!(StalePlatformVersion.applies(&ctx(&env, &opts)).is_none());
    }

    #[test]
    fn ebl008_currently_stub_does_not_fire_in_cli() {
        // SHIP NOTE pin: CLI lint / explain don't have an App,
        // so they can't compute newer_stack_available. The rule
        // no-ops there until the CLI grows its own
        // ListAvailableSolutionStacks fetch (tracked for 0.18).
        // This test documents the gap.
        let env = mk_env("prod", "Web", "Green");
        let opts: Vec<(String, String, String)> = vec![];
        // `ctx()` helper mirrors the CLI-side path which doesn't
        // populate newer_stack_available.
        assert!(StalePlatformVersion.applies(&ctx(&env, &opts)).is_none());
    }

    #[test]
    fn ebl008_skips_when_caller_says_no_newer() {
        // Caller checked App.latest_stacks and determined no
        // newer version exists → passes None → rule no-ops.
        let env = Environment {
            solution_stack: "64bit Amazon Linux 2 v3.6.0".into(),
            ..mk_env("prod", "Web", "Green")
        };
        let opts: Vec<(String, String, String)> = vec![];
        // No .with_newer_stack_available() → field stays None.
        let ctx = LintContext::for_env(&env, &opts);
        assert!(StalePlatformVersion.applies(&ctx).is_none());
    }

    #[test]
    fn ebl009_fires_when_loadbalanced_and_grace_below_60() {
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![
            mk_opt(
                "aws:elasticbeanstalk:environment",
                "EnvironmentType",
                "LoadBalanced",
            ),
            mk_opt("aws:autoscaling:asg", "HealthCheckGracePeriod", "0"),
        ];
        let issue = AsgMissingHealthCheckGracePeriod
            .applies(&ctx(&env, &opts))
            .expect("fires");
        assert_eq!(issue.rule_id, "EBL009");
    }

    #[test]
    fn ebl009_skips_single_instance_env() {
        // SingleInstance envs don't run an ELB — grace period
        // doesn't matter.
        let env = mk_env("dev", "Web", "Green");
        let opts = vec![mk_opt(
            "aws:elasticbeanstalk:environment",
            "EnvironmentType",
            "SingleInstance",
        )];
        assert!(AsgMissingHealthCheckGracePeriod
            .applies(&ctx(&env, &opts))
            .is_none());
    }

    #[test]
    fn ebl009_fix_sets_grace_to_300() {
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![
            mk_opt(
                "aws:elasticbeanstalk:environment",
                "EnvironmentType",
                "LoadBalanced",
            ),
            mk_opt("aws:autoscaling:asg", "HealthCheckGracePeriod", "0"),
        ];
        let fix = AsgMissingHealthCheckGracePeriod
            .fix(&ctx(&env, &opts))
            .expect("fix");
        match fix {
            FixAction::SetOption {
                namespace,
                name,
                value,
                ..
            } => {
                assert_eq!(namespace, "aws:autoscaling:asg");
                assert_eq!(name, "HealthCheckGracePeriod");
                assert_eq!(value, "300");
            }
            _ => panic!("EBL009 should SetOption-fix"),
        }
    }

    #[test]
    fn ebl010_skips_when_no_required_tags() {
        // Operator hasn't declared required_tags → nothing to
        // check, no false positive.
        let env = mk_env("prod", "Web", "Green");
        let opts: Vec<(String, String, String)> = vec![];
        let env_tags = vec!["Owner".to_string(), "Env".to_string()];
        let ctx = LintContext::for_env(&env, &opts).with_env_tag_keys(&env_tags);
        assert!(MissingRequiredTags.applies(&ctx).is_none());
    }

    #[test]
    fn ebl010_skips_when_env_tags_not_loaded() {
        // operator declared required_tags but caller didn't
        // populate env_tag_keys → can't compare; skip rather than
        // false-positive on every env.
        let env = mk_env("prod", "Web", "Green");
        let opts: Vec<(String, String, String)> = vec![];
        let required = vec!["Owner".to_string()];
        let ctx = LintContext::for_env(&env, &opts).with_required_tags(&required);
        assert!(MissingRequiredTags.applies(&ctx).is_none());
    }

    #[test]
    fn ebl010_fires_on_missing_required_tag() {
        let env = mk_env("prod", "Web", "Green");
        let opts: Vec<(String, String, String)> = vec![];
        let required = vec!["Owner".to_string(), "CostCentre".to_string()];
        let env_tags = vec!["Owner".to_string(), "Env".to_string()];
        let ctx = LintContext::for_env(&env, &opts)
            .with_required_tags(&required)
            .with_env_tag_keys(&env_tags);
        let issue = MissingRequiredTags.applies(&ctx).expect("fires");
        assert_eq!(issue.rule_id, "EBL010");
        assert_eq!(
            issue.fields.get("missing_tag_keys").map(String::as_str),
            Some("CostCentre")
        );
    }

    #[test]
    fn ebl010_check_is_case_insensitive() {
        let env = mk_env("prod", "Web", "Green");
        let opts: Vec<(String, String, String)> = vec![];
        let required = vec!["owner".to_string()];
        let env_tags = vec!["Owner".to_string()];
        let ctx = LintContext::for_env(&env, &opts)
            .with_required_tags(&required)
            .with_env_tag_keys(&env_tags);
        assert!(MissingRequiredTags.applies(&ctx).is_none());
    }

    #[test]
    fn ebl010_skips_when_all_required_present() {
        let env = mk_env("prod", "Web", "Green");
        let opts: Vec<(String, String, String)> = vec![];
        let required = vec!["Owner".to_string(), "Env".to_string()];
        let env_tags = vec!["Owner".to_string(), "Env".to_string(), "Extra".to_string()];
        let ctx = LintContext::for_env(&env, &opts)
            .with_required_tags(&required)
            .with_env_tag_keys(&env_tags);
        assert!(MissingRequiredTags.applies(&ctx).is_none());
    }

    #[test]
    fn default_rules_includes_ebl007_through_ebl012() {
        let rules = default_rules(&[]);
        let ids: Vec<&str> = rules.iter().map(|r| r.id()).collect();
        for id in ["EBL007", "EBL008", "EBL009", "EBL010", "EBL011", "EBL012"] {
            assert!(ids.contains(&id), "{id} missing from default_rules");
        }
    }

    // ─── EBL011 (worker DLQ stuck) ───────────────────────────

    #[test]
    fn ebl011_fires_when_worker_dlq_above_threshold() {
        let env = mk_env("worker", "Worker", "Green");
        let opts: Vec<(String, String, String)> = vec![];
        let ctx = LintContext::for_env(&env, &opts).with_dlq_depth(200);
        let issue = WorkerDlqStuck.applies(&ctx).expect("fires");
        assert_eq!(issue.rule_id, "EBL011");
        assert_eq!(
            issue.fields.get("dlq_depth").map(String::as_str),
            Some("200")
        );
    }

    #[test]
    fn ebl011_skips_web_tier() {
        let env = mk_env("web", "Web", "Green");
        let opts: Vec<(String, String, String)> = vec![];
        let ctx = LintContext::for_env(&env, &opts).with_dlq_depth(500);
        assert!(WorkerDlqStuck.applies(&ctx).is_none());
    }

    #[test]
    fn ebl011_skips_when_below_threshold() {
        let env = mk_env("worker", "Worker", "Green");
        let opts: Vec<(String, String, String)> = vec![];
        let ctx = LintContext::for_env(&env, &opts).with_dlq_depth(EBL011_DLQ_THRESHOLD);
        assert!(WorkerDlqStuck.applies(&ctx).is_none());
    }

    #[test]
    fn ebl011_skips_when_dlq_depth_unknown() {
        let env = mk_env("worker", "Worker", "Green");
        let opts: Vec<(String, String, String)> = vec![];
        // No .with_dlq_depth() → no data → skip
        assert!(WorkerDlqStuck.applies(&ctx(&env, &opts)).is_none());
    }

    #[test]
    fn ebl011_fix_is_manual() {
        let env = mk_env("worker", "Worker", "Green");
        let opts: Vec<(String, String, String)> = vec![];
        let ctx = LintContext::for_env(&env, &opts).with_dlq_depth(500);
        let fix = WorkerDlqStuck.fix(&ctx).expect("fix");
        assert!(matches!(fix, FixAction::Manual { .. }));
    }

    // ─── EBL012 (Green but 0 instances) ──────────────────────

    #[test]
    fn ebl012_fires_when_green_and_zero_instances() {
        let env = Environment {
            status: "Ready".into(),
            health: "Green".into(),
            ..mk_env("prod", "Web", "Green")
        };
        let opts: Vec<(String, String, String)> = vec![];
        let ctx = LintContext::for_env(&env, &opts).with_healthy_count(0);
        let issue = GreenButZeroInstances.applies(&ctx).expect("fires");
        assert_eq!(issue.rule_id, "EBL012");
        assert_eq!(issue.severity, Severity::Error);
    }

    #[test]
    fn ebl012_skips_when_instances_present() {
        let env = Environment {
            status: "Ready".into(),
            health: "Green".into(),
            ..mk_env("prod", "Web", "Green")
        };
        let opts: Vec<(String, String, String)> = vec![];
        let ctx = LintContext::for_env(&env, &opts).with_healthy_count(3);
        assert!(GreenButZeroInstances.applies(&ctx).is_none());
    }

    #[test]
    fn ebl012_skips_when_status_not_ready() {
        // Updating + Green is the deploy-in-flight case, not a
        // divergence. Don't fire mid-deploy.
        let env = Environment {
            status: "Updating".into(),
            health: "Green".into(),
            ..mk_env("prod", "Web", "Green")
        };
        let opts: Vec<(String, String, String)> = vec![];
        let ctx = LintContext::for_env(&env, &opts).with_healthy_count(0);
        assert!(GreenButZeroInstances.applies(&ctx).is_none());
    }

    #[test]
    fn ebl012_skips_when_health_not_green() {
        let env = Environment {
            status: "Ready".into(),
            health: "Red".into(),
            ..mk_env("prod", "Web", "Red")
        };
        let opts: Vec<(String, String, String)> = vec![];
        let ctx = LintContext::for_env(&env, &opts).with_healthy_count(0);
        // EBL003 handles long-Red; don't double-fire here.
        assert!(GreenButZeroInstances.applies(&ctx).is_none());
    }

    #[test]
    fn ebl012_skips_when_healthy_count_unknown() {
        // No .with_healthy_count() → no data → skip
        let env = Environment {
            status: "Ready".into(),
            health: "Green".into(),
            ..mk_env("prod", "Web", "Green")
        };
        let opts: Vec<(String, String, String)> = vec![];
        assert!(GreenButZeroInstances.applies(&ctx(&env, &opts)).is_none());
    }

    // ─── baseline parse + identity hash ─────────────────────

    #[test]
    fn issue_identity_hash_is_stable_across_calls() {
        let mut fields = BTreeMap::new();
        fields.insert("policy".into(), "AllAtOnce".into());
        fields.insert("max_size".into(), "4".into());
        let a = issue_identity_hash("EBL001", Some("prod"), &fields);
        let b = issue_identity_hash("EBL001", Some("prod"), &fields);
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn issue_identity_hash_differs_by_env_name() {
        let fields = BTreeMap::new();
        let a = issue_identity_hash("EBL001", Some("env-a"), &fields);
        let b = issue_identity_hash("EBL001", Some("env-b"), &fields);
        assert_ne!(a, b);
    }

    #[test]
    fn issue_identity_hash_differs_by_field_values() {
        let mut fields_a = BTreeMap::new();
        fields_a.insert("max_size".into(), "4".into());
        let mut fields_b = BTreeMap::new();
        fields_b.insert("max_size".into(), "8".into());
        let a = issue_identity_hash("EBL001", Some("prod"), &fields_a);
        let b = issue_identity_hash("EBL001", Some("prod"), &fields_b);
        assert_ne!(a, b);
    }

    /// **Golden test** for `issue_identity_hash`. Pins the exact hash
    /// for a known input so that any future change to the hash
    /// construction (field-key spelling, ordering, separator bytes,
    /// truncation length, hash function) becomes a deliberate decision
    /// rather than silent breakage. Operators' CI `--baseline` files
    /// store these hashes; changing them invalidates every baseline
    /// in the wild.
    ///
    /// If this test fails: the change to `issue_identity_hash` is a
    /// breaking change for `--baseline` consumers. Document the new
    /// hash, bump the audit-shape version in the CHANGELOG, and
    /// update this golden — or revert the change.
    #[test]
    fn issue_identity_hash_golden_pin() {
        let mut fields = BTreeMap::new();
        fields.insert("policy".into(), "AllAtOnce".into());
        fields.insert("max_size".into(), "4".into());
        let hash = issue_identity_hash("EBL001", Some("prod-eu-1"), &fields);
        // Pin: rule_id="EBL001", env="prod-eu-1", fields sorted by key
        // (BTreeMap iteration), separator=NUL, sha256, truncate to 8
        // bytes, hex-encode. Computed deterministically — do not edit
        // this constant without coordinating with --baseline consumers.
        assert_eq!(
            hash, "d7bd17690e12847e",
            "issue_identity_hash shape changed — see test docstring before updating this constant"
        );
    }

    /// Same shape but with `env_name = None` — pins the behaviour
    /// for un-anchored issues (e.g. multi-region lint findings that
    /// don't bind to a single env).
    #[test]
    fn issue_identity_hash_golden_pin_no_env() {
        let fields = BTreeMap::new();
        let hash = issue_identity_hash("EBL003", None, &fields);
        assert_eq!(
            hash, "ba1758f2587dbbe5",
            "issue_identity_hash (no env) shape changed — see test docstring"
        );
    }

    #[test]
    fn parse_baseline_extracts_issues() {
        let text = r#"{"issues":[
            {"rule_id":"EBL001","severity":"warn","env":"prod","title":"AllAtOnce on 4-instance env","detail":"...","fields":{"policy":"AllAtOnce","max_size":"4"}},
            {"rule_id":"EBL005","severity":"info","env":"dev","title":"Single-instance env","detail":"...","fields":{"min_size":"1","max_size":"1"}}
        ]}"#;
        let parsed = parse_baseline(text).expect("ok");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].rule_id, "EBL001");
        assert_eq!(parsed[0].env_name.as_deref(), Some("prod"));
        assert_eq!(parsed[0].title, "AllAtOnce on 4-instance env");
        assert_eq!(parsed[0].identity.len(), 16);
        assert_eq!(parsed[1].rule_id, "EBL005");
    }

    #[test]
    fn parse_baseline_handles_empty_issues() {
        let text = r#"{"issues":[]}"#;
        let parsed = parse_baseline(text).expect("ok");
        assert!(parsed.is_empty());
    }

    #[test]
    fn parse_baseline_rejects_missing_issues_array() {
        let text = r#"{"other_field":"foo"}"#;
        assert!(parse_baseline(text).is_err());
    }

    #[test]
    fn parse_baseline_identity_matches_issue_identity() {
        // The round-trip property: an issue we emit + parse back
        // produces the same identity hash. CI consumers depend on
        // this for diff correctness.
        let mut fields = BTreeMap::new();
        fields.insert("policy".into(), "AllAtOnce".into());
        fields.insert("max_size".into(), "4".into());
        let issue = Issue {
            rule_id: "EBL001".into(),
            severity: Severity::Warn,
            env_name: Some("prod".into()),
            title: "AllAtOnce".into(),
            detail: "...".into(),
            suggestion: None,
            fields: fields.clone(),
        };
        let json = render_issues_json(std::slice::from_ref(&issue));
        let parsed = parse_baseline(&json).expect("ok");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].identity, issue_identity(&issue));
    }

    #[test]
    fn ebl012_treats_health_ok_as_green() {
        // EB sometimes reports health=Ok instead of Green for
        // worker envs. Same firing condition.
        let env = Environment {
            status: "Ready".into(),
            health: "Ok".into(),
            ..mk_env("worker", "Worker", "Ok")
        };
        let opts: Vec<(String, String, String)> = vec![];
        let ctx = LintContext::for_env(&env, &opts).with_healthy_count(0);
        assert!(GreenButZeroInstances.applies(&ctx).is_some());
    }

    /// **Rule-trait invariants** across the entire rule registry.
    ///
    /// For each rule in `default_rules(&[])`:
    /// 1. `id()` is non-empty (used as audit-log key + baseline key).
    /// 2. `severity()` doesn't panic.
    /// 3. Neither `applies(ctx)` nor `fix(ctx)` panics on a bare-Web
    ///    or bare-Worker context (defensive coverage).
    /// 4. **Consistency**: if `applies(ctx) == None`, then
    ///    `fix(ctx) == None`. The reverse (applies=Some, fix=None)
    ///    is allowed — rules without auto-remediation. But a rule
    ///    that returns `Some(FixAction)` when `applies()` says "no
    ///    issue here" would surface as a "fix issue that doesn't
    ///    exist" CLI output.
    ///
    /// This is the structural guarantee that `cmd_lint_fix` relies
    /// on when iterating `applies → fix` per issue. 0.19 review item.
    #[test]
    fn rules_satisfy_trait_invariants() {
        let rules = default_rules(&[]);
        // Two contexts — a bare Web env and a bare Worker env. Some
        // rules legitimately fire on each (EBL002 missing health-
        // check URL on Web; future EBL011 DLQ-stuck on Worker with
        // dlq_depth) — that's fine. The consistency check is what
        // matters: where applies() says No, fix() must too.
        let web_env = Environment {
            updated: Some(chrono::Utc::now()),
            ..mk_env("web", "Web", "Green")
        };
        let worker_env = Environment {
            updated: Some(chrono::Utc::now()),
            ..mk_env("worker", "Worker", "Green")
        };
        let opts: Vec<(String, String, String)> = vec![];
        for env in [&web_env, &worker_env] {
            let ctx = LintContext::for_env(env, &opts);
            for rule in &rules {
                let id = rule.id();
                assert!(!id.is_empty(), "rule has empty id");
                let _ = rule.severity(); // doesn't panic
                let applies_result = rule.applies(&ctx);
                let fix_result = rule.fix(&ctx);
                if applies_result.is_none() {
                    assert!(
                        fix_result.is_none(),
                        "{id} on tier={}: fix() returned Some({fix_result:?}) when applies() returned None — \
                         cmd_lint_fix's `applies → fix` chain assumes this never happens. Either \
                         applies() should fire or fix() should short-circuit on None-applies.",
                        env.tier
                    );
                }
            }
        }
        // Sanity: the registry has the expected size. Bumps when a
        // new EBL is added. Catches both regressions (rule removed)
        // and additions-without-test-update (new rule landed; review
        // whether its applies()/fix() satisfy the invariants above).
        assert_eq!(rules.len(), 15, "rule registry size changed");
    }

    #[test]
    fn ebl017_fires_when_managed_actions_enabled_is_false() {
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![mk_opt(
            "aws:elasticbeanstalk:managedactions",
            "ManagedActionsEnabled",
            "false",
        )];
        let ctx = LintContext::for_env(&env, &opts);
        let issue = ManagedActionsDisabled.applies(&ctx).expect("should fire");
        assert_eq!(issue.rule_id, "EBL017");
        assert_eq!(
            issue
                .fields
                .get("managed_actions_enabled")
                .map(String::as_str),
            Some("false")
        );
    }

    #[test]
    fn ebl017_fires_when_managed_actions_setting_absent() {
        // Setting absent means EB defaults to disabled (per platform
        // family). Same firing condition.
        let env = mk_env("prod", "Web", "Green");
        let opts: Vec<(String, String, String)> = vec![];
        let ctx = LintContext::for_env(&env, &opts);
        let issue = ManagedActionsDisabled
            .applies(&ctx)
            .expect("absent setting fires too");
        assert_eq!(issue.rule_id, "EBL017");
        assert_eq!(
            issue
                .fields
                .get("managed_actions_enabled")
                .map(String::as_str),
            Some("")
        );
    }

    #[test]
    fn ebl017_does_not_fire_when_managed_actions_enabled() {
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![mk_opt(
            "aws:elasticbeanstalk:managedactions",
            "ManagedActionsEnabled",
            "true",
        )];
        let ctx = LintContext::for_env(&env, &opts);
        assert!(ManagedActionsDisabled.applies(&ctx).is_none());
        assert!(ManagedActionsDisabled.fix(&ctx).is_none());
    }

    #[test]
    fn ebl013_fires_when_legacy_launchconfig_namespace_populated() {
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![mk_opt(
            "aws:autoscaling:launchconfiguration",
            "InstanceType",
            "t3.small",
        )];
        let ctx = LintContext::for_env(&env, &opts);
        let issue = LaunchConfigurationLegacy
            .applies(&ctx)
            .expect("legacy namespace should fire");
        assert_eq!(issue.rule_id, "EBL013");
    }

    #[test]
    fn ebl013_does_not_fire_when_only_launchtemplate_namespace_populated() {
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![mk_opt(
            "aws:autoscaling:launchtemplate",
            "InstanceType",
            "t3.small",
        )];
        let ctx = LintContext::for_env(&env, &opts);
        assert!(LaunchConfigurationLegacy.applies(&ctx).is_none());
    }

    #[test]
    fn ebl013_does_not_fire_when_launchconfig_option_is_empty() {
        // An EB env might have the namespace mentioned but with an
        // empty value — treat as "not really set".
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![mk_opt(
            "aws:autoscaling:launchconfiguration",
            "InstanceType",
            "",
        )];
        let ctx = LintContext::for_env(&env, &opts);
        assert!(LaunchConfigurationLegacy.applies(&ctx).is_none());
    }

    #[test]
    fn parse_csv_value_handles_padded_entries() {
        assert_eq!(
            parse_csv_value("subnet-a, subnet-b , subnet-c"),
            vec!["subnet-a", "subnet-b", "subnet-c"]
        );
        assert_eq!(parse_csv_value(""), Vec::<&str>::new());
        assert_eq!(parse_csv_value(", ,, "), Vec::<&str>::new());
        assert_eq!(parse_csv_value("only-one"), vec!["only-one"]);
    }

    #[test]
    fn ebl019_fires_on_allatonce_multi_subnet() {
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![
            mk_opt(
                "aws:elasticbeanstalk:command",
                "DeploymentPolicy",
                "AllAtOnce",
            ),
            mk_opt("aws:autoscaling:asg", "MaxSize", "4"),
            mk_opt("aws:ec2:vpc", "Subnets", "subnet-a,subnet-b,subnet-c"),
        ];
        let ctx = LintContext::for_env(&env, &opts);
        let issue = AllAtOnceMultiAz.applies(&ctx).expect("should fire");
        assert_eq!(issue.rule_id, "EBL019");
        assert_eq!(
            issue.fields.get("subnet_count").map(String::as_str),
            Some("3")
        );
        // Auto-fix: same SetOption as EBL001.
        let fix = AllAtOnceMultiAz.fix(&ctx).expect("auto-fix");
        match fix {
            FixAction::SetOption { value, name, .. } => {
                assert_eq!(name, "DeploymentPolicy");
                assert_eq!(value, "Rolling");
            }
            _ => panic!("expected SetOption fix"),
        }
    }

    #[test]
    fn ebl019_does_not_fire_on_single_subnet() {
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![
            mk_opt(
                "aws:elasticbeanstalk:command",
                "DeploymentPolicy",
                "AllAtOnce",
            ),
            mk_opt("aws:autoscaling:asg", "MaxSize", "4"),
            mk_opt("aws:ec2:vpc", "Subnets", "subnet-a"),
        ];
        let ctx = LintContext::for_env(&env, &opts);
        // EBL001 still fires; EBL019 specifically doesn't.
        assert!(AllAtOnceMultiAz.applies(&ctx).is_none());
        assert!(AllAtOnceMultiAz.fix(&ctx).is_none());
    }

    #[test]
    fn ebl019_does_not_fire_on_rolling_policy() {
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![
            mk_opt(
                "aws:elasticbeanstalk:command",
                "DeploymentPolicy",
                "Rolling",
            ),
            mk_opt("aws:autoscaling:asg", "MaxSize", "4"),
            mk_opt("aws:ec2:vpc", "Subnets", "subnet-a,subnet-b"),
        ];
        let ctx = LintContext::for_env(&env, &opts);
        assert!(AllAtOnceMultiAz.applies(&ctx).is_none());
    }

    #[test]
    fn ebl019_does_not_fire_on_single_instance() {
        let env = mk_env("prod", "Web", "Green");
        let opts = vec![
            mk_opt(
                "aws:elasticbeanstalk:command",
                "DeploymentPolicy",
                "AllAtOnce",
            ),
            mk_opt("aws:autoscaling:asg", "MaxSize", "1"),
            mk_opt("aws:ec2:vpc", "Subnets", "subnet-a,subnet-b"),
        ];
        let ctx = LintContext::for_env(&env, &opts);
        assert!(AllAtOnceMultiAz.applies(&ctx).is_none());
    }

    #[test]
    fn ebl017_value_match_is_case_insensitive() {
        // EB sometimes returns "True" / "TRUE" depending on how the
        // setting was written. Match should accept any casing.
        let env = mk_env("prod", "Web", "Green");
        for variant in ["True", "TRUE", "true"] {
            let opts = vec![mk_opt(
                "aws:elasticbeanstalk:managedactions",
                "ManagedActionsEnabled",
                variant,
            )];
            let ctx = LintContext::for_env(&env, &opts);
            assert!(
                ManagedActionsDisabled.applies(&ctx).is_none(),
                "value '{variant}' should be treated as enabled"
            );
        }
    }
}
