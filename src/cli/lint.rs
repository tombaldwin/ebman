//! `ebman lint [--env NAME] [--regions r1,r2,r3] [--json] [--severity LVL]
//! [--rules ID1,ID2] [--quiet] [--fix (--yes | --dry-run)]` —
//! rule-engine diagnostics for git hooks / CI gates / monitoring,
//! with opt-in auto-remediation via `--fix`.
//!
//! Exit codes (per the 0.13 CLI charter):
//! - 0 clean / fix applied successfully
//! - 1 AWS-layer error (or `--fix` dispatch failure)
//! - 2 usage error
//! - 3 issues found (NOT used in `--fix` mode — operator's intent
//!   is "see issues then fix them"; a clean apply stays exit 0)
//!
//! `--fix` dispatches each rule's auto-remediation through the same
//! `update_env_option_settings` path the TUI uses. Respects
//! `safety.envs.NAME.read_only` + `safety.accounts.NAME.read_only`
//! pins (matched against `AWS_PROFILE`) so a TUI-locked env can't
//! be written from the CLI. Per-rule opt-out via `lint.fix_disable`.

use color_eyre::eyre::Result;

use crate::{audit, aws, config, lint, project};

/// Tracks whether any `--fix` dispatch failed during the run. Single
/// process-wide flag — CLI exits after `run` returns, so cross-run
/// state isn't a concern. Lives next to its sole reader/writer
/// (`run`).
static FIX_DISPATCH_FAILED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Print `--against-baseline --json` diff body. Hand-rolled to
/// avoid pulling serde_json; uses `crate::util::json_string` for
/// the value escapes. Shape:
///
/// ```json
/// {
///   "new": [{ "rule_id": "...", "env": "...", "title": "..." }, ...],
///   "cleared": [{ "rule_id": "...", "env": "...", "title": "..." }, ...]
/// }
/// ```
fn print_baseline_diff_json(new: &[&lint::Issue], cleared: &[&lint::BaselineIssue]) {
    let mut out = String::from("{\"new\":[");
    for (i, issue) in new.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"rule_id\":{},\"env\":{},\"title\":{}}}",
            crate::util::json_string(&issue.rule_id),
            crate::util::json_string(issue.env_name.as_deref().unwrap_or("")),
            crate::util::json_string(&issue.title),
        ));
    }
    out.push_str("],\"cleared\":[");
    for (i, b) in cleared.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"rule_id\":{},\"env\":{},\"title\":{}}}",
            crate::util::json_string(&b.rule_id),
            crate::util::json_string(b.env_name.as_deref().unwrap_or("")),
            crate::util::json_string(&b.title),
        ));
    }
    out.push_str("]}");
    println!("{out}");
}

/// Fully-resolved `ebman lint` arguments: flags parsed, interval and
/// region-CSV resolved, and all cross-flag validation already passed.
/// Separated from [`run`] so the whole parse+validate surface (every
/// exit-2 usage path) is unit-testable without `std::process::exit` or
/// the live config/AWS I/O that follows it.
#[derive(Debug, PartialEq, Eq)]
struct LintArgs {
    env_name: Option<String>,
    regions: Vec<Option<String>>,
    json: bool,
    quiet: bool,
    severity_filter: Option<lint::Severity>,
    rule_filter: Vec<String>,
    fix: bool,
    dry_run: bool,
    yes: bool,
    watch: bool,
    interval_secs: u64,
    baseline_write: Option<String>,
    baseline_against: Option<String>,
    probe_live: bool,
    webhook: Option<String>,
}

/// EBL020 input probe: when the env has `XRayEnabled=true`, resolve
/// its instance-profile role and IAM-simulate `xray:PutTraceSegments`
/// against it. `Some(true)` = denied (the rule's firing signal),
/// `Some(false)` = allowed, `None` = X-Ray off / no profile / probe
/// failed (the rule skips — never a false positive from a failed
/// probe). Lives at the call site rather than in `LintContext`
/// because rules are pure and synchronous.
/// What a permission probe actually learned.
///
/// These were `Option<bool>`, where `None` meant four different things:
/// the rule doesn't apply, there is no instance profile, the IAM call
/// failed, or the result was empty. The first two are a legitimate
/// skip. The others are "we could not check" — and collapsing them into
/// the same value made an `AccessDenied` on
/// `iam:SimulatePrincipalPolicy` indistinguishable from a clean bill of
/// health.
///
/// That matters because `ebman lint --json` is a CI gate and an MCP
/// tool result an agent treats as authoritative. "Clean" because IAM
/// denied the probe is not a smaller answer, it is a wrong one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProbeOutcome {
    /// The rule doesn't apply here — X-Ray is off, no instance profile,
    /// not a prod ALB env. A silent skip is correct.
    NotApplicable,
    /// The probe ran. `true` means the rule should fire.
    Checked(bool),
    /// The probe could not run. The rule skips, but says so.
    Unknown(String),
}

impl ProbeOutcome {
    /// The rule's verdict, or `None` when there wasn't one.
    pub(crate) fn verdict(&self) -> Option<bool> {
        match self {
            Self::Checked(v) => Some(*v),
            _ => None,
        }
    }

    /// The warning to surface when coverage silently shrank.
    pub(crate) fn coverage_warning(&self, rule: &str, env: &str) -> Option<String> {
        match self {
            Self::Unknown(why) => Some(format!(
                "{rule} could not be evaluated for {env}: {why} — this is \
                 NOT a clean result, the check did not run"
            )),
            _ => None,
        }
    }
}

async fn probe_xray_trace_denied(
    aws: &aws::AwsClient,
    options: &[(String, String, String)],
    disabled: &[String],
) -> ProbeOutcome {
    // The rule's own opt-out, checked inside the probe rather than at
    // the call site. A caller that forgets makes the documented escape
    // hatch stop working — and once a failed probe marks the run
    // degraded, that means a red pipeline for a rule the operator
    // switched off, with no remedy short of changing IAM. Here it
    // cannot be forgotten.
    if disabled.iter().any(|d| d == "EBL020") {
        return ProbeOutcome::NotApplicable;
    }
    let xray_on = options.iter().any(|(ns, n, v)| {
        ns == "aws:elasticbeanstalk:xray" && n == "XRayEnabled" && v.eq_ignore_ascii_case("true")
    });
    if !xray_on {
        return ProbeOutcome::NotApplicable;
    }
    let Some(profile) = options.iter().find_map(|(ns, n, v)| {
        (ns == "aws:autoscaling:launchconfiguration" && n == "IamInstanceProfile" && !v.is_empty())
            .then(|| v.clone())
    }) else {
        return ProbeOutcome::NotApplicable;
    };
    let role_arn = match aws.instance_profile_role_arn(&profile).await {
        Ok(Some(a)) => a,
        // No role on the profile is "doesn't apply"; a failed lookup is
        // not, and used to be the same value.
        Ok(None) => return ProbeOutcome::NotApplicable,
        Err(e) => return ProbeOutcome::Unknown(format!("instance-profile lookup failed: {e}")),
    };
    // `.complete()`, not `.items()`: one action can't truncate in
    // practice, but if it ever did the empty result would read as
    // "no decision" and the rule would silently skip. An error here
    // skips too — but visibly, via the probe's `None`.
    let results = match aws
        .simulate_principal_policy(&role_arn, &["xray:PutTraceSegments".to_string()], &[])
        .await
    {
        Ok(p) => match p.complete("X-Ray permission probe") {
            Ok(r) => r,
            Err(e) => return ProbeOutcome::Unknown(format!("{e}")),
        },
        // The one that matters: AccessDenied on
        // iam:SimulatePrincipalPolicy used to read as "no finding".
        Err(e) => return ProbeOutcome::Unknown(format!("SimulatePrincipalPolicy failed: {e}")),
    };
    match results.first() {
        Some(first) => ProbeOutcome::Checked(!first.decision.eq_ignore_ascii_case("allowed")),
        None => ProbeOutcome::Unknown("policy simulation returned no decision".into()),
    }
}

/// EBL018 input probe: for a prod-named env fronted by an ALB, ask
/// WAFv2 whether a WebACL is associated. `Some(true)` = no WAF (the
/// rule's firing signal), `Some(false)` = WAF present, `None` =
/// non-prod name / classic-or-network LB / no ALB ARN resolvable /
/// probe failed (the rule skips — never a false positive). Classic
/// ELBs are structurally out: WAFv2 can't associate with them.
async fn probe_waf_missing(
    aws: &aws::AwsClient,
    env: &aws::Environment,
    options: &[(String, String, String)],
    disabled: &[String],
) -> ProbeOutcome {
    // Same reasoning as EBL020's probe: the opt-out lives here.
    if disabled.iter().any(|d| d == "EBL018") {
        return ProbeOutcome::NotApplicable;
    }
    if !lint::is_prod_named(&env.name) {
        return ProbeOutcome::NotApplicable;
    }
    let alb = options.iter().any(|(ns, n, v)| {
        ns == "aws:elasticbeanstalk:environment"
            && n == "LoadBalancerType"
            && v.eq_ignore_ascii_case("application")
    });
    if !alb {
        return ProbeOutcome::NotApplicable;
    }
    let resources = match aws.describe_env_resources(&env.name).await {
        Ok(r) => r,
        Err(e) => return ProbeOutcome::Unknown(format!("DescribeEnvironmentResources: {e}")),
    };
    // For ALBs, DescribeEnvironmentResources reports the full ARN in
    // the name slot (classic ELBs report a bare name — filtered here).
    // No ARN means no ALB to check, which genuinely doesn't apply.
    let Some(alb_arn) = resources
        .load_balancers
        .iter()
        .find(|n| n.starts_with("arn:"))
    else {
        return ProbeOutcome::NotApplicable;
    };
    match aws.web_acl_for_resource(alb_arn).await {
        Ok(acl) => ProbeOutcome::Checked(acl.is_none()),
        // "No WAF associated" and "we were not allowed to look" are
        // very different answers to a security rule.
        Err(e) => ProbeOutcome::Unknown(format!("GetWebACLForResource: {e}")),
    }
}

/// Owned per-env lint inputs — everything a `LintContext` borrows,
/// fetched and held in one place. Extracted (0.26) so `ebman lint`
/// and the MCP `lint` tool share a single assembly path instead of
/// each growing its own copy of the fetch + probe choreography.
/// `dlq_depth` is deliberately absent: the CLI doesn't poll worker
/// queues, so EBL011 stays TUI-only (stated in the MCP tool's
/// coverage caveats).
pub(crate) struct EnvLintInputs {
    pub options: Vec<(String, String, String)>,
    /// `None` = the tag fetch failed or wasn't attempted, so EBL010
    /// skips. `Some(vec![])` = fetched successfully and the env has no
    /// tags, which fires. Flattening these together handed the rule a
    /// successful-but-empty result on every failure.
    pub env_tag_keys: Option<Vec<String>>,
    pub healthy_count: Option<i64>,
    pub xray_denied: Option<bool>,
    pub probe_failure: Option<String>,
    pub newer_stack: Option<String>,
    pub waf_missing: Option<bool>,
    /// Checks that could NOT run, with why.
    ///
    /// The rules still skip on a failed probe — that part was right, a
    /// failed probe must never become a false positive. What was wrong
    /// is that the skip was silent, so `--json` reported the same thing
    /// for "checked, clean" and "IAM denied the probe". These carry the
    /// difference out to the operator.
    pub coverage_warnings: Vec<String>,
}

impl EnvLintInputs {
    /// Inputs over just option settings, every probe unset (demo
    /// mode, tests). New probe fields default here so adding one
    /// doesn't mean editing every all-`None` literal.
    pub(crate) fn bare(options: Vec<(String, String, String)>) -> Self {
        Self {
            options,
            env_tag_keys: None,
            healthy_count: None,
            xray_denied: None,
            probe_failure: None,
            newer_stack: None,
            waf_missing: None,
            coverage_warnings: Vec::new(),
        }
    }
}

/// Fetch one env's lint inputs: parallel option-settings + tags +
/// instance-counts (matching the TUI's `spawn_confirm_lint`
/// plumbing), then the two gated probes (EBL020 IAM sim when X-Ray
/// is on; EBL016 HTTP probe when `probe_live`). Tags and health are
/// tolerated independently — a missing input means the corresponding
/// rule doesn't fire. `Err` carries the option-settings fetch error,
/// the one input lint can't run without.
pub(crate) async fn fetch_env_lint_inputs(
    aws: &aws::AwsClient,
    env: &aws::Environment,
    latest_stacks: &std::collections::HashMap<String, String>,
    probe_live: bool,
    // Rules the operator switched off (`lint.disable`, `--rules`). A
    // disabled rule must not run its probe: without this it still
    // fired, still paid the IAM / WAF calls, and — once a failed probe
    // started marking the run degraded — still turned the pipeline red
    // for a rule the operator had explicitly opted out of, with no
    // remedy short of changing IAM. The documented escape hatch has to
    // actually be one.
    disabled: &[String],
) -> Result<EnvLintInputs, String> {
    let opts_fut = aws.fetch_env_option_settings(&env.application, &env.name);
    let tags_fut = async {
        match env.arn.as_deref() {
            Some(arn) => aws.list_tags(arn).await.ok(),
            None => None,
        }
    };
    let health_fut = aws.fetch_env_instance_counts(&env.name);
    let (opts_res, tags_opt, health_res) = tokio::join!(opts_fut, tags_fut, health_fut);
    let options = opts_res.map_err(|e| e.to_string())?;
    let env_tag_keys: Option<Vec<String>> =
        tags_opt.map(|t| t.into_iter().map(|(k, _)| k).collect());
    let healthy_count = health_res.ok().map(|c| c.healthy as i64);
    let newer_stack = aws::newer_stack_version(&env.solution_stack, latest_stacks);
    // EBL020 probe — only when the env actually has X-Ray on (rare),
    // so the common path pays no IAM calls. Probe failures leave the
    // field unset: skip, never false-positive.
    let xray_outcome = probe_xray_trace_denied(aws, &options, disabled).await;
    // EBL018 probe — only for prod-named ALB envs (both gates checked
    // inside), so the common path pays no WAF calls.
    let waf_outcome = probe_waf_missing(aws, env, &options, disabled).await;
    // EBL016 probe — opt-in via `probe_live` (one curl HEAD per env
    // is too slow for default lint). Only a FAILURE is recorded.
    let probe_failure: Option<String> = if probe_live && !env.cname.is_empty() {
        let path = options
            .iter()
            .find_map(|(ns, n, v)| {
                (ns == "aws:elasticbeanstalk:application"
                    && n == "Application Healthcheck URL"
                    && !v.is_empty())
                .then(|| v.clone())
            })
            .unwrap_or_else(|| "/".to_string());
        let url = crate::probe::build_health_check_probe_url(&env.cname, &path);
        crate::probe::run_health_check_probe(&url).await.err()
    } else {
        None
    };
    let coverage_warnings = [
        xray_outcome.coverage_warning("EBL020", &env.name),
        waf_outcome.coverage_warning("EBL018", &env.name),
    ]
    .into_iter()
    .flatten()
    .collect();
    Ok(EnvLintInputs {
        options,
        env_tag_keys,
        healthy_count,
        xray_denied: xray_outcome.verdict(),
        probe_failure,
        newer_stack,
        waf_missing: waf_outcome.verdict(),
        coverage_warnings,
    })
}

/// EBL015 account-level assembly, shared by `run` and the MCP `lint`
/// tool: list custom platforms, resolve each branch's newest version
/// date via `latest_platform_version_date`, and run the pure
/// staleness pass. Returns the issues plus per-branch warnings for
/// branches whose date fetch failed (the CLI prints them unless
/// `--quiet`; MCP drops them — a tool result shouldn't fail over an
/// Info-severity side pass). `Err` carries the ListPlatformVersions
/// failure. Callers gate on scope (`--env` skips) + `lint.disable`.
pub(crate) async fn fetch_stale_platform_issues(
    aws: &aws::AwsClient,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(Vec<lint::Issue>, Vec<String>), String> {
    let platforms = aws
        .list_custom_platforms()
        .await
        .map_err(|e| e.to_string())?;
    let mut by_branch: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for p in platforms {
        by_branch.entry(p.branch.clone()).or_default().push(p.arn);
    }
    let mut dated: Vec<(String, chrono::DateTime<chrono::Utc>)> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    for (branch, arns) in by_branch {
        match aws.latest_platform_version_date(&arns).await {
            Ok(Some(latest)) => dated.push((branch, latest)),
            // No version reported a date: skip, never false-fire.
            Ok(None) => {}
            Err(e) => warnings.push(format!(
                "EBL015 skipped for '{branch}' — DescribePlatformVersion: {e}"
            )),
        }
    }
    Ok((lint::stale_custom_platform_issues(&dated, now), warnings))
}

/// Pure: assemble a borrowing `LintContext` over fetched inputs.
/// Shared by [`run_rules_for_env`] and `run`'s `--fix` path (which
/// needs the context again for `rule.fix(&ctx)`).
pub(crate) fn build_lint_context<'a>(
    env: &'a aws::Environment,
    inputs: &'a EnvLintInputs,
    required_tags: &'a [String],
) -> lint::LintContext<'a> {
    let mut ctx =
        lint::LintContext::for_env(env, &inputs.options).with_required_tags(required_tags);
    if let Some(keys) = inputs.env_tag_keys.as_deref() {
        ctx = ctx.with_env_tag_keys(keys);
    }
    if let Some(newer) = inputs.newer_stack.as_deref() {
        ctx = ctx.with_newer_stack_available(newer);
    }
    if let Some(count) = inputs.healthy_count {
        ctx = ctx.with_healthy_count(count);
    }
    if let Some(denied) = inputs.xray_denied {
        ctx = ctx.with_xray_trace_denied(denied);
    }
    if let Some(reason) = inputs.probe_failure.as_deref() {
        ctx = ctx.with_health_probe_failure(reason);
    }
    if let Some(missing) = inputs.waf_missing {
        ctx = ctx.with_waf_missing(missing);
    }
    ctx
}

/// Pure: build the `LintContext` over fetched inputs and run the
/// rule set. The second half of the shared assembly path — both
/// `run` and the MCP `lint` tool call this after
/// [`fetch_env_lint_inputs`].
pub(crate) fn run_rules_for_env(
    rules: &[Box<dyn lint::Rule>],
    env: &aws::Environment,
    inputs: &EnvLintInputs,
    required_tags: &[String],
) -> Vec<lint::Issue> {
    lint::run_rules(rules, &build_lint_context(env, inputs, required_tags))
}

/// Pure: one-line webhook body for a lint cycle. Caps at 5 issues so
/// a noisy fleet doesn't blow out the Slack message; an empty set
/// renders the all-clear (sent on the dirty→clean transition).
fn webhook_summary(issues: &[lint::Issue]) -> String {
    if issues.is_empty() {
        return "lint: ✓ clean (previous issues cleared)".to_string();
    }
    let mut parts: Vec<String> = issues
        .iter()
        .take(5)
        .map(|i| {
            format!(
                "{} {} {}: {}",
                i.severity.as_str(),
                i.rule_id,
                i.env_name.as_deref().unwrap_or("-"),
                i.title
            )
        })
        .collect();
    if issues.len() > 5 {
        parts.push(format!("…and {} more", issues.len() - 5));
    }
    format!("lint: {} issue(s) — {}", issues.len(), parts.join("; "))
}

/// Pure parser + validator for `ebman lint`. Returns `Err(msg)` for
/// every usage error (all exit-2 here, so the code is left implicit).
/// Ordering note: validation runs here, before [`run`] loads config —
/// a usage error now exits before the (silent) config read rather than
/// after. No observable change; strictly less wasted work.
fn parse_lint_args(args: &[String]) -> Result<LintArgs, String> {
    let mut env_name: Option<String> = None;
    let mut regions_csv: Option<String> = None;
    let mut json = false;
    let mut quiet = false;
    let mut severity_filter: Option<lint::Severity> = None;
    let mut rule_filter: Vec<String> = Vec::new();
    let mut fix = false;
    let mut dry_run = false;
    let mut yes = false;
    let mut watch = false;
    let mut interval_str: Option<String> = None;
    let mut baseline_write: Option<String> = None;
    let mut baseline_against: Option<String> = None;
    let mut probe_live = false;
    let mut webhook: Option<String> = None;
    let mut iter = args.iter().skip(1);
    // Every value-taking flag rejects a missing value or a following
    // flag. Silently swallowing either was dangerous: a forgotten
    // `--env` value on `--fix --yes` widened scope to the whole
    // fleet, and `--rules --json` filtered every issue out (exit 0)
    // while eating the JSON flag.
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--env" => {
                env_name = Some(crate::cli::take_value(
                    &mut iter,
                    "ebman lint",
                    "--env",
                    "an env name",
                )?)
            }
            "--regions" => {
                regions_csv = Some(crate::cli::take_value(
                    &mut iter,
                    "ebman lint",
                    "--regions",
                    "a region list",
                )?)
            }
            "--json" => json = true,
            "--quiet" => quiet = true,
            "--fix" => fix = true,
            "--dry-run" => dry_run = true,
            "--yes" => yes = true,
            "--watch" => watch = true,
            "--interval" => {
                interval_str = Some(crate::cli::take_value(
                    &mut iter,
                    "ebman lint",
                    "--interval",
                    "a duration",
                )?)
            }
            "--probe-live" => probe_live = true,
            "--webhook" => {
                let Some(u) = iter.next() else {
                    return Err("ebman lint: --webhook expects a URL".into());
                };
                if u.starts_with("--") {
                    return Err(format!(
                        "ebman lint: --webhook expects a URL, got flag '{u}'"
                    ));
                }
                webhook = Some(u.clone());
            }
            "--baseline" => {
                let Some(p) = iter.next() else {
                    return Err("ebman lint: --baseline expects a file path".into());
                };
                if p.starts_with("--") {
                    return Err(format!(
                        "ebman lint: --baseline expects a file path, got flag '{p}'"
                    ));
                }
                baseline_write = Some(p.clone());
            }
            "--against-baseline" => {
                let Some(p) = iter.next() else {
                    return Err("ebman lint: --against-baseline expects a file path".into());
                };
                if p.starts_with("--") {
                    return Err(format!(
                        "ebman lint: --against-baseline expects a file path, got flag '{p}'"
                    ));
                }
                baseline_against = Some(p.clone());
            }
            "--severity" => {
                let Some(v) = iter.next() else {
                    return Err(
                        "ebman lint: --severity expects a value (info / warn / error)".into(),
                    );
                };
                let Some(sev) = lint::Severity::parse(v) else {
                    return Err(format!(
                        "ebman lint: unknown severity '{v}' (info / warn / error)"
                    ));
                };
                severity_filter = Some(sev);
            }
            "--rules" => {
                let v = crate::cli::take_value(
                    &mut iter,
                    "ebman lint",
                    "--rules",
                    "a comma-separated rule id list",
                )?;
                rule_filter = crate::util::split_csv(&v);
                if rule_filter.is_empty() {
                    return Err(format!("ebman lint: --rules got '{v}' — no rule ids in it"));
                }
            }
            other => {
                return Err(format!("ebman lint: unknown flag '{other}'"));
            }
        }
    }

    if watch && fix {
        return Err("ebman lint: --watch and --fix are mutually exclusive (use one)".into());
    }
    if webhook.is_some() && !watch {
        return Err(
            "ebman lint: --webhook only makes sense with --watch (one-shot runs print their findings)"
                .into(),
        );
    }
    if baseline_write.is_some() && baseline_against.is_some() {
        return Err(
            "ebman lint: --baseline (write) and --against-baseline (compare) are mutually exclusive"
                .into(),
        );
    }
    if (baseline_write.is_some() || baseline_against.is_some()) && (fix || watch) {
        return Err(
            "ebman lint: --baseline / --against-baseline are incompatible with --fix / --watch"
                .into(),
        );
    }
    if fix && !yes && !dry_run {
        return Err(
            "ebman lint --fix: requires --yes to dispatch writes (or --dry-run to preview)".into(),
        );
    }
    if fix && yes && dry_run {
        return Err("ebman lint --fix: --yes and --dry-run are mutually exclusive".into());
    }
    // Default interval = 60s. Parse the same way other deadlines
    // are parsed (`5m / 30m / 1h`); accept a bare integer as
    // seconds for monitoring-friendly shapes like `--interval 30`.
    let interval_secs: u64 = match interval_str.as_deref() {
        None => 60,
        Some(s) => {
            if let Ok(n) = s.parse::<u64>() {
                if n == 0 {
                    return Err("ebman lint: --interval must be > 0".into());
                }
                n
            } else if let Some(ms) = aws::parse_window_ms(s) {
                ((ms / 1000) as u64).max(1)
            } else {
                return Err(
                    "ebman lint: --interval expects seconds (`30`) or a duration (`5m`/`1h`)"
                        .into(),
                );
            }
        }
    };

    let regions: Vec<Option<String>> = match regions_csv {
        Some(csv) => {
            let parsed: Vec<String> = crate::util::split_csv(&csv);
            if parsed.is_empty() {
                return Err("ebman lint: --regions list is empty".into());
            }
            parsed.into_iter().map(Some).collect()
        }
        None => vec![None],
    };

    Ok(LintArgs {
        env_name,
        regions,
        json,
        quiet,
        severity_filter,
        rule_filter,
        fix,
        dry_run,
        yes,
        watch,
        interval_secs,
        baseline_write,
        baseline_against,
        probe_live,
        webhook,
    })
}

pub async fn run(args: &[String]) -> Result<()> {
    let LintArgs {
        env_name,
        regions,
        json,
        quiet,
        severity_filter,
        rule_filter,
        fix,
        // `dry_run` is consumed entirely by the parser's validation
        // (--fix needs --yes XOR --dry-run); the apply path below keys
        // on `yes` alone, so it isn't bound here.
        dry_run: _,
        yes,
        watch,
        interval_secs,
        baseline_write,
        baseline_against,
        probe_live,
        webhook,
    } = match parse_lint_args(args) {
        Ok(parsed) => parsed,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };

    let mut disabled: Vec<String> = config::load_lint_disables();
    disabled.extend(project::load_lint_disables_from_cwd());
    let rules = lint::default_rules(&disabled);

    let mut fix_disabled: Vec<String> = config::load_lint_fix_disables();
    fix_disabled.extend(project::load_lint_fix_disables_from_cwd());

    let safety_cfg = config::load();
    let active_profile_for_safety = std::env::var("AWS_PROFILE").ok();
    // Cross-process fleet freeze: a real fix dispatch (--fix --yes)
    // must refuse while a live TUI session holds :freeze-deploys /
    // :incident (a --dry-run plans nothing, so it stays allowed).
    // This path had the same blind spot action/replay had.
    if fix && yes {
        crate::cli::refuse_if_frozen("ebman lint --fix");
    }
    if webhook.is_some() {
        // CLI mode installs no tracing subscriber — route webhook
        // delivery failures to stderr so a broken URL isn't silent.
        audit::webhook_errors_to_stderr();
    }

    let multi_region = regions.len() > 1;
    // `--watch` wraps the existing one-shot body in a polling loop
    // that emits each cycle's issues and sleeps `interval_secs`.
    // Ctrl-C breaks; the exit code reflects the LAST cycle's state
    // so a clean shutdown after a clean cycle exits 0, after a
    // dirty cycle exits 3.
    // Tracks the most-recent cycle's "no issues found" state.
    // Initialised here so the post-loop exit-code branch can read
    // it even if the loop somehow exits without running a full
    // cycle (currently impossible — the unconditional first
    // iteration always sets it — but the initial value keeps the
    // borrow checker honest and documents the invariant).
    let mut last_cycle_clean;
    // Tracks whether the most-recent cycle skipped any region/env on
    // a fetch failure. A degraded "clean" run must exit 1 (the
    // documented AWS-error code), not 0 — expired credentials in a
    // CI gate previously produced a silent green pass.
    let mut last_cycle_degraded;
    // `--webhook` change-guard: identity set of the last cycle POSTed.
    // `None` until the first cycle, so the first findings (or first
    // clean state) always fire once.
    let mut last_webhook_identities: Option<std::collections::BTreeSet<String>> = None;
    // One ctrl_c future for the whole watch loop: creating a fresh
    // stream each iteration loses a SIGINT delivered mid-cycle (the
    // first ctrl_c() call overrides SIGINT's default disposition for
    // the process lifetime, and no listener is live while a cycle's
    // AWS fetches run — the keypress was silently swallowed).
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    loop {
        let cycle_started = chrono::Utc::now();
        if watch && !quiet && !json {
            println!("--- {} ---", cycle_started.to_rfc3339());
        }
        let mut all_issues: Vec<lint::Issue> = Vec::new();
        // A cycle that skipped any region/env (transient AWS failure)
        // has an incomplete issue set — the webhook change-guard must
        // neither page on it (a shrunk set reads as a false all-clear
        // mid-outage) nor adopt it as the new baseline.
        let mut cycle_degraded = false;
        for region_opt in &regions {
            let aws = match aws::AwsClient::with(None, region_opt.clone()).await {
                Ok(c) => c,
                Err(e) => {
                    if !quiet {
                        let region_label = region_opt.as_deref().unwrap_or("default");
                        eprintln!(
                            "warning: skipping region '{region_label}' — AwsClient::with: {e}"
                        );
                    }
                    cycle_degraded = true;
                    continue;
                }
            };
            let envs = match aws.list_environments().await {
                Ok(envs) => envs,
                Err(e) => {
                    if !quiet {
                        let region_label = region_opt.as_deref().unwrap_or("default");
                        eprintln!(
                            "warning: skipping region '{region_label}' — list_environments: {e}"
                        );
                    }
                    cycle_degraded = true;
                    continue;
                }
            };
            // Per-region one-shot fetch for EBL008 (stale platform):
            // `ListAvailableSolutionStacks` is region-scoped + cheap
            // (single call, no pagination). On failure we just skip
            // EBL008 for the region rather than aborting lint — same
            // tolerance pattern the per-env opts/tags/health fetches
            // use below. Added in 0.18 to close the TUI/CLI parity
            // gap noted in the 0.17.1 CHANGELOG.
            let latest_stacks = match aws.list_solution_stacks().await {
                Ok(s) => aws::latest_stack_versions(&s),
                Err(e) => {
                    if !quiet {
                        let region_label = region_opt.as_deref().unwrap_or("default");
                        eprintln!(
                            "warning: region '{region_label}' — list_solution_stacks failed: {e} (EBL008 skipped)"
                        );
                    }
                    std::collections::HashMap::new()
                }
            };

            let targets: Vec<&aws::Environment> = match env_name.as_deref() {
                Some(name) => match envs.iter().find(|e| e.name == name) {
                    Some(env) => vec![env],
                    None => {
                        if multi_region && !quiet {
                            let region_label = region_opt.as_deref().unwrap_or("default");
                            eprintln!(
                                "warning: env '{name}' not in region '{region_label}' — skipping"
                            );
                        } else if !multi_region {
                            eprintln!("ebman lint: env '{name}' not found in current context");
                            crate::cli::exit_after_drain(2).await;
                        }
                        continue;
                    }
                },
                None => envs.iter().collect(),
            };

            for env in targets {
                // Fetch + build + run via the shared assembly path
                // (`fetch_env_lint_inputs` / `run_rules_for_env`) —
                // the same pair the MCP `lint` tool calls.
                let inputs =
                    match fetch_env_lint_inputs(&aws, env, &latest_stacks, probe_live, &disabled)
                        .await
                    {
                        Ok(inputs) => inputs,
                        Err(e) => {
                            if !quiet {
                                eprintln!(
                                    "warning: skipping {} — fetch_env_option_settings: {e}",
                                    env.name
                                );
                            }
                            cycle_degraded = true;
                            continue;
                        }
                    };
                // A probe that could not run is not a clean result. The
                // rule still skips — a failed probe must never become a
                // false positive — but silence here made an
                // AccessDenied on iam:SimulatePrincipalPolicy look
                // identical to a passing check, in output that gates CI.
                if !inputs.coverage_warnings.is_empty() {
                    cycle_degraded = true;
                    if !quiet {
                        for w in &inputs.coverage_warnings {
                            eprintln!("warning: {w}");
                        }
                    }
                }
                let mut issues = run_rules_for_env(&rules, env, &inputs, &safety_cfg.required_tags);
                if let Some(min) = severity_filter {
                    issues.retain(|i| i.severity >= min);
                }
                if !rule_filter.is_empty() {
                    issues.retain(|i| rule_filter.contains(&i.rule_id));
                }
                if let Some(region) = region_opt {
                    for issue in &mut issues {
                        issue.fields.insert("region".into(), region.clone());
                    }
                }

                if fix && !issues.is_empty() {
                    // Through the shared gate, not `pin_reason` alone.
                    // `active_freeze: None` because the freeze is
                    // handled once up front for the whole run — a
                    // per-env freeze check would print the same refusal
                    // N times, and this loop skips the env rather than
                    // exiting. Passing it explicitly rather than
                    // reaching for `pin_reason` keeps this path on the
                    // shared decision even so.
                    if let Some(reason) = crate::cli::write_refusal(
                        &safety_cfg,
                        &env.name,
                        &active_profile_for_safety,
                        None,
                    ) {
                        if !quiet {
                            eprintln!("ebman lint --fix: {reason}");
                        }
                        // Only a real (--yes) run treats the refusal as
                        // a dispatch failure — a --dry-run preview
                        // dispatched nothing and must not exit 1.
                        if yes {
                            FIX_DISPATCH_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                        all_issues.extend(issues);
                        continue;
                    }
                    let region_label = region_opt.as_deref().unwrap_or("default").to_string();
                    // Rebuild the (cheap, borrowing) context for the
                    // fix pass — `run_rules_for_env` consumed its own.
                    let ctx = build_lint_context(env, &inputs, &safety_cfg.required_tags);
                    let mut to_set: Vec<(String, String, String)> = Vec::new();
                    let mut planned: Vec<(String, lint::FixAction)> = Vec::new();
                    let mut planned_set_indices: Vec<usize> = Vec::new();
                    for issue in &issues {
                        if fix_disabled.contains(&issue.rule_id) {
                            if !quiet && !json {
                                println!(
                                    "skip {} ({}): in lint.fix_disable",
                                    issue.rule_id, env.name
                                );
                            }
                            continue;
                        }
                        let Some(rule) = rules.iter().find(|r| r.id() == issue.rule_id) else {
                            continue;
                        };
                        let Some(action) = rule.fix(&ctx) else {
                            if !quiet && !json {
                                println!(
                                    "no-fix {} ({}): rule has no auto-remediation",
                                    issue.rule_id, env.name
                                );
                            }
                            continue;
                        };
                        if let lint::FixAction::SetOption {
                            namespace,
                            name,
                            value,
                            ..
                        } = &action
                        {
                            planned_set_indices.push(planned.len());
                            to_set.push((namespace.clone(), name.clone(), value.clone()));
                        }
                        planned.push((issue.rule_id.clone(), action));
                    }
                    // Plan lines respect --quiet and stay off stdout
                    // under --json (prose interleaved with the JSON
                    // document broke every piped consumer).
                    if !quiet && !json {
                        for (rule_id, action) in &planned {
                            match action {
                                lint::FixAction::SetOption { description, .. } => {
                                    println!("fix {rule_id} ({}): {description}", env.name);
                                }
                                lint::FixAction::Manual { instructions } => {
                                    println!(
                                    "fix {rule_id} ({}) MANUAL — operator action required:\n  {instructions}",
                                    env.name
                                );
                                }
                            }
                        }
                    }
                    if !to_set.is_empty() && yes {
                        match aws
                            .update_env_option_settings(&env.name, &to_set, &[])
                            .await
                        {
                            Ok(()) => {
                                for &idx in &planned_set_indices {
                                    let (rule_id, action) = &planned[idx];
                                    if let lint::FixAction::SetOption {
                                        namespace,
                                        name,
                                        value,
                                        ..
                                    } = action
                                    {
                                        audit::append_lint_fix(
                                            &region_label,
                                            &env.name,
                                            rule_id,
                                            namespace,
                                            name,
                                            value,
                                            None,
                                        );
                                    }
                                }
                                if !quiet && !json {
                                    println!(
                                        "ok ({}): applied {} fix(es)",
                                        env.name,
                                        planned_set_indices.len()
                                    );
                                }
                            }
                            Err(e) => {
                                eprintln!(
                                "ebman lint --fix: dispatch failed for {} in {region_label}: {e}",
                                env.name
                            );
                                let err_str = e.to_string();
                                for &idx in &planned_set_indices {
                                    let (rule_id, action) = &planned[idx];
                                    if let lint::FixAction::SetOption {
                                        namespace,
                                        name,
                                        value,
                                        ..
                                    } = action
                                    {
                                        audit::append_lint_fix(
                                            &region_label,
                                            &env.name,
                                            rule_id,
                                            namespace,
                                            name,
                                            value,
                                            Some(&err_str),
                                        );
                                    }
                                }
                                FIX_DISPATCH_FAILED
                                    .store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                    }
                }

                all_issues.extend(issues);
            }

            // EBL015 — account-level pass (stale custom platforms) via
            // the assembly shared with the MCP lint tool. Outside the
            // per-env registry, so `lint.disable` is honoured here;
            // skipped when linting a single --env (the operator scoped
            // the run) and in the common zero-custom-platform account
            // the extra cost is one empty list call.
            if env_name.is_none() && !disabled.iter().any(|d| d == "EBL015") {
                match fetch_stale_platform_issues(&aws, chrono::Utc::now()).await {
                    Ok((mut issues, warnings)) => {
                        if !quiet {
                            for w in warnings {
                                eprintln!("warning: {w}");
                            }
                        }
                        if let Some(min) = severity_filter {
                            issues.retain(|i| i.severity >= min);
                        }
                        if !rule_filter.is_empty() {
                            issues.retain(|i| rule_filter.contains(&i.rule_id));
                        }
                        if let Some(region) = region_opt {
                            for issue in &mut issues {
                                issue.fields.insert("region".into(), region.clone());
                            }
                        }
                        all_issues.extend(issues);
                    }
                    Err(e) => {
                        if !quiet {
                            eprintln!("warning: EBL015 skipped — ListPlatformVersions: {e}");
                        }
                    }
                }
            }
        }

        // `--webhook URL` (watch mode): POST the cycle's findings when
        // the issue SET changed since the last post — a 60s interval
        // must not re-page the channel with the same three warnings
        // every minute, but a new issue (or the all-clear) should land
        // immediately. Identity comes from `lint::issue_identity`, the
        // same key the baseline machinery uses.
        if let Some(url) = webhook.as_deref() {
            if cycle_degraded {
                // Incomplete data: don't page, don't move the baseline.
                // The next full cycle compares against the last GOOD
                // state, so a real change during the outage still fires.
                if !quiet {
                    eprintln!("warning: cycle degraded (fetch failures) — webhook suppressed");
                }
            } else {
                let identities: std::collections::BTreeSet<String> =
                    all_issues.iter().map(lint::issue_identity).collect();
                // A first cycle that's already clean posts nothing —
                // the all-clear body claims issues cleared, and none
                // did. Only a change from a KNOWN previous state (or
                // first findings) is worth a page.
                let first_cycle_clean = last_webhook_identities.is_none() && identities.is_empty();
                if !first_cycle_clean && last_webhook_identities.as_ref() != Some(&identities) {
                    let detail = webhook_summary(&all_issues);
                    audit::fire_webhook(
                        url,
                        None,
                        active_profile_for_safety.as_deref(),
                        if multi_region {
                            "multi"
                        } else {
                            regions[0].as_deref().unwrap_or("default")
                        },
                        &detail,
                        &cycle_started.to_rfc3339(),
                    );
                }
                last_webhook_identities = Some(identities);
            }
        }

        // Baseline modes (write / diff) handle their own output
        // shape; skip the standard text/json render in those paths.
        let baseline_mode = baseline_write.is_some() || baseline_against.is_some();
        if !quiet && !baseline_mode {
            if json {
                println!("{}", lint::render_issues_json(&all_issues));
            } else if all_issues.is_empty() {
                println!("✓ No issues found");
            } else {
                for issue in &all_issues {
                    let sev = issue.severity.as_str();
                    let env_str = issue.env_name.as_deref().unwrap_or("-");
                    if multi_region {
                        let region = issue
                            .fields
                            .get("region")
                            .map(String::as_str)
                            .unwrap_or("-");
                        println!(
                            "{region}\t{sev}\t{}\t{env_str}\t{}",
                            issue.rule_id, issue.title
                        );
                    } else {
                        println!("{sev}\t{}\t{env_str}\t{}", issue.rule_id, issue.title);
                    }
                    if let Some(s) = &issue.suggestion {
                        println!("\t→ {s}");
                    }
                }
            }
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }

        // --baseline FILE: snapshot current issues to disk, exit 0.
        // Operators use this once when adopting `ebman lint` on a
        // fleet with existing warnings — grandfathers them so
        // subsequent runs only flag NEW issues.
        if let Some(path) = baseline_write.as_deref() {
            // A degraded run (skipped regions/envs) has an incomplete
            // issue set — snapshotting it would silently grandfather
            // whatever the outage hid, and the next --against-baseline
            // run would report the reappeared issues as NEW (or worse,
            // a fully-failed run writes an empty baseline).
            if cycle_degraded {
                eprintln!(
                    "ebman lint --baseline: refusing to snapshot a degraded run \
                     (fetch failures above) — fix access and re-run"
                );
                std::process::exit(1);
            }
            let body = lint::render_issues_json(&all_issues);
            if let Err(e) = std::fs::write(path, &body) {
                eprintln!("ebman lint --baseline: write {path}: {e}");
                std::process::exit(1);
            }
            if !quiet {
                eprintln!(
                    "ebman lint --baseline: wrote {} issue(s) to {path}",
                    all_issues.len()
                );
            }
            last_cycle_clean = true; // snapshot ALWAYS exits 0
        } else if let Some(path) = baseline_against.as_deref() {
            // --against-baseline FILE: diff current issues against
            // the snapshot. NEW issues exit 3; CLEARED issues are
            // informational. Composes with --json (emits a single
            // {new:[...],cleared:[...]} blob).
            let baseline_text = match std::fs::read_to_string(path) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("ebman lint --against-baseline: read {path}: {e}");
                    std::process::exit(1);
                }
            };
            let baseline_issues = match lint::parse_baseline(&baseline_text) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("ebman lint --against-baseline: {e}");
                    std::process::exit(1);
                }
            };
            let baseline_set: std::collections::HashSet<&str> = baseline_issues
                .iter()
                .map(|b| b.identity.as_str())
                .collect();
            let current_identities: Vec<String> =
                all_issues.iter().map(lint::issue_identity).collect();
            let current_set: std::collections::HashSet<&str> =
                current_identities.iter().map(String::as_str).collect();

            let new_issues: Vec<&lint::Issue> = all_issues
                .iter()
                .zip(current_identities.iter())
                .filter(|(_, id)| !baseline_set.contains(id.as_str()))
                .map(|(i, _)| i)
                .collect();
            let cleared: Vec<&lint::BaselineIssue> = baseline_issues
                .iter()
                .filter(|b| !current_set.contains(b.identity.as_str()))
                .collect();

            if !quiet {
                if json {
                    print_baseline_diff_json(&new_issues, &cleared);
                } else {
                    if new_issues.is_empty() && cleared.is_empty() {
                        println!(
                            "✓ No drift vs baseline ({} issues stable)",
                            baseline_set.len()
                        );
                    }
                    for issue in &new_issues {
                        let sev = issue.severity.as_str();
                        let env_str = issue.env_name.as_deref().unwrap_or("-");
                        println!(
                            "+ NEW\t{sev}\t{}\t{env_str}\t{}",
                            issue.rule_id, issue.title
                        );
                    }
                    for b in &cleared {
                        let env_str = b.env_name.as_deref().unwrap_or("-");
                        println!("✓ CLEARED\t{}\t{env_str}\t{}", b.rule_id, b.title);
                    }
                }
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }

            last_cycle_clean = new_issues.is_empty();
        } else {
            last_cycle_clean = all_issues.is_empty();
        }
        last_cycle_degraded = cycle_degraded;

        if !watch {
            break;
        }
        // Sleep `interval_secs` or break on Ctrl-C — whichever
        // fires first. `tokio::signal::ctrl_c` panics if called
        // outside a Tokio runtime, but `run` is `#[tokio::main]`-
        // driven so we're always inside one here.
        tokio::select! {
            _ = &mut ctrl_c => {
                if !quiet && !json {
                    eprintln!("(watch interrupted)");
                }
                break;
            }
            _ = tokio::time::sleep(
                // Interval is start-to-start: subtract the cycle's own
                // duration so `--interval 60s` fires every ~60s, not
                // 60s + however long the fleet scan took.
                std::time::Duration::from_secs(interval_secs).saturating_sub(
                    (chrono::Utc::now() - cycle_started)
                        .to_std()
                        .unwrap_or_default(),
                ),
            ) => {}
        }
    }

    // Drain in-flight webhook POSTs (lint --fix audit fan-out and
    // --watch --webhook cycle posts) before the process ends —
    // fire-and-forget tasks are cancelled at runtime drop.
    audit::drain_webhooks(std::time::Duration::from_secs(12)).await;
    if fix {
        if FIX_DISPATCH_FAILED.load(std::sync::atomic::Ordering::Relaxed) || last_cycle_degraded {
            std::process::exit(1);
        }
        Ok(())
    } else if !last_cycle_clean {
        // Issues found wins over degraded — exit 3 is actionable.
        std::process::exit(3);
    } else if last_cycle_degraded {
        // "Clean" but incomplete: some region/env was skipped on a
        // fetch failure, so clean is unproven. The documented
        // AWS-error exit code — a CI gate must not pass green on
        // expired credentials.
        std::process::exit(1);
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn bare_lint_has_sane_defaults() {
        let p = parse_lint_args(&argv(&["lint"])).unwrap();
        assert_eq!(p.regions, vec![None]);
        assert_eq!(p.interval_secs, 60);
        assert!(!p.json && !p.quiet && !p.fix && !p.watch);
        assert!(p.severity_filter.is_none() && p.rule_filter.is_empty());
        assert!(p.baseline_write.is_none() && p.baseline_against.is_none());
    }

    #[test]
    fn collects_filters_and_flags() {
        let p = parse_lint_args(&argv(&[
            "lint",
            "--env",
            "prod",
            "--json",
            "--quiet",
            "--severity",
            "warn",
            "--rules",
            "EBL001, EBL004 ,EBL019",
        ]))
        .unwrap();
        assert_eq!(p.env_name.as_deref(), Some("prod"));
        assert!(p.json && p.quiet);
        assert_eq!(p.severity_filter, Some(lint::Severity::Warn));
        // CSV split + trimmed.
        assert_eq!(p.rule_filter, vec!["EBL001", "EBL004", "EBL019"]);
    }

    #[test]
    fn unknown_flag_and_severity_are_usage_errors() {
        assert!(parse_lint_args(&argv(&["lint", "--bogus"]))
            .unwrap_err()
            .contains("unknown flag"));
        assert!(parse_lint_args(&argv(&["lint", "--severity", "loud"]))
            .unwrap_err()
            .contains("unknown severity"));
    }

    #[test]
    fn baseline_flag_requires_a_path_not_another_flag() {
        // value-flag that swallows the next token must reject a flag
        // sitting where the path should be (the `--baseline --json` trap).
        let err = parse_lint_args(&argv(&["lint", "--baseline", "--json"])).unwrap_err();
        assert!(err.contains("--baseline expects a file path"), "got: {err}");
        // ...and a totally missing value is the same class of error.
        let err2 = parse_lint_args(&argv(&["lint", "--baseline"])).unwrap_err();
        assert!(
            err2.contains("--baseline expects a file path"),
            "got: {err2}"
        );
    }

    #[test]
    fn interval_accepts_bare_seconds_and_durations_rejects_zero_and_garbage() {
        assert_eq!(
            parse_lint_args(&argv(&["lint", "--interval", "30"]))
                .unwrap()
                .interval_secs,
            30
        );
        assert_eq!(
            parse_lint_args(&argv(&["lint", "--interval", "5m"]))
                .unwrap()
                .interval_secs,
            300
        );
        assert!(parse_lint_args(&argv(&["lint", "--interval", "0"]))
            .unwrap_err()
            .contains("must be > 0"));
        assert!(parse_lint_args(&argv(&["lint", "--interval", "soon"]))
            .unwrap_err()
            .contains("expects seconds"));
        // The docs' own example form — rejected until the 0.26
        // max-review added the seconds unit to parse_window_ms.
        assert_eq!(
            parse_lint_args(&argv(&["lint", "--interval", "60s"]))
                .unwrap()
                .interval_secs,
            60
        );
    }

    #[test]
    fn value_flags_reject_missing_or_flag_values() {
        // A trailing --env on `--fix --yes` used to silently widen
        // scope to the whole fleet; --rules eating --json used to
        // filter every issue out and exit 0.
        assert!(parse_lint_args(&argv(&["lint", "--fix", "--yes", "--env"]))
            .unwrap_err()
            .contains("--env expects"));
        assert!(parse_lint_args(&argv(&["lint", "--env", "--json"]))
            .unwrap_err()
            .contains("got flag"));
        assert!(parse_lint_args(&argv(&["lint", "--rules", "--json"]))
            .unwrap_err()
            .contains("got flag"));
        assert!(parse_lint_args(&argv(&["lint", "--rules", " , "]))
            .unwrap_err()
            .contains("no rule ids"));
        assert!(parse_lint_args(&argv(&["lint", "--regions"]))
            .unwrap_err()
            .contains("--regions expects"));
        assert!(parse_lint_args(&argv(&["lint", "--interval", "--watch"]))
            .unwrap_err()
            .contains("got flag"));
    }

    #[test]
    fn fix_requires_yes_or_dry_run() {
        // --fix alone is a usage error: it must pick apply (--yes) or
        // preview (--dry-run) explicitly.
        assert!(parse_lint_args(&argv(&["lint", "--fix"]))
            .unwrap_err()
            .contains("requires --yes"));
        // --fix --yes and --fix --dry-run both parse.
        assert!(
            parse_lint_args(&argv(&["lint", "--fix", "--yes"]))
                .unwrap()
                .fix
        );
        assert!(
            parse_lint_args(&argv(&["lint", "--fix", "--dry-run"]))
                .unwrap()
                .dry_run
        );
        // ...but not both at once.
        assert!(
            parse_lint_args(&argv(&["lint", "--fix", "--yes", "--dry-run"]))
                .unwrap_err()
                .contains("mutually exclusive")
        );
    }

    #[test]
    fn mutually_exclusive_mode_combinations_are_rejected() {
        assert!(
            parse_lint_args(&argv(&["lint", "--watch", "--fix", "--yes"]))
                .unwrap_err()
                .contains("--watch and --fix")
        );
        assert!(parse_lint_args(&argv(&[
            "lint",
            "--baseline",
            "b.json",
            "--against-baseline",
            "a.json"
        ]))
        .unwrap_err()
        .contains("mutually exclusive"));
        assert!(
            parse_lint_args(&argv(&["lint", "--baseline", "b.json", "--fix", "--yes"]))
                .unwrap_err()
                .contains("incompatible with --fix")
        );
    }

    #[test]
    fn empty_regions_csv_is_usage_error() {
        let err = parse_lint_args(&argv(&["lint", "--regions", " , "])).unwrap_err();
        assert!(err.contains("--regions list is empty"), "got: {err}");
    }

    #[test]
    fn probe_live_flag_parses() {
        let p = parse_lint_args(&argv(&["lint", "--probe-live"])).unwrap();
        assert!(p.probe_live);
        let p = parse_lint_args(&argv(&["lint"])).unwrap();
        assert!(!p.probe_live);
    }

    #[test]
    fn webhook_requires_watch_and_a_real_url() {
        let p = parse_lint_args(&argv(&[
            "lint",
            "--watch",
            "--webhook",
            "https://hooks.example/x",
        ]))
        .unwrap();
        assert_eq!(p.webhook.as_deref(), Some("https://hooks.example/x"));
        // Without --watch: usage error (one-shot runs print findings).
        let err =
            parse_lint_args(&argv(&["lint", "--webhook", "https://hooks.example/x"])).unwrap_err();
        assert!(err.contains("--watch"), "got: {err}");
        // Value-flag trap: a following flag is not a URL.
        let err = parse_lint_args(&argv(&["lint", "--watch", "--webhook", "--json"])).unwrap_err();
        assert!(err.contains("expects a URL"), "got: {err}");
        let err = parse_lint_args(&argv(&["lint", "--watch", "--webhook"])).unwrap_err();
        assert!(err.contains("expects a URL"), "got: {err}");
    }

    #[test]
    fn run_rules_for_env_wires_inputs_through_the_context() {
        // EBL017 fires on a bare env (managed actions absent) —
        // proves the shared builder produces a working context.
        let env = aws::Environment {
            name: "prod".into(),
            application: "shop".into(),
            status: "Ready".into(),
            health: "Green".into(),
            platform: "Node.js 20".into(),
            solution_stack: String::new(),
            tier: "Web".into(),
            cname: "prod.example.com".into(),
            version_label: "b1".into(),
            arn: None,
            updated: Some(chrono::Utc::now()),
            id: None,
            region: None,
        };
        let rules = lint::default_rules(&[]);
        let bare = EnvLintInputs::bare(vec![]);
        let issues = run_rules_for_env(&rules, &env, &bare, &[]);
        assert!(issues.iter().any(|i| i.rule_id == "EBL017"));
        // Probe inputs reach EBL016 / EBL018 through the same path
        // (the env is prod-named, so the WAF signal fires).
        let probed = EnvLintInputs {
            probe_failure: Some("HTTP 503".into()),
            waf_missing: Some(true),
            ..EnvLintInputs::bare(vec![])
        };
        let issues = run_rules_for_env(&rules, &env, &probed, &[]);
        assert!(issues.iter().any(|i| i.rule_id == "EBL016"));
        assert!(issues.iter().any(|i| i.rule_id == "EBL018"));
    }

    #[test]
    fn webhook_summary_caps_and_handles_all_clear() {
        assert!(webhook_summary(&[]).contains("clean"));
        let mk = |n: usize| lint::Issue {
            rule_id: format!("EBL00{n}"),
            severity: lint::Severity::Warn,
            env_name: Some(format!("env-{n}")),
            title: format!("issue {n}"),
            detail: String::new(),
            suggestion: None,
            fields: Default::default(),
        };
        let issues: Vec<lint::Issue> = (1..=7).map(mk).collect();
        let s = webhook_summary(&issues);
        assert!(s.starts_with("lint: 7 issue(s)"), "got: {s}");
        assert!(s.contains("EBL001 env-1: issue 1"), "got: {s}");
        assert!(s.contains("…and 2 more"), "got: {s}");
        assert!(!s.contains("issue 6"), "cap at 5, got: {s}");
    }
}

#[cfg(test)]
mod probe_outcome_tests {
    use super::ProbeOutcome;

    #[test]
    fn a_failed_probe_is_not_a_clean_result() {
        // The whole point. `Option<bool>` collapsed "the rule doesn't
        // apply" and "IAM denied the probe" into the same `None`, so
        // `ebman lint --json` — a CI gate — reported a clean bill of
        // health for a check that never ran.
        let denied = ProbeOutcome::Unknown("SimulatePrincipalPolicy failed: AccessDenied".into());
        assert_eq!(denied.verdict(), None, "the rule still skips");
        let warn = denied
            .coverage_warning("EBL020", "api-prod")
            .expect("an unrunnable check must say so");
        assert!(
            warn.contains("EBL020") && warn.contains("api-prod"),
            "{warn}"
        );
        assert!(
            warn.contains("NOT a clean result"),
            "the wording has to leave no room for reading it as a pass: {warn}"
        );
    }

    #[test]
    fn not_applicable_stays_silent() {
        // X-Ray off, no instance profile, not a prod ALB — a genuine
        // skip. Warning on these would train the operator to ignore the
        // warnings that matter.
        let na = ProbeOutcome::NotApplicable;
        assert_eq!(na.verdict(), None);
        assert_eq!(na.coverage_warning("EBL020", "api-prod"), None);
    }

    #[test]
    fn a_checked_probe_reports_its_verdict_and_warns_about_nothing() {
        assert_eq!(ProbeOutcome::Checked(true).verdict(), Some(true));
        assert_eq!(ProbeOutcome::Checked(false).verdict(), Some(false));
        assert_eq!(
            ProbeOutcome::Checked(false).coverage_warning("EBL018", "api-prod"),
            None
        );
    }

    #[test]
    fn the_three_outcomes_are_distinguishable() {
        // If two of them ever compared equal the distinction would be
        // decorative — which is what `Option<bool>` was.
        assert_ne!(ProbeOutcome::NotApplicable, ProbeOutcome::Checked(false));
        assert_ne!(
            ProbeOutcome::NotApplicable,
            ProbeOutcome::Unknown("x".into())
        );
        assert_ne!(
            ProbeOutcome::Checked(false),
            ProbeOutcome::Unknown("x".into())
        );
    }
}

#[cfg(test)]
mod disabled_rule_probes {
    use super::ProbeOutcome;

    /// A disabled rule must not produce a coverage warning.
    ///
    /// The panel's blocker: once a failed probe started marking the run
    /// degraded (exit 1), a rule the operator had switched off via
    /// `lint.disable` could still redden the pipeline — and the only
    /// remedy was changing IAM, because the documented escape hatch
    /// didn't reach the probe. `fetch_env_lint_inputs` takes the
    /// disabled list now and skips the probe entirely, which also stops
    /// paying for the IAM/WAF calls.
    #[test]
    fn a_disabled_rule_yields_no_coverage_warning() {
        // What a skipped probe returns, and what that means downstream.
        let skipped = ProbeOutcome::NotApplicable;
        assert_eq!(skipped.coverage_warning("EBL018", "api-prod"), None);
        assert_eq!(skipped.verdict(), None);

        // Contrast: an ENABLED rule whose probe failed still warns, and
        // still degrades the run. Collapsing these two would put the
        // original false-green bug straight back.
        let failed = ProbeOutcome::Unknown("GetWebACLForResource: AccessDenied".into());
        assert!(failed.coverage_warning("EBL018", "api-prod").is_some());
    }
}

#[cfg(test)]
mod disabled_rule_wiring {
    use super::ProbeOutcome;

    fn env() -> crate::aws::Environment {
        crate::aws::Environment {
            name: "api-prod".into(),
            application: "uflexi".into(),
            status: "Ready".into(),
            health: "Green".into(),
            platform: "Java 17".into(),
            solution_stack: "64bit Amazon Linux 2023 running Corretto 17".into(),
            tier: "Web".into(),
            cname: String::new(),
            version_label: "build-1".into(),
            arn: None,
            updated: None,
            id: None,
            region: None,
        }
    }

    /// A disabled rule's probe returns before touching AWS.
    ///
    /// The panel's blocker: a rule switched off via `lint.disable`
    /// could still redden CI, because the probe ran anyway and a failed
    /// probe marks the run degraded (exit 1). The only remedy was
    /// changing IAM — the documented escape hatch didn't reach here.
    ///
    /// Testable because the check lives INSIDE the probe. The earlier
    /// version checked at the call site, which meant reaching it
    /// required `fetch_env_lint_inputs` to get past
    /// `DescribeConfigurationSettings` first — which a stub client
    /// cannot — so that test was vacuous and said so in its own comment.
    #[tokio::test]
    async fn a_disabled_rules_probe_does_not_run() {
        let aws = crate::aws::AwsClient::stub();
        // Options that WOULD make the X-Ray probe fire: X-Ray on, with
        // an instance profile to look up.
        let options = vec![
            (
                "aws:elasticbeanstalk:xray".to_string(),
                "XRayEnabled".to_string(),
                "true".to_string(),
            ),
            (
                "aws:autoscaling:launchconfiguration".to_string(),
                "IamInstanceProfile".to_string(),
                "eb-ec2-role".to_string(),
            ),
            // ...and what makes the WAF probe fire: an ALB. Without
            // this the WAF probe returns NotApplicable for its OWN
            // reason, so removing the opt-out changed nothing and the
            // assertion below passed either way.
            (
                "aws:elasticbeanstalk:environment".to_string(),
                "LoadBalancerType".to_string(),
                "application".to_string(),
            ),
        ];

        // Disabled: NotApplicable, without an AWS call.
        assert_eq!(
            super::probe_xray_trace_denied(&aws, &options, &["EBL020".to_string()]).await,
            ProbeOutcome::NotApplicable
        );
        assert_eq!(
            super::probe_waf_missing(&aws, &env(), &options, &["EBL018".to_string()]).await,
            ProbeOutcome::NotApplicable
        );

        // Enabled: the probe runs and the stub fails it, which must
        // report Unknown — NOT a clean skip. Collapsing these two puts
        // the original false-green bug straight back.
        let enabled = super::probe_xray_trace_denied(&aws, &options, &[]).await;
        assert!(
            matches!(enabled, ProbeOutcome::Unknown(_)),
            "an enabled probe that cannot run must say so, got {enabled:?}"
        );
    }
}
