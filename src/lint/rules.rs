//! The lint rules themselves — one `struct` + `impl Rule` per rule id.
//!
//! Split out of `lint.rs`, which was 3308 lines and the only large
//! domain module still in one file while `aws/` is one module per
//! service and `cli/` one per subcommand. The framework it uses —
//! `Rule`, `Issue`, `LintContext`, `Severity`, `run_rules` — lives in
//! `super`, and each rule reads as its own unit here.

use super::*;

pub(crate) struct AllAtOnceMultiInstance;

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
pub(crate) struct WebTierNoHealthCheckUrl;

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
pub(crate) struct EnvRedForExtendedPeriod;

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
pub(crate) struct BatchSizeExceedsMaxSize;

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
pub(crate) struct SingleInstanceEnv;

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
pub(crate) struct CooldownBelowRecommended;

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
pub(crate) struct ElbWithoutHttps;

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
pub(crate) struct StalePlatformVersion;

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
pub(crate) struct AsgMissingHealthCheckGracePeriod;

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
pub(crate) struct MissingRequiredTags;

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
        //  1. Caller LOADED the env's tags. `None` is "not loaded";
        //     `Some(&[])` is a successful fetch of an env with no
        //     tags, which fires for every required key — that env is
        //     the worst case the rule exists to catch.
        //  2. Operator declared required_tags (else nothing to check)
        //  3. At least one required key is missing from the env
        let Some(env_tag_keys) = ctx.env_tag_keys else {
            // Not loaded — nothing to compare against. A FAILED fetch
            // is a different thing and the caller reports it; what we
            // must not do is treat either as "all tags present".
            return None;
        };
        if ctx.required_tags.is_empty() {
            return None;
        }
        let missing: Vec<&str> = ctx
            .required_tags
            .iter()
            .filter(|req| !env_tag_keys.iter().any(|k| k.eq_ignore_ascii_case(req)))
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
pub(crate) struct WorkerDlqStuck;

/// Threshold for EBL011. Hard-coded for v1; future config-tunable
/// via `lint.ebl011.threshold` if operators ask.
pub(crate) const EBL011_DLQ_THRESHOLD: i64 = 100;

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
pub(crate) struct GreenButZeroInstances;

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
pub(crate) struct LaunchConfigurationLegacy;

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
pub(crate) fn parse_csv_value(value: &str) -> Vec<&str> {
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
pub(crate) struct AllAtOnceMultiAz;

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
pub(crate) struct ManagedActionsDisabled;

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

/// EBL014 — scaling trigger driving a *scaling* ASG off the legacy
/// default network metric. EB's out-of-the-box trigger is
/// `aws:autoscaling:trigger` `MeasureName=NetworkOut` — a poor
/// scaling signal for web workloads (bytes-out tracks response
/// sizes, not load; the modern signals are CPUUtilization, ALB
/// RequestCount, or env-health metrics). Fires only when the ASG
/// can actually scale (MaxSize > MinSize) — on a fixed-size env
/// the trigger is inert and warning would be noise. Fix=Manual:
/// the right replacement metric is workload-dependent.
///
/// (BACKLOG framed this as "deprecated CW namespace"; EB's trigger
/// namespace has no CW-namespace key, so the honest checkable
/// signal is the legacy NetworkIn/NetworkOut measure itself.)
pub(crate) struct ScalingTriggerLegacyNetworkMeasure;

impl Rule for ScalingTriggerLegacyNetworkMeasure {
    fn id(&self) -> &'static str {
        "EBL014"
    }
    fn severity(&self) -> Severity {
        Severity::Warn
    }
    fn fix(&self, ctx: &LintContext) -> Option<FixAction> {
        self.applies(ctx)?;
        Some(FixAction::Manual {
            instructions:
                "The env scales on the legacy default network metric. Pick a signal that tracks \
                 your actual load: `:scaling-triggers` with MeasureName=CPUUtilization is the \
                 common default; latency- or request-count-driven fleets should use ALB metrics \
                 or env-health-based scaling instead. The right metric is workload-dependent, \
                 so --fix can't choose one."
                    .into(),
        })
    }
    fn applies(&self, ctx: &LintContext) -> Option<Issue> {
        let measure = option_value(ctx.options, "aws:autoscaling:trigger", "MeasureName");
        if !measure.eq_ignore_ascii_case("NetworkOut") && !measure.eq_ignore_ascii_case("NetworkIn")
        {
            return None;
        }
        let min_size = parse_i32(option_value(ctx.options, "aws:autoscaling:asg", "MinSize"))?;
        let max_size = parse_i32(option_value(ctx.options, "aws:autoscaling:asg", "MaxSize"))?;
        if max_size <= min_size {
            return None;
        }
        let mut fields = BTreeMap::new();
        fields.insert("measure_name".into(), measure.to_string());
        fields.insert("min_size".into(), min_size.to_string());
        fields.insert("max_size".into(), max_size.to_string());
        Some(Issue {
            rule_id: self.id().into(),
            severity: self.severity(),
            env_name: Some(ctx.env.name.clone()),
            title: format!("ASG scales on legacy default metric ({measure})"),
            detail: format!(
                "The env's scaling trigger uses `aws:autoscaling:trigger` \
                 MeasureName={measure} — EB's legacy out-of-the-box default. Network \
                 bytes track response sizes, not load, so the fleet scales late under \
                 CPU-bound pressure and thrashes on payload-size changes. The ASG here \
                 genuinely scales (MinSize={min_size}, MaxSize={max_size}), so the \
                 trigger choice is live."
            ),
            suggestion: Some(
                "Switch the trigger to CPUUtilization (`:scaling-triggers`), or move to \
                 ALB-request-count / env-health-driven scaling."
                    .into(),
            ),
            fields,
        })
    }
}

/// EBL020 — X-Ray daemon enabled but the instance-profile role
/// can't write traces. `aws:elasticbeanstalk:xray` `XRayEnabled=true`
/// starts the daemon on every instance, but without
/// `xray:PutTraceSegments` on the instance profile the segments are
/// silently dropped — the operator sees "X-Ray on" in config and an
/// empty service map, with nothing in between to explain the gap.
/// The IAM answer comes from an `iam:SimulatePrincipalPolicy` probe
/// run by the caller (CLI-only; see `LintContext::xray_trace_denied`)
/// — the rule itself stays pure and skips when the probe didn't run.
pub(crate) struct XrayEnabledButTracesDenied;

impl Rule for XrayEnabledButTracesDenied {
    fn id(&self) -> &'static str {
        "EBL020"
    }
    fn severity(&self) -> Severity {
        Severity::Warn
    }
    fn fix(&self, ctx: &LintContext) -> Option<FixAction> {
        self.applies(ctx)?;
        Some(FixAction::Manual {
            instructions:
                "Attach X-Ray write permissions to the env's instance-profile role — the \
                 managed policy `AWSXRayDaemonWriteAccess` is the standard grant \
                 (xray:PutTraceSegments + PutTelemetryRecords). IAM policy attachment is \
                 outside EB option settings, so --fix can't drive it."
                    .into(),
        })
    }
    fn applies(&self, ctx: &LintContext) -> Option<Issue> {
        let enabled = option_value(ctx.options, "aws:elasticbeanstalk:xray", "XRayEnabled");
        if !enabled.eq_ignore_ascii_case("true") {
            return None;
        }
        // Probe not run (TUI path / probe error) or allowed → skip.
        if ctx.xray_trace_denied != Some(true) {
            return None;
        }
        let profile = option_value(
            ctx.options,
            "aws:autoscaling:launchconfiguration",
            "IamInstanceProfile",
        );
        let mut fields = BTreeMap::new();
        fields.insert("xray_enabled".into(), "true".into());
        if !profile.is_empty() {
            fields.insert("instance_profile".into(), profile.to_string());
        }
        Some(Issue {
            rule_id: self.id().into(),
            severity: self.severity(),
            env_name: Some(ctx.env.name.clone()),
            title: "X-Ray enabled but instance profile can't write traces".into(),
            detail: "`XRayEnabled=true` runs the X-Ray daemon on every instance, but an IAM \
                 simulation of `xray:PutTraceSegments` against the env's instance-profile \
                 role came back denied — segments are being dropped silently. The service \
                 map stays empty while the config claims tracing is on."
                .into(),
            suggestion: Some(
                "Attach `AWSXRayDaemonWriteAccess` (or an equivalent xray:PutTraceSegments \
                 grant) to the instance-profile role."
                    .into(),
            ),
            fields,
        })
    }
}

/// EBL018 — a prod-named env's ALB has no WAF WebACL associated.
/// Internet-facing production load balancers with no WAF pass every
/// scanner probe straight to the app tier — the low-traffic health
/// flapping that motivated this rule (2026-08: `.env` / traversal
/// sweeps 500-ing against Tomcat and tripping enhanced health) is
/// the mild version; the severe version is the probe that works.
/// Detection input comes from the caller (CLI-only; see
/// `LintContext::waf_missing`): a `wafv2:GetWebACLForResource` probe
/// against the env's ALB, run only for prod-named envs with
/// `LoadBalancerType=application`. Classic ELBs are out of scope —
/// WAFv2 can't associate with them (that fleet's WAF story is
/// CloudFront-level, which this rule can't verify). The rule itself
/// stays pure and skips when the probe didn't run.
pub(crate) struct NoWafOnProdAlb;

impl Rule for NoWafOnProdAlb {
    fn id(&self) -> &'static str {
        "EBL018"
    }
    fn severity(&self) -> Severity {
        Severity::Warn
    }
    fn fix(&self, ctx: &LintContext) -> Option<FixAction> {
        self.applies(ctx)?;
        Some(FixAction::Manual {
            instructions:
                "Create a WAFv2 WebACL (the `AWSManagedRulesCommonRuleSet` managed group is \
                 the standard starting point) and associate it with the env's ALB. WAF \
                 association lives outside EB option settings, so --fix can't drive it."
                    .into(),
        })
    }
    fn applies(&self, ctx: &LintContext) -> Option<Issue> {
        // Probe not run (TUI path / non-prod name / classic LB /
        // probe error) or WAF present → skip.
        if ctx.waf_missing != Some(true) || !is_prod_named(&ctx.env.name) {
            return None;
        }
        let mut fields = BTreeMap::new();
        fields.insert("load_balancer_type".into(), "application".into());
        Some(Issue {
            rule_id: self.id().into(),
            severity: self.severity(),
            env_name: Some(ctx.env.name.clone()),
            title: "Prod env's ALB has no WAF WebACL associated".into(),
            detail: "The env is prod-named with an application load balancer, and a \
                 `wafv2:GetWebACLForResource` probe found no WebACL associated — every \
                 scanner sweep and injection probe reaches the app tier unfiltered."
                .into(),
            suggestion: Some(
                "Associate a WAFv2 WebACL with the ALB — `AWSManagedRulesCommonRuleSet` \
                 blocks the commodity probe traffic. Not prod? Disable per-env via \
                 `lint.disable = [\"EBL018\"]`."
                    .into(),
            ),
            fields,
        })
    }
}

/// EBL015 — custom platform with no published versions in 180+ days
/// (Info). The first account-level lint pass: input is one
/// `(branch_name, latest_version_date)` pair per custom platform
/// (assembled by the CLI from `ListPlatformVersions` +
/// `DescribePlatformVersion`, which is where the dates live), not a
/// per-env `LintContext` — so this is a pure function outside the
/// `Rule` registry rather than a trait impl. Callers honour
/// `lint.disable` themselves (the registry's load-time filter can't
/// see it). Issues carry `env_name: None` — the fleet-wide slot the
/// `Issue` struct reserved from day one.
pub(crate) fn stale_custom_platform_issues(
    platforms: &[(String, chrono::DateTime<chrono::Utc>)],
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<Issue> {
    const STALE_DAYS: i64 = 180;
    let mut out: Vec<Issue> = platforms
        .iter()
        .filter_map(|(branch, latest)| {
            let age_days = (now - *latest).num_days();
            if age_days < STALE_DAYS {
                return None;
            }
            let mut fields = BTreeMap::new();
            fields.insert("platform".into(), branch.clone());
            Some(Issue {
                rule_id: "EBL015".into(),
                severity: Severity::Info,
                env_name: None,
                title: format!("Custom platform '{branch}' has no versions in {age_days} days"),
                detail: format!(
                    "The custom platform's newest version was published {age_days} days ago \
                     ({}). Long-idle custom platforms usually mean the operator forgot the \
                     platform exists — its AMIs age (unpatched base images), and envs still \
                     pinned to it drift ever further from current runtimes.",
                    latest.format("%Y-%m-%d")
                ),
                suggestion: Some(
                    "Publish a rebuilt version, migrate its envs to a managed platform, or \
                     delete it (`:custom-platform-delete`) if it's genuinely dead."
                        .into(),
                ),
                fields,
            })
        })
        .collect();
    out.sort_by(|a, b| a.title.cmp(&b.title));
    out
}

/// EBL016 — the env's health-check URL fails a live HTTP probe.
/// Detection input comes from the caller (CLI-only, behind
/// `ebman lint --probe-live` — one curl HEAD per env is too slow
/// for default lint): the same probe the Deploy confirm modal
/// ships (`build_health_check_probe_url` + curl + 2s cap), run at
/// lint time instead of deploy time. A failing probe on a
/// nominally-healthy env means EB's own health checks and the
/// operator's mental model have drifted — usually a health path
/// that changed, a security-group hole, or an env serving 5xx
/// that EB's ELB checks don't exercise.
pub(crate) struct HealthCheckProbeFailing;

impl Rule for HealthCheckProbeFailing {
    fn id(&self) -> &'static str {
        "EBL016"
    }
    fn severity(&self) -> Severity {
        Severity::Warn
    }
    fn fix(&self, ctx: &LintContext) -> Option<FixAction> {
        self.applies(ctx)?;
        Some(FixAction::Manual {
            instructions:
                "Probe the env's health-check URL yourself (`curl -IL http://<cname><path>`) and \
                 fix what it surfaces: wrong `Application Healthcheck URL` path, a security group \
                 blocking public HTTP, or the app genuinely failing. Not auto-fixable — the \
                 failure is in the running app or its network path, not in an option setting."
                    .into(),
        })
    }
    fn applies(&self, ctx: &LintContext) -> Option<Issue> {
        let reason = ctx.health_probe_failure?;
        // The failure reason stays out of `fields` deliberately:
        // `issue_identity` hashes fields, and a reason that flips
        // between runs ("timeout" vs "HTTP 503") would re-trigger the
        // webhook change-guard and churn baselines. The reason lives
        // in `detail`; identity is rule + env + cname.
        let mut fields = BTreeMap::new();
        if !ctx.env.cname.is_empty() {
            fields.insert("cname".into(), ctx.env.cname.clone());
        }
        Some(Issue {
            rule_id: self.id().into(),
            severity: self.severity(),
            env_name: Some(ctx.env.name.clone()),
            title: "Live health-check probe failing".into(),
            detail: format!(
                "A live HTTP probe of the env's health-check URL failed: {reason}. EB's \
                 internal health can lag or diverge from what an outside client sees — a \
                 failing external probe on an env you believe is healthy usually means the \
                 health path moved, a security group closed, or the app is erroring on \
                 paths EB's ELB checks don't exercise."
            ),
            suggestion: Some(
                "curl the URL from your network and fix what the response shows; re-run \
                 `ebman lint --probe-live` to confirm."
                    .into(),
            ),
            fields,
        })
    }
}

pub(crate) fn default_rules(disabled: &[String]) -> Vec<Box<dyn Rule>> {
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
        Box::new(ScalingTriggerLegacyNetworkMeasure),
        Box::new(HealthCheckProbeFailing),
        Box::new(ManagedActionsDisabled),
        Box::new(AllAtOnceMultiAz),
        Box::new(XrayEnabledButTracesDenied),
        Box::new(NoWafOnProdAlb),
    ];
    candidates
        .into_iter()
        .filter(|r| !disabled.iter().any(|d| d == r.id()))
        .collect()
}
