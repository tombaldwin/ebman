//! A live environment's lint inputs: the fetch-and-assemble step
//! shared by `ebman lint`, the MCP `lint` tool, and the TUI's `:lint`
//! and `:explain`.
//!
//! Rules are pure and synchronous (`lint::rules`); everything here is
//! the async half that feeds them. It lived in `cli/lint.rs`, where the
//! TUI could not reach it without depending on the CLI — so the TUI
//! kept its own copies, and they drifted: a failed tag fetch flattened
//! to "no tags" (EBL010 false positives in `:explain`), a failed option
//! fetch read as "no issues" before a deploy. One assembly, one set of
//! answers to "could this check run?".

use crate::{aws, lint};

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

/// EBL020 input probe: when the env has `XRayEnabled=true`, resolve
/// its instance-profile role and IAM-simulate `xray:PutTraceSegments`
/// against it. `Some(true)` = denied (the rule's firing signal),
/// `Some(false)` = allowed, `None` = X-Ray off / no profile / probe
/// failed (the rule skips — never a false positive from a failed
/// probe). Lives at the call site rather than in `LintContext`
/// because rules are pure and synchronous.
pub(crate) async fn probe_xray_trace_denied(
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
pub(crate) async fn probe_waf_missing(
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
    /// EBL011's input: the worker DLQ depth. Not fetched here — the
    /// lint path does not poll queues — so it is `None` from
    /// `fetch_env_lint_inputs`, and the TUI fills it from the depth it
    /// already caches. Without the field the TUI could not use this
    /// assembly at all, which is why it kept its own copies.
    pub dlq_depth: Option<i64>,
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
            dlq_depth: None,
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
    // EBL010 can only fire when the operator declared required tags,
    // so a failed tag fetch only costs coverage when there are some.
    required_tags: &[String],
) -> Result<EnvLintInputs, String> {
    let opts_fut = aws.fetch_env_option_settings(&env.application, &env.name);
    let tags_fut = async {
        match env.arn.as_deref() {
            Some(arn) => Some(aws.list_tags(arn).await),
            None => None,
        }
    };
    let health_fut = aws.fetch_env_instance_counts(&env.name);
    let (opts_res, tags_opt, health_res) = tokio::join!(opts_fut, tags_fut, health_fut);
    let options = opts_res.map_err(|e| e.to_string())?;
    // A failed fetch still leaves the input `None`, so the rule skips —
    // a failed fetch must never become a false positive. What changed
    // is that it is no longer SILENT: `.ok()` on both of these turned
    // AccessDenied or throttling into a skip indistinguishable from a
    // clean pass, so `lint` exited 0 and `--baseline` adopted a run
    // whose EBL010/EBL012 checks never happened. The rule's own comment
    // said "a FAILED fetch is a different thing and the caller reports
    // it"; no caller did.
    let mut fetch_warnings: Vec<String> = Vec::new();
    let env_tag_keys: Option<Vec<String>> = match tags_opt {
        Some(Ok(t)) => Some(t.into_iter().map(|(k, _)| k).collect()),
        Some(Err(e)) => {
            if ebl010_could_fire(disabled, required_tags) {
                fetch_warnings.extend(
                    ProbeOutcome::Unknown(e.to_string()).coverage_warning("EBL010", &env.name),
                );
            }
            None
        }
        None => None,
    };
    let healthy_count = match health_res {
        Ok(c) => Some(c.healthy as i64),
        Err(e) => {
            if ebl012_could_fire(disabled, env, &options) {
                fetch_warnings.extend(
                    ProbeOutcome::Unknown(e.to_string()).coverage_warning("EBL012", &env.name),
                );
            }
            None
        }
    };
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
    let coverage_warnings = fetch_warnings
        .into_iter()
        .chain(
            [
                xray_outcome.coverage_warning("EBL020", &env.name),
                waf_outcome.coverage_warning("EBL018", &env.name),
            ]
            .into_iter()
            .flatten(),
        )
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
        dlq_depth: None,
    })
}

/// Could EBL010 have fired, had the tag fetch succeeded? Only when it
/// is enabled and the operator declared required tags — otherwise a
/// failed fetch costs no coverage, and reporting it would mark a run
/// degraded over a check that could never have run.
pub(crate) fn ebl010_could_fire(disabled: &[String], required_tags: &[String]) -> bool {
    !disabled.iter().any(|d| d == "EBL010") && !required_tags.is_empty()
}

/// Could EBL012 have fired, had the health fetch succeeded?
///
/// Only for an env that is Ready and Green — the rule's own
/// preconditions — and not on BASIC health reporting, where
/// `DescribeEnvironmentHealth` is unavailable by design. Without that
/// last condition every basic-health env would fail the fetch and mark
/// every run degraded: a false alarm on an ordinary configuration,
/// which is worse than the silence it replaces.
pub(crate) fn ebl012_could_fire(
    disabled: &[String],
    env: &aws::Environment,
    options: &[(String, String, String)],
) -> bool {
    let basic = options.iter().any(|(ns, name, value)| {
        ns == "aws:elasticbeanstalk:healthreporting:system"
            && name == "SystemType"
            && value.eq_ignore_ascii_case("basic")
    });
    let green = env.health.eq_ignore_ascii_case("Green") || env.health.eq_ignore_ascii_case("Ok");
    !disabled.iter().any(|d| d == "EBL012")
        && !basic
        && env.status.eq_ignore_ascii_case("Ready")
        && green
}

/// EBL015 account-level assembly, shared by `run` and the MCP `lint`
/// tool: list custom platforms, resolve each branch's newest version
/// date via `latest_platform_version_date`, and run the pure
/// staleness pass. Returns the issues plus per-branch warnings for
/// branches whose date fetch failed — lost coverage, which the CLI
/// degrades on and MCP reports in `skipped_envs`. `Err` carries the
/// ListPlatformVersions failure. Callers gate on scope (`--env` skips) + `lint.disable`.
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
    if let Some(depth) = inputs.dlq_depth {
        ctx = ctx.with_dlq_depth(depth);
    }
    ctx
}

/// What `:explain RULE` should say about one env.
#[derive(Debug, PartialEq)]
pub(crate) enum ExplainVerdict<'a> {
    /// The rule fired: explain this issue.
    Fires(&'a lint::Issue),
    /// The rule could not be evaluated: its input fetch or probe
    /// failed. Saying "doesn't fire" here would be a clean bill of
    /// health for a check that never ran.
    NotEvaluated(&'a str),
    /// Evaluated, and it does not fire.
    DoesNotFire,
}

/// Decide `:explain`'s answer from the rule run and its lost coverage.
///
/// The TUI's own copy of the assembly flattened a failed tag fetch into
/// "no tags", so EBL010 FIRED for every required tag on an env whose
/// tags were never read — and a failed fetch of any other input read as
/// "doesn't fire". Coverage warnings lead with the rule id (see
/// `ProbeOutcome::coverage_warning`), which is how one is matched here.
pub(crate) fn explain_verdict<'a>(
    rule_id: &str,
    issues: &'a [lint::Issue],
    coverage_warnings: &'a [String],
) -> ExplainVerdict<'a> {
    if let Some(issue) = issues.iter().find(|i| i.rule_id == rule_id) {
        return ExplainVerdict::Fires(issue);
    }
    match coverage_warnings
        .iter()
        .find(|w| w.split_whitespace().next() == Some(rule_id))
    {
        Some(w) => ExplainVerdict::NotEvaluated(w),
        None => ExplainVerdict::DoesNotFire,
    }
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

#[cfg(test)]
mod explain_tests {
    use super::{explain_verdict, ExplainVerdict};
    use crate::lint;

    fn issue(rule: &str) -> lint::Issue {
        lint::Issue {
            rule_id: rule.into(),
            severity: lint::Severity::Warn,
            env_name: Some("api-prod".into()),
            title: format!("{rule} fired"),
            detail: String::new(),
            suggestion: None,
            fields: Default::default(),
        }
    }

    #[test]
    fn a_rule_that_fired_is_explained() {
        let issues = vec![issue("EBL001"), issue("EBL010")];
        assert_eq!(
            explain_verdict("EBL010", &issues, &[]),
            ExplainVerdict::Fires(&issues[1])
        );
    }

    /// The defect: a check whose input fetch failed must not read as
    /// "doesn't fire" — that is a clean bill of health for a check
    /// that never ran.
    #[test]
    fn a_rule_that_could_not_run_says_so() {
        let warnings = vec![
            "EBL010 could not be evaluated for api-prod: ListTagsForResource failed: AccessDenied"
                .to_string(),
        ];
        assert_eq!(
            explain_verdict("EBL010", &[], &warnings),
            ExplainVerdict::NotEvaluated(&warnings[0])
        );
    }

    /// A finding stands even when some OTHER check lost coverage on the
    /// same env — the ordinary mixed case.
    #[test]
    fn a_finding_stands_beside_another_rules_lost_coverage() {
        let issues = vec![issue("EBL010")];
        let warnings = vec!["EBL012 could not be evaluated for api-prod: throttled".to_string()];
        assert_eq!(
            explain_verdict("EBL010", &issues, &warnings),
            ExplainVerdict::Fires(&issues[0])
        );
    }

    #[test]
    fn a_rule_evaluated_and_quiet_does_not_fire() {
        assert_eq!(
            explain_verdict("EBL010", &[issue("EBL001")], &[]),
            ExplainVerdict::DoesNotFire
        );
    }

    /// Matched on the rule id as a WORD: another rule's warning, or one
    /// that merely mentions this id later on, is not this rule's.
    #[test]
    fn another_rules_lost_coverage_is_not_this_rules() {
        let warnings = vec![
            "EBL012 could not be evaluated for api-prod: see EBL010 too".to_string(),
            "EBL0100 could not be evaluated".to_string(),
        ];
        assert_eq!(
            explain_verdict("EBL010", &[], &warnings),
            ExplainVerdict::DoesNotFire
        );
    }
}
