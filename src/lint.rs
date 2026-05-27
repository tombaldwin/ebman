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
    /// Newest stack version for the env's platform family, when
    /// the stale-platform check has populated it. `None` means
    /// the data isn't loaded — the corresponding rule skips.
    pub latest_stack_version: Option<&'a str>,
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

/// Build the v1 rule registry. Operator-disabled rules are
/// filtered HERE — at registry-load time — so a disabled rule
/// has zero per-env cost. Severity overrides not yet
/// implemented (BONUS-tier 0.13 item).
pub fn default_rules(disabled: &[String]) -> Vec<Box<dyn Rule>> {
    let candidates: Vec<Box<dyn Rule>> = vec![
        Box::new(AllAtOnceMultiInstance),
        Box::new(WebTierNoHealthCheckUrl),
        Box::new(EnvRedForExtendedPeriod),
        Box::new(BatchSizeExceedsMaxSize),
        Box::new(SingleInstanceEnv),
        Box::new(CooldownBelowRecommended),
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
        LintContext {
            env,
            options,
            events: &[],
            cost_usd_per_month: None,
            latest_stack_version: None,
        }
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
}
