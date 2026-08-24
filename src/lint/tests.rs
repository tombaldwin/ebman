//! Tests for the lint framework and every rule.
//!
//! Moved wholesale out of `lint.rs` with the split; unchanged.

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
    let _: serde_json::Value =
        serde_json::from_str(&json).expect("rendered output must be valid JSON");
    // Spot-check the escape for the embedded quote + newline.
    assert!(json.contains("\\\"quotes\\\""));
    assert!(json.contains("\\n"));
    // Empty issues list — still a well-formed object.
    let empty = render_issues_json(&[]);
    let _: serde_json::Value = serde_json::from_str(&empty).unwrap();
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
            match (&applies_result, &fix_result) {
                // applies() said No → fix() must too. The CLI's
                // `applies → fix` loop in src/cli/lint.rs relies on this:
                // a rule that returned Some(FixAction) here would offer to
                // "fix" an issue that doesn't exist.
                (None, fix) => assert!(
                    fix.is_none(),
                    "{id} on tier={}: fix() returned Some({fix:?}) when applies() returned None — \
                     the `applies → fix` chain assumes this never happens. Either \
                     applies() should fire or fix() should short-circuit on None-applies.",
                    env.tier
                ),
                // applies() fired AND a fix is offered → the payload must be
                // well-formed. An empty namespace/name dispatches a malformed
                // UpdateEnvironment; an empty value silently no-ops the fix;
                // a blank Manual instruction prints "here's what to do: ".
                // None of these surface at the type level, so pin them here.
                (
                    Some(_),
                    Some(FixAction::SetOption {
                        namespace,
                        name,
                        value,
                        description,
                    }),
                ) => {
                    assert!(
                        !namespace.is_empty(),
                        "{id}: SetOption fix has empty namespace"
                    );
                    assert!(!name.is_empty(), "{id}: SetOption fix has empty name");
                    assert!(!value.is_empty(), "{id}: SetOption fix has empty value");
                    assert!(
                        !description.is_empty(),
                        "{id}: SetOption fix has empty description"
                    );
                }
                (Some(_), Some(FixAction::Manual { instructions })) => {
                    assert!(
                        !instructions.is_empty(),
                        "{id}: Manual fix has empty instructions"
                    );
                }
                // applies() fired but no fix offered — legal (EBL003 etc.).
                (Some(_), None) => {}
            }
        }
    }
    // Sanity: the registry has the expected size. Bumps when a
    // new EBL is added. Catches both regressions (rule removed)
    // and additions-without-test-update (new rule landed; review
    // whether its applies()/fix() satisfy the invariants above).
    assert_eq!(rules.len(), 19, "rule registry size changed");
}

// ── EBL016 — live health-check probe ────────────────────────────

#[test]
fn ebl016_fires_only_when_probe_failure_attached() {
    let env = mk_env("prod", "Web", "Green");
    let no_probe = ctx(&env, &[]);
    assert!(
        HealthCheckProbeFailing.applies(&no_probe).is_none(),
        "no probe run → skip (default lint stays silent)"
    );
    let failed = ctx(&env, &[]).with_health_probe_failure("HTTP 503");
    let issue = HealthCheckProbeFailing
        .applies(&failed)
        .expect("failure reason attached → fire");
    assert_eq!(issue.rule_id, "EBL016");
    assert!(issue.detail.contains("HTTP 503"));
    // Volatile reason must stay OUT of fields — issue_identity
    // hashes fields, and a flapping reason would churn baselines
    // + the webhook change-guard.
    assert!(!issue.fields.contains_key("probe_failure"));
    assert!(matches!(
        HealthCheckProbeFailing.fix(&failed),
        Some(FixAction::Manual { .. })
    ));
}

// ── EBL018 — prod ALB without WAF ───────────────────────────────

#[test]
fn ebl018_fires_only_on_probed_prod_envs() {
    let prod = mk_env("shop-Prod", "Web", "Green");
    let no_probe = ctx(&prod, &[]);
    assert!(
        NoWafOnProdAlb.applies(&no_probe).is_none(),
        "no probe run → skip (TUI / classic LB / probe error)"
    );
    let waf_present = ctx(&prod, &[]).with_waf_missing(false);
    assert!(NoWafOnProdAlb.applies(&waf_present).is_none());
    let missing = ctx(&prod, &[]).with_waf_missing(true);
    let issue = NoWafOnProdAlb.applies(&missing).expect("should fire");
    assert_eq!(issue.rule_id, "EBL018");
    assert_eq!(issue.severity, Severity::Warn);
    assert!(matches!(
        NoWafOnProdAlb.fix(&missing),
        Some(FixAction::Manual { .. })
    ));
    // Non-prod name skips even with a (mistaken) probe result.
    let staging = mk_env("shop-staging", "Web", "Green");
    let staging_missing = ctx(&staging, &[]).with_waf_missing(true);
    assert!(NoWafOnProdAlb.applies(&staging_missing).is_none());
}

#[test]
fn is_prod_named_matches_loosely() {
    assert!(is_prod_named("shop-prod"));
    assert!(is_prod_named("PRODUCTION-eu"));
    assert!(is_prod_named("api-prd-1"));
    assert!(!is_prod_named("shop-staging"));
    assert!(!is_prod_named("dev"));
}

// ── EBL015 — stale custom platform (account-level) ──────────────

#[test]
fn ebl015_fires_on_stale_platforms_only() {
    use chrono::{Duration, TimeZone, Utc};
    let now = Utc.with_ymd_and_hms(2026, 8, 20, 0, 0, 0).unwrap();
    let platforms = vec![
        ("old-tomcat".to_string(), now - Duration::days(400)),
        ("fresh-node".to_string(), now - Duration::days(30)),
        ("edge-exact".to_string(), now - Duration::days(180)),
    ];
    let issues = stale_custom_platform_issues(&platforms, now);
    assert_eq!(issues.len(), 2, "180d boundary is inclusive; 30d skips");
    for i in &issues {
        assert_eq!(i.rule_id, "EBL015");
        assert_eq!(i.severity, Severity::Info);
        assert!(i.env_name.is_none(), "account-level issue has no env");
    }
    assert!(issues[0].title.contains("edge-exact"));
    assert!(issues[1].title.contains("old-tomcat"));
    assert!(stale_custom_platform_issues(&[], now).is_empty());
}

// ── EBL014 — legacy network scaling trigger ─────────────────────

#[test]
fn ebl014_fires_on_network_measure_when_asg_scales() {
    let env = mk_env("prod", "Web", "Green");
    let opts = vec![
        mk_opt("aws:autoscaling:trigger", "MeasureName", "NetworkOut"),
        mk_opt("aws:autoscaling:asg", "MinSize", "2"),
        mk_opt("aws:autoscaling:asg", "MaxSize", "6"),
    ];
    let issue = ScalingTriggerLegacyNetworkMeasure
        .applies(&ctx(&env, &opts))
        .expect("should fire");
    assert_eq!(issue.rule_id, "EBL014");
    assert!(issue.title.contains("NetworkOut"));
    assert_eq!(issue.fields.get("max_size").map(String::as_str), Some("6"));
    assert!(matches!(
        ScalingTriggerLegacyNetworkMeasure.fix(&ctx(&env, &opts)),
        Some(FixAction::Manual { .. })
    ));
}

#[test]
fn ebl014_skips_fixed_size_asg_and_modern_measures() {
    let env = mk_env("prod", "Web", "Green");
    // min == max: the trigger is inert — no warning.
    let fixed = vec![
        mk_opt("aws:autoscaling:trigger", "MeasureName", "NetworkOut"),
        mk_opt("aws:autoscaling:asg", "MinSize", "3"),
        mk_opt("aws:autoscaling:asg", "MaxSize", "3"),
    ];
    assert!(ScalingTriggerLegacyNetworkMeasure
        .applies(&ctx(&env, &fixed))
        .is_none());
    // CPU-based trigger: the modern default — no warning.
    let cpu = vec![
        mk_opt("aws:autoscaling:trigger", "MeasureName", "CPUUtilization"),
        mk_opt("aws:autoscaling:asg", "MinSize", "2"),
        mk_opt("aws:autoscaling:asg", "MaxSize", "6"),
    ];
    assert!(ScalingTriggerLegacyNetworkMeasure
        .applies(&ctx(&env, &cpu))
        .is_none());
    // No trigger options at all (options not loaded): skip.
    assert!(ScalingTriggerLegacyNetworkMeasure
        .applies(&ctx(&env, &[]))
        .is_none());
}

// ── EBL020 — X-Ray enabled but traces denied ────────────────────

#[test]
fn ebl020_fires_only_when_probe_says_denied() {
    let env = mk_env("prod", "Web", "Green");
    let opts = vec![
        mk_opt("aws:elasticbeanstalk:xray", "XRayEnabled", "true"),
        mk_opt(
            "aws:autoscaling:launchconfiguration",
            "IamInstanceProfile",
            "aws-elasticbeanstalk-ec2-role",
        ),
    ];
    let denied = ctx(&env, &opts).with_xray_trace_denied(true);
    let issue = XrayEnabledButTracesDenied
        .applies(&denied)
        .expect("should fire when probe says denied");
    assert_eq!(issue.rule_id, "EBL020");
    assert_eq!(
        issue.fields.get("instance_profile").map(String::as_str),
        Some("aws-elasticbeanstalk-ec2-role")
    );
    assert!(matches!(
        XrayEnabledButTracesDenied.fix(&denied),
        Some(FixAction::Manual { .. })
    ));
    // Probe says allowed → no issue.
    let allowed = ctx(&env, &opts).with_xray_trace_denied(false);
    assert!(XrayEnabledButTracesDenied.applies(&allowed).is_none());
    // Probe didn't run (TUI path) → skip, never false-positive.
    assert!(XrayEnabledButTracesDenied
        .applies(&ctx(&env, &opts))
        .is_none());
}

#[test]
fn ebl020_skips_when_xray_disabled_even_if_probe_denied() {
    let env = mk_env("prod", "Web", "Green");
    let opts = vec![mk_opt("aws:elasticbeanstalk:xray", "XRayEnabled", "false")];
    let c = ctx(&env, &opts).with_xray_trace_denied(true);
    assert!(XrayEnabledButTracesDenied.applies(&c).is_none());
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

/// A degraded run and a clean run must not produce identical JSON.
///
/// A probe that could not run makes its rule skip rather than report
/// a false positive — right, but it meant `--json` emitted the same
/// bytes whether every check ran or half were skipped on
/// AccessDenied. The human output made the distinction;
/// the machine output flattened it back, which is what
/// `ProbeOutcome::Unknown` exists to prevent.
#[test]
fn report_json_distinguishes_a_degraded_run_from_a_clean_one() {
    let issues: Vec<Issue> = Vec::new();

    let clean = render_report_json(&issues, &[]);
    assert!(
        clean.contains(r#""degraded":false"#),
        "a clean run must say so: {clean}"
    );
    assert!(clean.contains(r#""degraded_reasons":[]"#), "{clean}");

    let degraded = render_report_json(
        &issues,
        &["EBL020 on api-prod: AccessDenied: iam:SimulatePrincipalPolicy".to_string()],
    );
    assert!(
        degraded.contains(r#""degraded":true"#),
        "a degraded run must say so: {degraded}"
    );
    assert!(
        degraded.contains("SimulatePrincipalPolicy"),
        "and must say WHY, or a consumer can only guess: {degraded}"
    );
    assert_ne!(
        clean, degraded,
        "the whole point: these must not be byte-identical"
    );
}

/// The report wraps the baseline renderer, so the issues themselves
/// must survive the wrapping and the result must still parse.
#[test]
fn report_json_is_valid_json_and_keeps_the_issues() {
    let issues = vec![Issue {
        rule_id: "EBL001".into(),
        severity: Severity::Warn,
        env_name: Some("api-prod".into()),
        title: "single instance".into(),
        detail: r#"quotes " and \ backslash"#.into(),
        suggestion: None,
        fields: Default::default(),
    }];
    let body = render_report_json(&issues, &["a \"quoted\" reason".to_string()]);
    let parsed: serde_json::Value =
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("must be valid JSON: {e}\n{body}"));
    assert_eq!(parsed["issues"][0]["rule_id"], "EBL001");
    assert_eq!(parsed["degraded"], true);
    assert_eq!(parsed["degraded_reasons"][0], r#"a "quoted" reason"#);
}
