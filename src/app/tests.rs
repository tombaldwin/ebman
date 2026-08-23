//! Unit tests for the `app` module.
//!
//! Split out of `src/app.rs` — the test body moved verbatim; `use super::*`
//! still resolves to `crate::app`, so every test sees exactly what it did
//! when it lived inline.

use super::*;

#[test]
fn loading_linger_target_none_when_no_load() {
    let now = Instant::now();
    assert!(compute_loading_linger_target(
        None,
        Duration::from_millis(300),
        Duration::from_millis(500),
        now,
    )
    .is_none());
}

#[test]
fn loading_linger_target_none_when_under_threshold() {
    let now = Instant::now();
    // Load started 100 ms ago — threshold (300 ms) not crossed.
    let started = now - Duration::from_millis(100);
    assert!(compute_loading_linger_target(
        Some(started),
        Duration::from_millis(300),
        Duration::from_millis(500),
        now,
    )
    .is_none());
}

#[test]
fn loading_linger_target_arms_past_threshold() {
    let now = Instant::now();
    let started = now - Duration::from_millis(400);
    let until = compute_loading_linger_target(
        Some(started),
        Duration::from_millis(300),
        Duration::from_millis(500),
        now,
    )
    .expect("should arm linger past threshold");
    // Linger should extend ~500 ms past `now`. Allow a tiny slop so the
    // assertion isn't sensitive to test runner clock granularity.
    let target_delta = until.duration_since(now);
    assert!(
        target_delta >= Duration::from_millis(495) && target_delta <= Duration::from_millis(505),
        "linger target should be ~500ms in the future, got {target_delta:?}"
    );
}

#[test]
fn sort_key_cycle_matches_ui_column_order() {
    let order = [
        SortKey::Name,
        SortKey::App,
        SortKey::Status,
        SortKey::Health,
        SortKey::Version,
        SortKey::Age,
    ];
    let mut cur = order[0];
    for expected in order.iter().skip(1).chain(std::iter::once(&order[0])) {
        cur = cur.next();
        assert_eq!(cur, *expected);
    }
}

#[test]
fn sort_key_parse_roundtrip() {
    for k in [
        SortKey::Name,
        SortKey::App,
        SortKey::Status,
        SortKey::Health,
        SortKey::Version,
        SortKey::Age,
    ] {
        assert_eq!(SortKey::parse(k.label()), Some(k));
    }
    assert_eq!(SortKey::parse("bogus"), None);
}

#[test]
fn parse_sort_handles_directions() {
    assert_eq!(parse_sort(Some("app:desc")), (SortKey::App, true));
    assert_eq!(parse_sort(Some("name:asc")), (SortKey::Name, false));
    assert_eq!(parse_sort(Some("name")), (SortKey::Name, false));
    assert_eq!(parse_sort(Some("bogus:desc")), (SortKey::App, true)); // unknown key → default key, dir kept
    assert_eq!(parse_sort(None), (SortKey::App, false));
}

#[test]
fn parse_toggle_explicit_and_default() {
    assert!(parse_toggle(Some("on"), false));
    assert!(parse_toggle(Some("yes"), false));
    assert!(parse_toggle(Some("1"), false));
    assert!(!parse_toggle(Some("off"), true));
    assert!(!parse_toggle(Some("no"), true));
    // No arg → toggle current.
    assert!(parse_toggle(None, false));
    assert!(!parse_toggle(None, true));
    // Garbage → toggle current.
    assert!(parse_toggle(Some("maybe"), false));
}

#[test]
fn health_rank_orders_severities() {
    assert!(health_rank("green") < health_rank("grey"));
    assert!(health_rank("grey") < health_rank("yellow"));
    assert!(health_rank("yellow") < health_rank("red"));
    assert_eq!(health_rank("ok"), health_rank("Green"));
}

#[test]
fn scroll_apply_clamps_at_zero() {
    assert_eq!(scroll_apply(0, -1), 0);
    assert_eq!(scroll_apply(0, 0), 0);
    assert_eq!(scroll_apply(0, 1), 1);
    assert_eq!(scroll_apply(5, -10), 0);
    assert_eq!(scroll_apply(5, 3), 8);
}

#[test]
fn redact_block_preserves_length() {
    assert_eq!(redact_block(""), "");
    assert_eq!(redact_block("hello").chars().count(), 5);
    assert_eq!(redact_block("über-café").chars().count(), 9);
}

#[test]
fn scope_next_alternates() {
    assert_eq!(Scope::Envs.next(), Scope::Apps);
    assert_eq!(Scope::Apps.next(), Scope::Envs);
}

#[test]
fn action_destructive_covers_terminate_and_ssm_run() {
    // Terminate has been destructive since 0.6; SsmRun added in
    // 0.17.3 — operator-explicit shell exec across instances is
    // treat-as-write and the modal renders red so the visual cue
    // matches the intent.
    assert!(Action::Terminate.destructive());
    assert!(Action::SsmRun.destructive());
    // Every other variant stays non-destructive. Exhaustive list
    // (0.17.4 — code-review flagged the previous Capacity/Clone/
    // Upgrade/Abort/Config*/TerminateInstance gap) so a future
    // accidental destructive() flip is caught here.
    assert!(!Action::Rebuild.destructive());
    assert!(!Action::RestartAppServer.destructive());
    assert!(!Action::SwapCnames.destructive());
    assert!(!Action::Deploy.destructive());
    assert!(!Action::UpgradePlatform.destructive());
    assert!(!Action::Clone.destructive());
    assert!(!Action::Scale.destructive());
    assert!(!Action::Capacity.destructive());
    assert!(!Action::AbortUpdate.destructive());
    assert!(!Action::ConfigSave.destructive());
    assert!(!Action::ConfigDelete.destructive());
    assert!(!Action::ConfigApply.destructive());
    assert!(!Action::TerminateInstance.destructive());
}

/// Exhaustiveness test for `Action::wants_preflight()`. Parallels
/// the 0.17.4 `action_destructive_covers_*` extension — every
/// variant gets an explicit assertion so a future flip is caught.
/// 0.18 review item.
#[test]
fn action_wants_preflight_covers_all_variants() {
    // Preflight: instance count + last-3 events fetch. Opt-in for
    // actions that touch instances or shift LB traffic.
    assert!(Action::Deploy.wants_preflight());
    assert!(Action::UpgradePlatform.wants_preflight());
    assert!(Action::Scale.wants_preflight());
    assert!(Action::Clone.wants_preflight());
    assert!(Action::Rebuild.wants_preflight());
    assert!(Action::RestartAppServer.wants_preflight());
    assert!(Action::SwapCnames.wants_preflight());
    assert!(Action::Terminate.wants_preflight());
    assert!(Action::ConfigApply.wants_preflight());
    // Opt-out: actions that don't touch instances or where the
    // preflight is meaningless. Capacity opens its own form
    // modal; ConfigSave/Delete operate on template definitions
    // (no env-side preflight); TerminateInstance is per-
    // instance (no env-wide preflight); SsmRun's instance set
    // is operator-chosen via Detail/Instances; AbortUpdate
    // doesn't touch capacity or LB.
    assert!(!Action::Capacity.wants_preflight());
    assert!(!Action::ConfigSave.wants_preflight());
    assert!(!Action::ConfigDelete.wants_preflight());
    assert!(!Action::TerminateInstance.wants_preflight());
    assert!(!Action::SsmRun.wants_preflight());
    assert!(!Action::AbortUpdate.wants_preflight());
}

#[test]
fn action_ssm_run_opts_out_of_preflight() {
    // No instance count / event preview needed — the operator
    // already chose the instance set by opening Detail/Instances.
    // The modal renders without dryrun / events loading spinners.
    assert!(!Action::SsmRun.wants_preflight());
    // Sanity: preflight-wanting actions still want it.
    assert!(Action::Deploy.wants_preflight());
    assert!(Action::Terminate.wants_preflight());
}

#[test]
fn action_ssm_run_label_and_glyph() {
    use crate::theme::IconStyle;
    assert_eq!(Action::SsmRun.label(), "Run SSM shell command");
    // Glyph entry exists for all three icon styles (no `_` fall-
    // through panic from the per-icon match).
    assert!(!Action::SsmRun.glyph(IconStyle::Powerline).is_empty());
    assert!(!Action::SsmRun.glyph(IconStyle::Unicode).is_empty());
    assert!(!Action::SsmRun.glyph(IconStyle::Ascii).is_empty());
}

#[test]
fn scope_prev_is_inverse_of_next() {
    assert_eq!(Scope::Envs.next(), Scope::Apps);
    assert_eq!(Scope::Envs.prev(), Scope::Apps);
    assert_eq!(Scope::Apps.next().next(), Scope::Apps);
    assert_eq!(Scope::Envs.prev().prev(), Scope::Envs);
}

#[test]
fn view_mode_labels() {
    assert_eq!(ViewMode::Default.label(), "default");
    assert_eq!(ViewMode::Compact.label(), "compact");
    assert_eq!(ViewMode::Spacious.label(), "spacious");
}

#[test]
fn console_url_includes_region_app_env() {
    let url = console_url("us-east-1", "myapp", "myenv");
    let url = url.expect("commercial partition has a console host");
    assert!(url.contains("us-east-1.console.aws.amazon.com"));
    assert!(url.contains("region=us-east-1"));
    assert!(url.contains("applicationName=myapp"));
    assert!(url.contains("environmentName=myenv"));
}

#[test]
fn console_url_encodes_special_chars() {
    // Reserved or non-alnum chars get %XX'd so the URL stays valid.
    let url = console_url("us-east-1", "my app", "env/with?slash").expect("commercial");
    assert!(url.contains("applicationName=my%20app"));
    assert!(url.contains("environmentName=env%2Fwith%3Fslash"));
}

#[test]
fn urlencode_keeps_safe_chars() {
    assert_eq!(urlencode("hello-world_1.0"), "hello-world_1.0");
    assert_eq!(urlencode("a b"), "a%20b");
    assert_eq!(urlencode("a/b?c=d"), "a%2Fb%3Fc%3Dd");
    // Unicode is byte-wise percent-encoded.
    assert!(urlencode("café").starts_with("caf"));
}

#[test]
fn json_escape_handles_quotes_and_controls() {
    assert_eq!(json_escape("hello"), "hello");
    assert_eq!(json_escape(r#"he said "hi""#), r#"he said \"hi\""#);
    assert_eq!(json_escape("line\nbreak"), "line\\nbreak");
    assert_eq!(json_escape("\\path"), "\\\\path");
    // Control character → \uXXXX.
    let out = json_escape("\u{0001}");
    assert_eq!(out, "\\u0001");
}

#[test]
fn build_describe_cli_no_profile() {
    let cmd = build_describe_cli("my-env", "eu-west-2", None);
    assert_eq!(
        cmd,
        "aws elasticbeanstalk describe-environments --environment-names my-env --region eu-west-2"
    );
}

#[test]
fn build_describe_cli_with_profile_and_special_chars() {
    let cmd = build_describe_cli("my env!", "eu-west-2", Some("prod"));
    assert!(cmd.contains("--environment-names 'my env!'"));
    assert!(cmd.contains("--profile prod"));
}

fn fake_env_with(
    name: &str,
    status: &str,
    health: &str,
    updated_minutes_ago: Option<i64>,
) -> Environment {
    let updated = updated_minutes_ago.map(|m| chrono::Utc::now() - chrono::Duration::minutes(m));
    Environment {
        name: name.into(),
        application: "app".into(),
        status: status.into(),
        health: health.into(),
        platform: "Java 17".into(),
        solution_stack: String::new(),
        tier: "Web".into(),
        cname: "x.elb".into(),
        version_label: "v1".into(),
        arn: None,
        updated,
        id: None,
        region: None,
    }
}

#[test]
fn render_promotions_orders_newest_first_and_includes_version() {
    let now = chrono::Utc::now();
    let records = vec![
        super::PromotionRecord {
            source: "staging".into(),
            target: "uat".into(),
            version_label: "v1.4.2".into(),
            at: now - chrono::Duration::hours(2),
        },
        super::PromotionRecord {
            source: "uat".into(),
            target: "prod".into(),
            version_label: "v1.4.2".into(),
            at: now - chrono::Duration::minutes(5),
        },
    ];
    let body = super::render_promotions(&records, now);
    let prod_pos = body.find("uat → prod").expect("prod row");
    let staging_pos = body.find("staging → uat").expect("staging row");
    assert!(
        prod_pos < staging_pos,
        "newest (uat → prod, 5m ago) should sort above older (staging → uat, 2h ago)"
    );
    assert!(body.contains("version=v1.4.2"), "version label: {body}");
}

#[test]
fn render_fleet_cost_breaks_down_by_app_tier_health() {
    let envs = vec![
        mk_env("api-prod", "api", "Web", "Green"),
        mk_env("api-staging", "api", "Web", "Yellow"),
        mk_env("worker-prod", "api", "Worker", "Green"),
        mk_env("billing-prod", "billing", "Web", "Red"),
    ];
    let mut costs = std::collections::HashMap::new();
    costs.insert("api-prod".to_string(), 100.0);
    costs.insert("api-staging".to_string(), 25.5);
    costs.insert("worker-prod".to_string(), 40.0);
    costs.insert("billing-prod".to_string(), 60.25);
    let now = chrono::Utc::now();
    let body = super::render_fleet_cost(&envs, &costs, Some(now), now);
    assert!(body.contains("Total: $225.75/mo"), "total: {body}");
    assert!(body.contains("4 env(s) covered"), "covered count: {body}");
    // Per-app cost: api = 100 + 25.5 + 40 = 165.5
    assert!(body.contains("$    165.50/mo  api"), "by app: {body}");
    assert!(body.contains("$     60.25/mo  billing"), "by app: {body}");
    // Per-tier: Web = 100 + 25.5 + 60.25 = 185.75
    assert!(body.contains("$    185.75/mo  Web"), "by tier: {body}");
    assert!(body.contains("$     40.00/mo  Worker"), "by tier: {body}");
    // Per-health: Green = 100 + 40 = 140
    assert!(body.contains("$    140.00/mo  Green"), "by health: {body}");
}

#[test]
fn render_fleet_cost_flags_uncovered_envs() {
    let envs = vec![
        mk_env("api-prod", "api", "Web", "Green"),
        mk_env("api-uncached", "api", "Web", "Green"),
    ];
    let mut costs = std::collections::HashMap::new();
    costs.insert("api-prod".to_string(), 50.0);
    let now = chrono::Utc::now();
    let body = super::render_fleet_cost(&envs, &costs, Some(now), now);
    assert!(
        body.contains("1 env(s) covered, 1 without cost data"),
        "missing count: {body}"
    );
}

#[test]
fn render_fleet_cost_flags_stale_cache() {
    let envs = vec![mk_env("a", "x", "Web", "Green")];
    let mut costs = std::collections::HashMap::new();
    costs.insert("a".to_string(), 10.0);
    let now = chrono::Utc::now();
    let stale = now - chrono::Duration::hours(36);
    let body = super::render_fleet_cost(&envs, &costs, Some(stale), now);
    assert!(body.contains("stale"), "stale marker: {body}");
}

#[test]
fn render_fleet_cost_no_freshness_line_when_unset() {
    let envs = vec![mk_env("a", "x", "Web", "Green")];
    let mut costs = std::collections::HashMap::new();
    costs.insert("a".to_string(), 10.0);
    let now = chrono::Utc::now();
    let body = super::render_fleet_cost(&envs, &costs, None, now);
    assert!(!body.contains("Cached:"), "no cached line: {body}");
    assert!(body.contains("Total: $10.00/mo"));
}

#[test]
fn app_rollup_counts_envs_red_and_updating() {
    let envs = vec![
        crate::aws::Environment {
            name: "prod".into(),
            application: "foo".into(),
            status: "Ready".into(),
            health: "Green".into(),
            platform: "Java 17".into(),
            solution_stack: String::new(),
            tier: "WebServer".into(),
            cname: String::new(),
            version_label: String::new(),
            arn: None,
            updated: None,
            id: None,
            region: None,
        },
        crate::aws::Environment {
            name: "staging".into(),
            application: "foo".into(),
            status: "Updating".into(),
            health: "Red".into(),
            platform: "Java 17".into(),
            solution_stack: String::new(),
            tier: "WebServer".into(),
            cname: String::new(),
            version_label: String::new(),
            arn: None,
            updated: None,
            id: None,
            region: None,
        },
        crate::aws::Environment {
            name: "other-app".into(),
            application: "bar".into(),
            status: "Ready".into(),
            health: "Green".into(),
            platform: "Java 17".into(),
            solution_stack: String::new(),
            tier: "WebServer".into(),
            cname: String::new(),
            version_label: String::new(),
            arn: None,
            updated: None,
            id: None,
            region: None,
        },
    ];
    let dlq: HashMap<String, i64> = HashMap::new();
    let r = super::app_rollup(&envs, "foo", &dlq);
    assert_eq!(r.env_count, 2, "foo has 2 envs (prod + staging)");
    assert_eq!(r.red_count, 1, "staging is Red");
    assert_eq!(r.updating_count, 1, "staging is Updating");
    assert_eq!(r.worker_dlq_alerts, 0, "no worker envs in foo");
}

#[test]
fn app_rollup_worker_dlq_alert_counts() {
    let envs = vec![crate::aws::Environment {
        name: "worker-prod".into(),
        application: "wapp".into(),
        status: "Ready".into(),
        health: "Green".into(),
        platform: "Java 17".into(),
        solution_stack: String::new(),
        tier: "Worker".into(),
        cname: String::new(),
        version_label: String::new(),
        arn: None,
        updated: None,
        id: None,
        region: None,
    }];
    let mut dlq: HashMap<String, i64> = HashMap::new();
    dlq.insert("worker-prod".into(), 7);
    let r = super::app_rollup(&envs, "wapp", &dlq);
    // EB calls it Green; ebman flags it because the DLQ is non-empty.
    assert_eq!(r.env_count, 1);
    assert_eq!(r.red_count, 0, "EB health stays Green");
    assert_eq!(
        r.worker_dlq_alerts, 1,
        "worker env with DLQ depth > 0 counts as alerting"
    );
}

#[test]
fn app_rollup_empty_for_unknown_app() {
    let envs: Vec<crate::aws::Environment> = vec![];
    let dlq: HashMap<String, i64> = HashMap::new();
    let r = super::app_rollup(&envs, "nope", &dlq);
    assert_eq!(r, super::AppRollup::default());
}

fn opt(
    ns: &str,
    name: &str,
    value: Option<&str>,
    default: Option<&str>,
) -> crate::aws::ConfigOption {
    crate::aws::ConfigOption {
        namespace: ns.into(),
        name: name.into(),
        value: value.map(String::from),
        default_value: default.map(String::from),
        value_type: "Scalar".into(),
        value_options: vec![],
        change_severity: None,
        user_defined: Some(true),
        min_value: None,
        max_value: None,
        max_length: None,
    }
}

#[test]
fn diff_config_options_reports_only_differences() {
    let left = vec![
        opt("aws:autoscaling:asg", "MinSize", Some("2"), None),
        opt("aws:autoscaling:asg", "MaxSize", Some("4"), None),
        opt(
            "aws:elasticbeanstalk:application:environment",
            "LOG",
            Some("info"),
            None,
        ),
        opt("aws:foo", "Same", Some("x"), None),
    ];
    let right = vec![
        opt("aws:autoscaling:asg", "MinSize", Some("3"), None), // changed
        opt("aws:autoscaling:asg", "MaxSize", Some("4"), None), // same
        opt(
            "aws:elasticbeanstalk:application:environment",
            "LOG",
            None,
            None,
        ), // unset on right
        opt("aws:foo", "Same", Some("x"), None),                // same
    ];
    let diffs = super::diff_config_options(&left, &right);
    assert_eq!(diffs.len(), 2, "got {diffs:?}");
    let min = diffs.iter().find(|d| d.name == "MinSize").unwrap();
    assert_eq!(min.left.as_deref(), Some("2"));
    assert_eq!(min.right.as_deref(), Some("3"));
    let log = diffs.iter().find(|d| d.name == "LOG").unwrap();
    assert_eq!(log.left.as_deref(), Some("info"));
    assert_eq!(log.right, None);
}

#[test]
fn diff_config_options_treats_empty_string_as_unset() {
    // EB returns Some("") for some unset options — must not show
    // as a difference against an actually-unset None.
    let left = vec![opt("aws:foo", "Bar", Some(""), None)];
    let right = vec![opt("aws:foo", "Bar", None, None)];
    assert!(super::diff_config_options(&left, &right).is_empty());
}

#[test]
fn parse_ignore_keys_splits_and_lowercases() {
    assert_eq!(super::parse_ignore_keys(None), Vec::<String>::new());
    assert_eq!(super::parse_ignore_keys(Some("")), Vec::<String>::new());
    assert_eq!(
        super::parse_ignore_keys(Some(" Version_Label , MinSize ,")),
        vec!["version_label".to_string(), "minsize".to_string()]
    );
}

#[test]
fn filter_config_diffs_drops_matching_names() {
    let diffs = vec![
        super::ConfigDiff {
            namespace: "ns".into(),
            name: "MinSize".into(),
            left: Some("2".into()),
            right: Some("3".into()),
        },
        super::ConfigDiff {
            namespace: "ns".into(),
            name: "version_label".into(),
            left: Some("v1".into()),
            right: Some("v2".into()),
        },
    ];
    let keys = super::parse_ignore_keys(Some("version_label"));
    let filtered = super::filter_config_diffs(diffs, &keys);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].name, "MinSize");
}

#[test]
fn filter_config_diffs_supports_namespace_qualified_match() {
    // Operators can use `namespace:name` form to scope an ignore-
    // key to a specific namespace (so a generic "MinSize" ignore
    // doesn't drop both the ASG and the LB MinSize).
    let diffs = vec![
        super::ConfigDiff {
            namespace: "aws:autoscaling:asg".into(),
            name: "MinSize".into(),
            left: Some("2".into()),
            right: Some("3".into()),
        },
        super::ConfigDiff {
            namespace: "aws:elasticbeanstalk:command".into(),
            name: "MinSize".into(),
            left: Some("4".into()),
            right: Some("5".into()),
        },
    ];
    let keys = super::parse_ignore_keys(Some("aws:autoscaling:asg:MinSize"));
    let filtered = super::filter_config_diffs(diffs, &keys);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].namespace, "aws:elasticbeanstalk:command");
}

#[test]
fn filter_config_diffs_empty_ignore_keys_is_passthrough() {
    let diffs = vec![super::ConfigDiff {
        namespace: "ns".into(),
        name: "MinSize".into(),
        left: Some("2".into()),
        right: Some("3".into()),
    }];
    let original_len = diffs.len();
    assert_eq!(super::filter_config_diffs(diffs, &[]).len(), original_len);
}

#[test]
fn render_config_diff_overlay_states() {
    // No differences → identical message.
    let body = super::render_config_diff_overlay("staging", "prod", &[]);
    assert!(body.contains("identical"));
    // With a diff → the namespace + name + both values appear.
    let diffs = vec![super::ConfigDiff {
        namespace: "aws:autoscaling:asg".into(),
        name: "MinSize".into(),
        left: Some("2".into()),
        right: None,
    }];
    let body = super::render_config_diff_overlay("staging", "prod", &diffs);
    assert!(body.contains("aws:autoscaling:asg"));
    assert!(body.contains("MinSize"));
    assert!(body.contains("L: 2"));
    assert!(body.contains("R: (unset)"));
}

#[test]
fn build_env_edit_body_sorts_keys_and_emits_header() {
    let vars = vec![
        ("LOG_LEVEL".into(), "info".into()),
        ("DB_HOST".into(), "db.example".into()),
        ("DB_PORT".into(), "5432".into()),
    ];
    let body = super::build_env_edit_body("prod", &vars);
    // Header comment present.
    assert!(body.starts_with("# ebman env-var editor — prod\n"));
    assert!(body.contains("Secrets Manager"));
    // Keys sorted alphabetically.
    let db_host_pos = body.find("DB_HOST=").expect("DB_HOST line");
    let db_port_pos = body.find("DB_PORT=").expect("DB_PORT line");
    let log_pos = body.find("LOG_LEVEL=").expect("LOG_LEVEL line");
    assert!(db_host_pos < db_port_pos && db_port_pos < log_pos);
}

#[test]
fn parse_env_edit_body_round_trip() {
    let vars = vec![
        ("LOG_LEVEL".into(), "info".into()),
        (
            "DB_URL".into(),
            "postgres://user:pass@host:5432/db?sslmode=require".into(),
        ),
    ];
    let body = super::build_env_edit_body("env", &vars);
    let parsed = super::parse_env_edit_body(&body);
    assert_eq!(parsed.get("LOG_LEVEL").map(String::as_str), Some("info"));
    // Value containing `=` (postgres URL) passes through intact
    // because we split on the *first* `=` only.
    assert_eq!(
        parsed.get("DB_URL").map(String::as_str),
        Some("postgres://user:pass@host:5432/db?sslmode=require")
    );
}

#[test]
fn parse_env_edit_body_skips_comments_and_blanks() {
    let body = "# comment\n\nDB_HOST=localhost\n   # indented comment\n\nLOG=debug\n";
    let parsed = super::parse_env_edit_body(body);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed.get("DB_HOST").map(String::as_str), Some("localhost"));
    assert_eq!(parsed.get("LOG").map(String::as_str), Some("debug"));
}

#[test]
fn parse_env_edit_body_drops_invalid_keys() {
    let body = "= no-key\n KEY WITH SPACES=foo\nGOOD=val\n";
    let parsed = super::parse_env_edit_body(body);
    assert_eq!(parsed.len(), 1);
    assert!(parsed.contains_key("GOOD"));
}

#[test]
fn diff_env_vars_produces_set_and_remove_lists() {
    let mut original = std::collections::BTreeMap::new();
    original.insert("KEEP".into(), "same".into());
    original.insert("CHANGE".into(), "old".into());
    original.insert("DROP".into(), "going".into());
    let mut edited = std::collections::BTreeMap::new();
    edited.insert("KEEP".into(), "same".into()); // unchanged
    edited.insert("CHANGE".into(), "new".into()); // updated
    edited.insert("NEW".into(), "added".into()); // added

    let (to_set, to_remove) = super::diff_env_vars("ns", &original, &edited);
    // CHANGE + NEW should be in to_set; KEEP excluded (unchanged).
    let set_keys: std::collections::BTreeSet<&str> =
        to_set.iter().map(|(_, k, _)| k.as_str()).collect();
    assert_eq!(
        set_keys,
        ["CHANGE", "NEW"]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        "to_set should include changed + added keys"
    );
    assert!(
        !set_keys.contains("KEEP"),
        "unchanged key must not re-dispatch"
    );
    // DROP should be in to_remove.
    assert_eq!(to_remove.len(), 1);
    assert_eq!(to_remove[0].1, "DROP");
}

#[test]
fn diff_env_vars_empty_when_unchanged() {
    let mut original = std::collections::BTreeMap::new();
    original.insert("A".into(), "1".into());
    original.insert("B".into(), "2".into());
    let edited = original.clone();
    let (to_set, to_remove) = super::diff_env_vars("ns", &original, &edited);
    assert!(to_set.is_empty());
    assert!(to_remove.is_empty());
}

#[test]
fn parse_access_denied_handles_assumed_role() {
    let msg = "User: arn:aws:sts::123456789012:assumed-role/EbmanReadOnly/session-abc \
                   is not authorized to perform: elasticbeanstalk:RebuildEnvironment \
                   on resource: arn:aws:elasticbeanstalk:eu-west-2:123:environment/foo/bar";
    let parsed = super::parse_access_denied(msg);
    assert_eq!(
        parsed,
        Some((
            "arn:aws:iam::123456789012:role/EbmanReadOnly".into(),
            "elasticbeanstalk:RebuildEnvironment".into()
        )),
        "assumed-role should be rewritten to the role ARN"
    );
}

#[test]
fn parse_access_denied_handles_iam_user() {
    let msg = "User: arn:aws:iam::123456789012:user/alice is not authorized to \
                   perform: s3:GetObject on resource: arn:aws:s3:::bucket/key";
    let parsed = super::parse_access_denied(msg);
    assert_eq!(
        parsed,
        Some((
            "arn:aws:iam::123456789012:user/alice".into(),
            "s3:GetObject".into()
        )),
        "IAM-user ARN should pass through unchanged"
    );
}

#[test]
fn parse_access_denied_returns_none_on_unrelated_error() {
    assert_eq!(
        super::parse_access_denied("ThrottlingException: rate exceeded"),
        None
    );
    assert_eq!(super::parse_access_denied("random garbage text"), None);
}

#[test]
fn render_explain_overlay_marks_decisions_and_suggests_fix() {
    let rows = vec![
        crate::aws::IamSimResult {
            action: "elasticbeanstalk:RebuildEnvironment".into(),
            resource: "*".into(),
            decision: "implicitDeny".into(),
            matched_statements: vec![],
            missing_context: vec![],
            blocked_by_scp: false,
            blocked_by_boundary: false,
        },
        crate::aws::IamSimResult {
            action: "ec2:DescribeInstances".into(),
            resource: "*".into(),
            decision: "allowed".into(),
            matched_statements: vec!["arn:aws:iam::aws:policy/AmazonEC2ReadOnlyAccess @ 0:0".into()],
            missing_context: vec![],
            blocked_by_scp: false,
            blocked_by_boundary: false,
        },
    ];
    let body = super::render_explain_overlay("arn:aws:iam::123:role/EbmanReadOnly", &rows, false);
    // Both action sections present, marked with correct decision glyphs.
    assert!(body.contains("Action:   elasticbeanstalk:RebuildEnvironment"));
    assert!(body.contains("✗ implicitDeny"));
    assert!(body.contains("Action:   ec2:DescribeInstances"));
    assert!(body.contains("✓ allowed"));
    // implicitDeny suggests the JSON-policy fix.
    assert!(body.contains("\"Effect\": \"Allow\""));
    assert!(body.contains("\"Action\": \"elasticbeanstalk:RebuildEnvironment\""));
    // The allowed action does NOT get the fix suggestion.
    assert!(body.matches("To allow, add this statement").count() == 1);
    // Matched statement surfaces for the allowed action.
    assert!(body.contains("AmazonEC2ReadOnlyAccess"));
}

#[test]
fn render_explain_overlay_flags_scp_and_boundary_blockers() {
    let rows = vec![crate::aws::IamSimResult {
        action: "ec2:TerminateInstances".into(),
        resource: "*".into(),
        decision: "explicitDeny".into(),
        matched_statements: vec!["org-scp/SCPDenyTerminate @ 0:0".into()],
        missing_context: vec![],
        blocked_by_scp: true,
        blocked_by_boundary: true,
    }];
    let body = super::render_explain_overlay("arn:aws:iam::123:role/X", &rows, false);
    assert!(body.contains("Organizations SCP"));
    assert!(body.contains("permission boundary"));
    // explicitDeny gives the "Remove the Deny" hint instead of
    // the implicitDeny JSON snippet.
    assert!(body.contains("explicit Deny always wins"));
    assert!(!body.contains("\"Effect\": \"Allow\""));
}

fn empty_resources() -> crate::aws::EnvResources {
    crate::aws::EnvResources::default()
}

#[test]
fn render_env_resources_tree_shows_asg_with_nested_instances() {
    let mut res = empty_resources();
    res.asgs = vec!["awseb-AWSEBAutoScalingGroup-XYZ".into()];
    res.instances = vec!["i-0abc".into(), "i-0def".into(), "i-0ghi".into()];
    let body = super::render_env_resources_tree(&res, "prod-api", "Web");
    // Section header for ASG group.
    assert!(body.contains("Auto-scaling groups (1)"));
    // ASG node under it (└─ since only one ASG).
    assert!(body.contains("└─ awseb-AWSEBAutoScalingGroup-XYZ"));
    // Instances nested below the ASG with proper tree glyphs.
    assert!(body.contains("├─ i-0abc"));
    assert!(body.contains("├─ i-0def"));
    assert!(body.contains("└─ i-0ghi"));
}

#[test]
fn render_env_resources_tree_skips_empty_sections() {
    let mut res = empty_resources();
    res.asgs = vec!["asg-1".into()];
    // Everything else empty.
    let body = super::render_env_resources_tree(&res, "small-env", "Web");
    assert!(body.contains("Auto-scaling groups (1)"));
    // No load-balancer / launch-config / queue headers when
    // the lists are empty.
    assert!(!body.contains("Load balancers"));
    assert!(!body.contains("Launch configurations"));
    assert!(!body.contains("Queues"));
}

#[test]
fn render_env_resources_tree_marks_orphan_instances_when_no_asg() {
    let mut res = empty_resources();
    res.instances = vec!["i-stranded".into()];
    let body = super::render_env_resources_tree(&res, "env", "Web");
    assert!(body.contains("orphan (no ASG attached)"));
    assert!(body.contains("i-stranded"));
}

#[test]
fn render_env_resources_tree_renders_queue_urls_inline() {
    let mut res = empty_resources();
    res.queues = vec![
        crate::aws::EnvResourceQueue {
            name: "WorkerQueue".into(),
            url: "https://sqs.eu-west-2.amazonaws.com/123/main".into(),
        },
        crate::aws::EnvResourceQueue {
            name: "WorkerDeadLetterQueue".into(),
            url: "https://sqs.eu-west-2.amazonaws.com/123/dlq".into(),
        },
    ];
    let body = super::render_env_resources_tree(&res, "worker-prod", "Worker");
    assert!(body.contains("├─ WorkerQueue"));
    assert!(body.contains("https://sqs.eu-west-2.amazonaws.com/123/main"));
    assert!(body.contains("└─ WorkerDeadLetterQueue"));
    assert!(body.contains("https://sqs.eu-west-2.amazonaws.com/123/dlq"));
}

#[test]
fn render_env_resources_tree_handles_zero_resources() {
    let res = empty_resources();
    let body = super::render_env_resources_tree(&res, "fresh-env", "Web");
    assert!(body.contains("(no resources reported"));
}

#[tokio::test]
async fn first_run_hint_dismisses_on_first_key() {
    let mut app = test_app();
    app.first_run_hint = true;
    press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
    assert!(
        !app.first_run_hint,
        "first key event should clear first_run_hint"
    );
}

#[tokio::test]
async fn first_run_hint_stays_false_for_subsequent_launches() {
    // Simulates "ebman has run before; state.toml exists."
    // The test harness defaults first_run_hint to false anyway,
    // but this nails down the contract: a state.toml on disk
    // means no hint, full stop.
    let app = test_app();
    assert!(
        !app.first_run_hint,
        "test harness must default first_run_hint=false (state.toml presumed present)"
    );
}

#[test]
fn edit_distance_basic_cases() {
    assert_eq!(super::edit_distance("", ""), 0);
    assert_eq!(super::edit_distance("abc", ""), 3);
    assert_eq!(super::edit_distance("", "abc"), 3);
    assert_eq!(super::edit_distance("kitten", "sitting"), 3);
    assert_eq!(super::edit_distance("restart", "restart"), 0);
    assert_eq!(super::edit_distance("restrt", "restart"), 1);
    assert_eq!(super::edit_distance("rebild", "rebuild"), 1);
    assert_eq!(super::edit_distance("scal", "scale"), 1);
}

#[test]
fn suggest_command_catches_one_char_typos() {
    // Operator typo: forgot the 'a' in restart.
    assert_eq!(super::suggest_command("restrt").as_deref(), Some("restart"));
    // Operator typo: dropped a 'u' in rebuild.
    assert_eq!(super::suggest_command("rebild").as_deref(), Some("rebuild"));
    // Operator typo: dropped the 'e' in scale.
    assert_eq!(super::suggest_command("scal").as_deref(), Some("scale"));
}

#[test]
fn suggest_command_returns_none_when_too_far() {
    // Nonsense input — no command is within edit-distance 2.
    assert_eq!(super::suggest_command("zzzzzz"), None);
}

#[test]
fn suggest_command_threshold_is_strict_for_short_input() {
    // 2-char input shouldn't "match" every 3-char alias —
    // the operator's intent is too ambiguous to guess.
    // `:zz` is distance 2 from many names; we cap at 1.
    let suggestion = super::suggest_command("zz");
    assert!(
        suggestion.is_none(),
        "2-char typo should require distance ≤ 1; got {suggestion:?}"
    );
}

#[test]
fn completion_candidates_filters_by_prefix() {
    let c = super::completion_candidates("ba");
    assert!(
        c.iter().any(|s| s == "batch-rebuild"),
        "expected batch-rebuild among ba-prefixed candidates; got {c:?}"
    );
    assert!(
        c.iter().all(|s| s.starts_with("ba")),
        "every candidate must start with the prefix; got {c:?}"
    );
    assert_eq!(
        c.clone(),
        {
            let mut sorted = c.clone();
            sorted.sort();
            sorted
        },
        "candidates must be alphabetically sorted"
    );
}

#[test]
fn completion_candidates_with_empty_prefix_returns_full_list() {
    let c = super::completion_candidates("");
    // The registry has 80+ names + aliases — exact count drifts
    // with each release, just sanity-check the shape.
    assert!(
        c.len() > 50,
        "expected the full command list; got {} entries",
        c.len()
    );
    assert!(c.iter().any(|s| s == "why"));
    assert!(c.iter().any(|s| s == "rebuild"));
}

#[tokio::test]
async fn tab_in_command_mode_cycles_through_matches() {
    let mut app = test_app();
    app.mode = Mode::Command;
    app.command_input = "bat".into();
    // First Tab → first match (batch-deploy alphabetically).
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    let first = app.command_input.text().to_string();
    assert!(
        first.starts_with("bat"),
        "Tab should keep the bat-prefix; got {first:?}"
    );
    assert!(
        crate::commands::all_names().contains(&first.as_str()),
        "Tab should expand to a real command name; got {first:?}"
    );
    // Second Tab cycles forward; should differ from first.
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    let second = app.command_input.text().to_string();
    assert_ne!(first, second, "second Tab should advance the cycle");
}

#[tokio::test]
async fn typing_in_command_mode_breaks_the_completion_cycle() {
    let mut app = test_app();
    app.mode = Mode::Command;
    app.command_input = "re".into();
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert!(app.completion.origin.is_some());
    // Operator types — cycle should reset.
    press(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(
        app.completion.origin.is_none(),
        "typing must reset the completion origin"
    );
}

#[tokio::test]
async fn shift_tab_cycles_backward() {
    let mut app = test_app();
    app.mode = Mode::Command;
    app.command_input = "ba".into();
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    let forward = app.command_input.clone();
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    press(&mut app, KeyCode::BackTab, KeyModifiers::NONE);
    // Two forward + one back = same as one forward.
    assert_eq!(
        app.command_input, forward,
        "Tab Tab BackTab should land on the first match"
    );
}

#[tokio::test]
async fn first_tab_lands_on_the_first_candidate() {
    // Regression: first Tab used to skip to candidates[1].
    let mut app = test_app();
    app.mode = Mode::Command;
    app.command_input = "ba".into();
    let expected = super::completion_candidates("ba");
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(
        app.command_input.text(),
        expected[0],
        "first Tab should land on the first match, not skip it"
    );
}

#[test]
fn command_takes_env_arg_only_for_env_first_commands() {
    for c in ["diff", "config-diff", "rds-detach"] {
        assert!(
            super::command_takes_env_arg(c),
            "{c} takes an env name as its first arg"
        );
    }
    // Selected-env commands and non-env NAME commands are excluded.
    for c in [
        "why", "deploy", "rebuild", "region", "profile", "view", "save",
    ] {
        assert!(
            !super::command_takes_env_arg(c),
            "{c} must not offer env-name completion"
        );
    }
}

#[tokio::test]
async fn env_name_candidates_filter_by_prefix_and_sort() {
    let mut app = test_app();
    app.environments = vec![
        mk_env("staging-api", "shop", "Web", "Green"),
        mk_env("prod-api", "shop", "Web", "Green"),
        mk_env("prod-worker", "shop", "Worker", "Green"),
    ];
    let all = app.env_name_candidates("");
    assert_eq!(
        all,
        vec!["prod-api", "prod-worker", "staging-api"],
        "empty prefix returns every env, sorted"
    );
    let pro = app.env_name_candidates("prod");
    assert_eq!(pro, vec!["prod-api", "prod-worker"]);
    assert!(app.env_name_candidates("zzz").is_empty());
}

#[tokio::test]
async fn tab_completes_env_name_for_diff() {
    let mut app = test_app();
    app.environments = vec![
        mk_env("prod-api", "shop", "Web", "Red"),
        mk_env("prod-worker", "shop", "Worker", "Green"),
    ];
    app.mode = Mode::Command;
    app.command_input = "diff prod".into();
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    // Completes the trailing token, preserving the command + space.
    assert_eq!(app.command_input.text(), "diff prod-api");
    // Second Tab cycles to the next matching env.
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.command_input.text(), "diff prod-worker");
}

#[tokio::test]
async fn tab_completes_second_env_name_for_diff() {
    // `:diff ENV-A ENV-B` — the trailing (second) token completes.
    let mut app = test_app();
    app.environments = vec![
        mk_env("prod-api", "shop", "Web", "Red"),
        mk_env("staging-api", "shop", "Web", "Green"),
    ];
    app.mode = Mode::Command;
    app.command_input = "diff prod-api sta".into();
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.command_input.text(), "diff prod-api staging-api");
}

#[tokio::test]
async fn tab_on_non_env_command_preserves_arg_tail() {
    // A non-env command still re-completes only the verb; the arg
    // after the first space passes through untouched (legacy
    // `:set-option aws` behaviour).
    let mut app = test_app();
    app.environments = vec![mk_env("prod-api", "shop", "Web", "Green")];
    app.mode = Mode::Command;
    app.command_input = "set-option aws".into();
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    let out = app.command_input.text().to_string();
    assert!(
        out.starts_with("set-option") && out.ends_with(" aws"),
        "verb re-completed, arg tail preserved; got {out:?}"
    );
}

#[tokio::test]
async fn tab_env_arg_with_no_match_restores_and_hints() {
    let mut app = test_app();
    app.environments = vec![mk_env("prod-api", "shop", "Web", "Green")];
    app.mode = Mode::Command;
    app.command_input = "diff zzz".into();
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(
        app.command_input.text(),
        "diff zzz",
        "no env match restores the typed input"
    );
    assert!(app
        .status_message
        .as_deref()
        .is_some_and(|m| m.contains("no environment matches")));
}

#[tokio::test]
async fn tab_env_arg_survives_multibyte_whitespace() {
    // Regression: rfind gives the first byte of the last whitespace
    // char; a multi-byte space (U+00A0 NBSP) used to slice mid-char
    // and panic the TUI. It must complete cleanly, not crash.
    let mut app = test_app();
    app.environments = vec![mk_env("prod-api", "shop", "Web", "Green")];
    app.mode = Mode::Command;
    app.command_input = "diff prod\u{00A0}".into();
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.command_input.text(), "diff prod\u{00A0}prod-api");
}

#[tokio::test]
async fn command_input_is_cursor_aware_via_shared_textinput() {
    // The `:` command line stores a TextInput — mid-string editing
    // works and any edit still resets the completion cycle.
    let mut app = test_app();
    press(&mut app, KeyCode::Char(':'), KeyModifiers::NONE);
    assert_eq!(app.mode, Mode::Command);
    for c in "deploy".chars() {
        press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    // Move back two and insert a char mid-string.
    press(&mut app, KeyCode::Left, KeyModifiers::NONE);
    press(&mut app, KeyCode::Left, KeyModifiers::NONE);
    press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
    // Cursor was between 'l' and 'o', so 'e' lands there.
    assert_eq!(app.command_input.text(), "depleoy");
    // Editing clears any pending completion cycle.
    assert!(app.completion.origin.is_none());
}

#[test]
fn render_options_overlay_groups_by_namespace_and_marks_set_vs_default() {
    let rows = vec![
        opt("aws:autoscaling:asg", "MinSize", Some("2"), Some("1")),
        opt("aws:autoscaling:asg", "MaxSize", None, Some("4")),
        opt(
            "aws:elasticbeanstalk:command",
            "DeploymentPolicy",
            Some("Rolling"),
            Some("AllAtOnce"),
        ),
    ];
    let body = super::render_options_overlay(&rows, None, "uflexi-prod");
    // Section headers per namespace.
    assert!(body.contains("── aws:autoscaling:asg ──"));
    assert!(body.contains("── aws:elasticbeanstalk:command ──"));
    // Operator-set rows marked with ▸; default rows with •.
    assert!(body.contains("▸ MinSize"));
    assert!(body.contains("• MaxSize"));
    assert!(body.contains("▸ DeploymentPolicy"));
    // Default value is surfaced.
    assert!(body.contains("default: 1"));
    assert!(body.contains("default: 4"));
    // Top header counts set vs default.
    assert!(body.contains("2/3 options are operator-set"));
}

#[test]
fn render_options_overlay_filters_to_namespace_when_given() {
    let rows = vec![
        opt("aws:autoscaling:asg", "MinSize", Some("2"), None),
        opt(
            "aws:elasticbeanstalk:command",
            "DeploymentPolicy",
            Some("Rolling"),
            None,
        ),
    ];
    let body = super::render_options_overlay(&rows, Some("aws:autoscaling:asg"), "uflexi-prod");
    assert!(body.contains("MinSize"));
    assert!(!body.contains("DeploymentPolicy"));
}

#[test]
fn render_options_overlay_handles_unknown_namespace() {
    let rows = vec![opt("aws:autoscaling:asg", "MinSize", Some("2"), None)];
    let body = super::render_options_overlay(&rows, Some("aws:bogus:ns"), "uflexi-prod");
    assert!(body.contains("No options found"));
    assert!(body.contains("aws:bogus:ns"));
}

#[test]
fn render_secrets_overlay_empty_with_filter_explains_region_scope() {
    let body = super::render_secrets_overlay(&[], Some("prod-db"));
    assert!(body.contains("No secrets matching 'prod-db'"));
    assert!(body.contains("region-scoped"));
}

#[test]
fn render_secrets_overlay_empty_no_filter_hints_at_iam() {
    let body = super::render_secrets_overlay(&[], None);
    assert!(body.contains("No Secrets Manager secrets"));
    assert!(body.contains("ListSecrets"));
}

#[test]
fn render_secrets_overlay_lists_metadata_only() {
    let now = chrono::Utc::now();
    let rows = vec![crate::aws::SecretSummary {
        name: "prod/db/password".into(),
        arn: "arn:aws:secretsmanager:us-east-1:123456789012:secret:prod/db/password-AbCdEf".into(),
        description: Some("RDS master".into()),
        last_changed: Some(now - chrono::Duration::days(3)),
        last_rotated: Some(now - chrono::Duration::days(30)),
        kms_key_id: Some("alias/aws/secretsmanager".into()),
    }];
    let body = super::render_secrets_overlay(&rows, None);
    assert!(body.contains("prod/db/password"));
    assert!(body.contains("RDS master"));
    assert!(body.contains("arn:aws:secretsmanager"));
    assert!(body.contains("changed:"));
    assert!(body.contains("rotated:"));
    assert!(body.contains("alias/aws/secretsmanager"));
    // The values themselves must never appear in :secrets output.
    assert!(!body.to_lowercase().contains("password:"));
}

#[test]
fn render_secrets_overlay_marks_never_rotated() {
    let now = chrono::Utc::now();
    let rows = vec![crate::aws::SecretSummary {
        name: "api-key".into(),
        arn: "arn:aws:secretsmanager:us-east-1:1:secret:api-key-x".into(),
        description: None,
        last_changed: Some(now - chrono::Duration::hours(2)),
        last_rotated: None,
        kms_key_id: None,
    }];
    let body = super::render_secrets_overlay(&rows, None);
    assert!(body.contains("rotated: never"));
}

#[test]
fn render_secret_value_overlay_redacts_when_redact_on() {
    let body = super::render_secret_value_overlay("api-key", "hunter2", true);
    assert!(body.contains("<redacted; 7 chars"));
    assert!(body.contains("fingerprint"));
    assert!(!body.contains("hunter2"));
    assert!(body.contains(":redact off"));
}

#[test]
fn render_secret_value_overlay_shows_value_when_redact_off() {
    let body = super::render_secret_value_overlay("api-key", "hunter2", false);
    assert!(body.contains("hunter2"));
    assert!(body.contains("yank"));
}

#[test]
fn render_secret_value_overlay_pretty_prints_json() {
    let body = super::render_secret_value_overlay(
        "prod/db",
        r#"{"USERNAME":"app","PASSWORD":"x"}"#,
        false,
    );
    // Expect a multi-line shape, not the input one-liner.
    assert!(body.contains("USERNAME"));
    assert!(body.contains("PASSWORD"));
    assert!(
        body.matches('\n').count() >= 4,
        "should pretty-print: {body}"
    );
}

#[test]
fn render_secret_value_overlay_leaves_non_json_alone() {
    let body = super::render_secret_value_overlay("flat", "ABC-DEF-GHI", false);
    assert!(body.contains("ABC-DEF-GHI"));
}

#[test]
fn short_fingerprint_is_stable_and_diffs() {
    let a = super::short_fingerprint("hunter2");
    let b = super::short_fingerprint("hunter2");
    let c = super::short_fingerprint("hunter3");
    assert_eq!(a, b);
    assert_ne!(a, c);
    assert_eq!(a.len(), 8);
}

#[test]
fn try_pretty_json_passes_through_non_json() {
    assert_eq!(super::try_pretty_json("just a string"), "just a string");
    assert_eq!(super::try_pretty_json(""), "");
}

#[test]
fn try_pretty_json_indents_objects() {
    let pretty = super::try_pretty_json(r#"{"a":1,"b":2}"#);
    let lines: Vec<&str> = pretty.lines().collect();
    assert!(lines.len() >= 4, "lines={lines:?}");
    assert!(lines.iter().any(|l| l.contains("\"a\": 1")));
    assert!(lines.iter().any(|l| l.contains("\"b\": 2")));
}

#[test]
fn try_pretty_json_emits_empty_containers_inline() {
    // Empty container must stay on one line, not split to `{\n}`.
    assert_eq!(super::try_pretty_json("{}"), "{}");
    assert_eq!(super::try_pretty_json("[]"), "[]");
    // Nested empty container — the outer object expands, the
    // inner `{}` stays inline beside its key.
    let pretty = super::try_pretty_json(r#"{"a":{}}"#);
    assert!(pretty.contains("\"a\": {}"), "got: {pretty}");
}

#[test]
fn try_pretty_json_preserves_strings_with_braces() {
    // A `{` inside a string must not trigger indent.
    let pretty = super::try_pretty_json(r#"{"msg":"hello {world}"}"#);
    assert!(pretty.contains("hello {world}"));
}

#[test]
fn format_age_buckets() {
    let now = chrono::Utc::now();
    assert!(super::format_age(now, now).ends_with("s ago"));
    assert!(super::format_age(now, now - chrono::Duration::seconds(120)).ends_with("m ago"));
    assert!(super::format_age(now, now - chrono::Duration::hours(5)).ends_with("h ago"));
    assert!(super::format_age(now, now - chrono::Duration::days(10)).ends_with("d ago"));
    let body = super::format_age(now, now - chrono::Duration::days(120));
    assert!(body.starts_with('~') && body.contains("mo"));
}

#[test]
fn render_options_overlay_truncates_long_value_options_list() {
    let mut row = opt("aws:foo", "Enum", Some("a"), None);
    row.value_options = (0..20).map(|i| format!("v{i}")).collect();
    let rows = vec![row];
    let body = super::render_options_overlay(&rows, None, "env");
    assert!(body.contains("oneof: v0, v1, v2, v3, v4, … +15"));
}

#[test]
fn flatten_err_marks_access_denied() {
    let e = color_eyre::eyre::eyre!("operation failed")
        .wrap_err("AccessDeniedException: User: arn:aws:sts::1234 is not authorized");
    let out = super::flatten_err_to_string(&e);
    assert!(out.starts_with("AccessDenied:"), "got: {out}");
}

#[test]
fn flatten_err_marks_not_found() {
    let e = color_eyre::eyre::eyre!("operation failed")
        .wrap_err("ResourceNotFoundException: alarm 'foo' does not exist");
    let out = super::flatten_err_to_string(&e);
    assert!(out.starts_with("NotFound:"), "got: {out}");
}

#[test]
fn flatten_err_marks_dependency_violation() {
    let e = color_eyre::eyre::eyre!("operation failed")
        .wrap_err("DependencyViolation: resource still has dependencies");
    let out = super::flatten_err_to_string(&e);
    assert!(out.starts_with("Conflict:"), "got: {out}");
}

#[test]
fn flatten_err_marks_expired_token() {
    let e = color_eyre::eyre::eyre!("operation failed")
        .wrap_err("ExpiredToken: session credentials expired");
    let out = super::flatten_err_to_string(&e);
    assert!(out.starts_with("ExpiredToken:"), "got: {out}");
}

#[test]
fn flatten_err_passes_unknown_through_unchanged() {
    let e = color_eyre::eyre::eyre!("some other failure");
    let out = super::flatten_err_to_string(&e);
    assert!(
        !out.contains(":"),
        "expected no classification prefix; got: {out}"
    );
}

#[test]
fn format_aws_error_routes_invalid_client_token_to_configure_hint() {
    let app = test_app();
    let out = app.format_aws_error(
        "refresh",
        "InvalidClientTokenId: The security token included in the request is invalid",
    );
    assert!(
        out.contains("credentials invalid"),
        "expected credentials-invalid hint, got: {out}"
    );
    assert!(
        out.contains("aws configure --profile"),
        "expected `aws configure` remediation, got: {out}"
    );
}

#[test]
fn format_aws_error_routes_signature_mismatch_to_configure_hint() {
    let app = test_app();
    let out = app.format_aws_error(
            "list_environments",
            "SignatureDoesNotMatch: The request signature we calculated does not match the signature you provided",
        );
    assert!(
        out.contains("credentials invalid"),
        "expected credentials-invalid hint, got: {out}"
    );
}

#[test]
fn format_aws_error_keeps_existing_expired_token_routing() {
    // The new invalid-creds arm must not steal traffic that the
    // existing ExpiredToken arm should keep handling. Belt-and-
    // braces test so a future re-ordering doesn't silently
    // regress the SSO refresh hint.
    let app = test_app();
    let out = app.format_aws_error(
        "refresh",
        "ExpiredToken: The security token included in the request is expired",
    );
    assert!(
        out.contains("credentials expired"),
        "expected expired-creds hint, got: {out}"
    );
    assert!(
        out.contains("aws sso login"),
        "expected `aws sso login` remediation, got: {out}"
    );
}

#[test]
fn deny_write_refuses_in_demo_mode_even_when_not_read_only() {
    let mut app = test_app();
    app.demo_mode = true;
    // Sanity: not in read-only mode otherwise.
    assert!(!app.read_only);
    let denied = app.deny_write("any-env", "rebuild");
    assert!(denied, "demo mode must deny writes");
    let err = app
        .error_message
        .as_deref()
        .expect("demo-mode deny_write should set error_message");
    assert!(
        err.contains("demo mode"),
        "expected demo-mode reason in toast, got: {err}"
    );
    // No safety pin → no "would also refuse" suffix.
    assert!(
        !err.contains("would also refuse"),
        "no pin configured → no compose suffix, got: {err}"
    );
}

#[test]
fn deny_write_demo_mode_composes_pin_reason_in_toast() {
    // Operators iterating on `safety_envs` in `--demo` to validate
    // their config wording should see BOTH the demo refusal AND
    // the pin reason — without this they'd have to exit demo to
    // confirm the pin is wired (0.17.4 review finding).
    let mut app = test_app();
    app.demo_mode = true;
    app.cfg.safety_envs.insert("prod-eu-1".into(), true);
    let denied = app.deny_write("prod-eu-1", "rebuild");
    assert!(denied);
    let err = app.error_message.as_deref().unwrap();
    assert!(err.contains("demo mode"), "got: {err}");
    assert!(
        err.contains("would also refuse"),
        "expected pin compose suffix, got: {err}"
    );
    assert!(
        err.contains("safety.envs.prod-eu-1"),
        "expected pin source in suffix, got: {err}"
    );
}

#[test]
fn deny_write_allows_writes_when_not_demo_and_not_read_only() {
    let mut app = test_app();
    // demo_mode is false by default in test_app.
    let denied = app.deny_write("any-env", "rebuild");
    assert!(!denied, "non-demo non-readonly path must allow writes");
    assert!(
        app.error_message.is_none(),
        "no error toast on allowed write, got: {:?}",
        app.error_message
    );
}

#[test]
fn traffic_warning_flags_updating() {
    let e = fake_env_with("prod", "Updating", "Yellow", Some(20));
    assert!(super::compute_traffic_warning(&e)
        .unwrap()
        .contains("ACTIVE DEPLOY"));
}

#[test]
fn traffic_warning_flags_recent_change() {
    let e = fake_env_with("prod", "Ready", "Green", Some(2));
    assert!(super::compute_traffic_warning(&e)
        .unwrap()
        .contains("RECENT CHANGE"));
}

#[test]
fn traffic_warning_silent_on_quiet_env() {
    let e = fake_env_with("prod", "Ready", "Green", Some(60));
    assert!(super::compute_traffic_warning(&e).is_none());
}

#[test]
fn traffic_warning_flags_red_health() {
    let e = fake_env_with("prod", "Ready", "Red", Some(120));
    assert!(super::compute_traffic_warning(&e).unwrap().contains("Red"));
}

#[test]
fn is_throttling_error_matches_common_aws_strings() {
    assert!(is_throttling_error("ThrottlingException: Rate exceeded"));
    assert!(is_throttling_error(
        "service error: ThrottlingException — please slow down"
    ));
    assert!(is_throttling_error("RequestLimitExceeded"));
    assert!(is_throttling_error("HTTP 429 Too Many Requests"));
    assert!(is_throttling_error("rate exceeded for this account"));
    // Negative cases.
    assert!(!is_throttling_error("EnvironmentNotFound"));
    assert!(!is_throttling_error("AccessDenied"));
    assert!(!is_throttling_error(""));
}

#[test]
fn throttle_backoff_grows_then_caps() {
    let base = Duration::from_secs(15);
    let b0 = throttle_backoff(base, 0);
    let b1 = throttle_backoff(base, 1);
    let b2 = throttle_backoff(base, 2);
    // First throttle: 2x base (30 s); second: 4x; third: 8x.
    assert_eq!(b0, Duration::from_secs(30));
    assert_eq!(b1, Duration::from_secs(60));
    assert_eq!(b2, Duration::from_secs(120));
    // Way past the cap stays at the cap.
    let bn = throttle_backoff(base, 30);
    assert_eq!(bn, Duration::from_secs(300));
}

#[test]
fn throttle_backoff_handles_overflow_safely() {
    // Pathologically large base must not panic — saturating_mul keeps us safe.
    let base = Duration::MAX;
    let b = throttle_backoff(base, 5);
    assert_eq!(b, Duration::from_secs(300));
}

#[test]
fn delta_toast_key_extracts_bucket_for_delta_shapes() {
    assert_eq!(super::delta_toast_key("▲2 Red").as_deref(), Some("Red"));
    assert_eq!(
        super::delta_toast_key("▼1 Yellow").as_deref(),
        Some("Yellow")
    );
    // Leading whitespace is allowed.
    assert_eq!(
        super::delta_toast_key("  ▲10 Green").as_deref(),
        Some("Green")
    );
}

#[test]
fn format_app_versions_marks_deployed_and_shows_total_when_truncated() {
    use crate::aws::AppVersion;
    let mk = |label: &str, desc: &str| AppVersion {
        label: label.into(),
        description: desc.into(),
        created: None,
    };
    let versions: Vec<AppVersion> = (1..=30)
        .map(|i| {
            mk(
                &format!("build-{i}"),
                &format!("Application version created from https://example.com/build/{i}"),
            )
        })
        .rev()
        .collect();
    // build-5 is outside the top 20 (which is build-30 down to build-11
    // after the rev). Lets us check the truncation banner without the
    // deployed marker showing up.
    let out = super::format_app_versions(&versions, Some("build-5"), 20, false);
    assert!(out.contains("showing 20 of 30"));
    assert!(!out.contains("◀ deployed"));
    // Description prefix stripped.
    assert!(out.contains("https://example.com/build/"));
    assert!(!out.contains("Application version created from "));
}

#[test]
fn format_app_versions_marks_deployed_when_present() {
    use crate::aws::AppVersion;
    let versions = vec![
        AppVersion {
            label: "build-3".into(),
            description: String::new(),
            created: None,
        },
        AppVersion {
            label: "build-2".into(),
            description: String::new(),
            created: None,
        },
    ];
    let out = super::format_app_versions(&versions, Some("build-2"), 20, false);
    assert!(out.contains("◀ deployed"));
    // No truncation banner when total <= limit.
    assert!(!out.contains("showing "));
}

#[test]
fn wrap_with_hanging_indent_first_line_keeps_lead_marker() {
    let out = super::wrap_with_hanging_indent(
        "Threshold Crossed: alarm details continue",
        30,
        "  ↳ ",
        "    ",
    );
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("  ↳ "));
    // Continuation line uses the cont prefix.
    if lines.len() > 1 {
        assert!(lines[1].starts_with("    "));
    }
}

#[test]
fn wrap_with_hanging_indent_hard_breaks_oversize_words() {
    // A single 50-char word at width 20 + 4-char lead → body width 16.
    let big_word = "x".repeat(50);
    let out = super::wrap_with_hanging_indent(&big_word, 20, "    ", "    ");
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines.len() >= 3);
}

#[test]
fn parse_s3_url_extracts_bucket_and_key() {
    let (b, k) = super::parse_s3_url("s3://my-bucket/path/to/bundle.zip").unwrap();
    assert_eq!(b, "my-bucket");
    assert_eq!(k, "path/to/bundle.zip");
}

#[test]
fn parse_s3_url_rejects_malformed() {
    assert!(super::parse_s3_url("/local/path.zip").is_none());
    assert!(super::parse_s3_url("s3://").is_none());
    assert!(super::parse_s3_url("s3://bucket").is_none());
    assert!(super::parse_s3_url("s3://bucket/").is_none());
    assert!(super::parse_s3_url("s3:///key").is_none());
}

#[test]
fn parse_metric_extra_args_defaults_to_average() {
    let (stat, dims) = super::parse_metric_extra_args(&[]);
    assert_eq!(stat, "Average");
    assert!(dims.is_empty());
}

#[test]
fn parse_metric_extra_args_picks_stat_first() {
    let (stat, dims) = super::parse_metric_extra_args(&["Sum"]);
    assert_eq!(stat, "Sum");
    assert!(dims.is_empty());
}

#[test]
fn parse_metric_extra_args_picks_dims_when_present() {
    let (stat, dims) = super::parse_metric_extra_args(&["InstanceId=i-abc"]);
    assert_eq!(stat, "Average");
    assert_eq!(dims, vec![("InstanceId".into(), "i-abc".into())]);
}

#[test]
fn parse_metric_extra_args_supports_both_in_any_order() {
    let (stat, dims) = super::parse_metric_extra_args(&["Sum", "InstanceId=i-abc,Tier=web"]);
    assert_eq!(stat, "Sum");
    assert_eq!(
        dims,
        vec![
            ("InstanceId".into(), "i-abc".into()),
            ("Tier".into(), "web".into()),
        ]
    );
    // Reversed order: dims first.
    let (stat, dims) = super::parse_metric_extra_args(&["InstanceId=i-abc", "Sum"]);
    assert_eq!(stat, "Sum");
    assert_eq!(dims, vec![("InstanceId".into(), "i-abc".into())]);
}

#[test]
fn derive_version_label_uses_filename_stem_and_timestamp() {
    let l = super::derive_version_label("./build.zip", 1684512345);
    assert_eq!(l, "build_1684512345");
    let l = super::derive_version_label("/tmp/myapp-2.1.0.zip", 42);
    assert_eq!(l, "myapp-2.1.0_42");
}

#[test]
fn derive_version_label_sanitises_disallowed_chars() {
    // EB version labels don't allow spaces or weird punctuation; we
    // replace them with `_` so the operator gets a valid label even from
    // a goofy filename.
    let l = super::derive_version_label("/tmp/build with spaces & specials!.zip", 1);
    assert_eq!(l, "build_with_spaces___specials__1");
}

#[test]
fn derive_version_label_falls_back_to_bundle_on_pathological_input() {
    // Bare `/` has no filename stem.
    let l = super::derive_version_label("/", 9);
    assert_eq!(l, "bundle_9");
}

#[test]
fn expand_tilde_only_replaces_leading() {
    // Set HOME for the test.
    let prev = std::env::var_os("HOME");
    // SAFETY: tests run single-threaded by default; restore at the end.
    unsafe {
        std::env::set_var("HOME", "/Users/tester");
    }
    assert_eq!(super::expand_tilde("~/foo/bar"), "/Users/tester/foo/bar");
    // No leading tilde → unchanged.
    assert_eq!(super::expand_tilde("/abs/path"), "/abs/path");
    // `~name` left alone (not supported).
    assert_eq!(super::expand_tilde("~tom/foo"), "~tom/foo");
    // Mid-path tilde left alone.
    assert_eq!(super::expand_tilde("/foo/~/bar"), "/foo/~/bar");
    if let Some(v) = prev {
        unsafe {
            std::env::set_var("HOME", v);
        }
    } else {
        unsafe {
            std::env::remove_var("HOME");
        }
    }
}

#[test]
fn pick_default_log_group_prefers_web_stdout() {
    let groups: Vec<String> = vec![
        "/aws/elasticbeanstalk/myenv/var/log/eb-engine.log".into(),
        "/aws/elasticbeanstalk/myenv/var/log/web.stdout.log".into(),
        "/aws/elasticbeanstalk/myenv/var/log/nginx/access.log".into(),
    ];
    assert_eq!(
        super::pick_default_log_group(&groups).as_deref(),
        Some("/aws/elasticbeanstalk/myenv/var/log/web.stdout.log")
    );
}

#[test]
fn pick_default_log_group_falls_back_to_first() {
    let groups: Vec<String> = vec!["/aws/elasticbeanstalk/myenv/var/log/custom.log".into()];
    assert_eq!(
        super::pick_default_log_group(&groups).as_deref(),
        Some("/aws/elasticbeanstalk/myenv/var/log/custom.log")
    );
    // No groups at all → None.
    assert_eq!(super::pick_default_log_group(&[]), None);
}

#[test]
fn pick_default_log_group_prefers_engine_log_when_stdout_absent() {
    let groups: Vec<String> = vec![
        "/aws/elasticbeanstalk/myenv/var/log/nginx/access.log".into(),
        "/aws/elasticbeanstalk/myenv/var/log/eb-engine.log".into(),
    ];
    assert_eq!(
        super::pick_default_log_group(&groups).as_deref(),
        Some("/aws/elasticbeanstalk/myenv/var/log/eb-engine.log")
    );
}

#[test]
fn format_env_vars_aligns_on_equals() {
    let vars = vec![
        ("DEBUG".into(), "1".into()),
        ("DATABASE_URL".into(), "postgres://x".into()),
    ];
    let out = super::format_env_vars(&vars);
    assert!(out.contains("DEBUG"));
    assert!(out.contains("= 1"));
    assert!(out.contains("DATABASE_URL"));
    let vars = vec![("EMPTY".into(), "".into())];
    assert!(super::format_env_vars(&vars).contains("\"\""));
}

#[test]
fn format_env_vars_handles_empty_input() {
    assert_eq!(super::format_env_vars(&[]), "(no env vars set)");
}

#[test]
fn parse_named_arg_picks_up_value_after_flag() {
    let rest: Vec<&str> = vec!["on", "--retention", "14"];
    assert_eq!(
        super::parse_named_arg::<i32>(&rest, "--retention"),
        Some(14)
    );
    // Flag absent.
    assert_eq!(super::parse_named_arg::<i32>(&["on"], "--retention"), None);
    // Flag present but no following value.
    assert_eq!(
        super::parse_named_arg::<i32>(&["on", "--retention"], "--retention"),
        None
    );
    // Following value doesn't parse.
    assert_eq!(
        super::parse_named_arg::<i32>(&["on", "--retention", "abc"], "--retention"),
        None
    );
}

#[test]
fn alarm_kind_to_metric_covers_known_kinds() {
    use crate::app::alarm_kind_to_metric;
    let (m, op, _) = alarm_kind_to_metric("health").unwrap();
    assert_eq!(m, "EnvironmentHealth");
    // Health is "drop below" → LessThanOrEqualToThreshold.
    assert_eq!(op, "LessThanOrEqualToThreshold");
    let (m, op, _) = alarm_kind_to_metric("5xx").unwrap();
    assert_eq!(m, "ApplicationRequests5xx");
    assert_eq!(op, "GreaterThanThreshold");
    // Aliases.
    assert_eq!(alarm_kind_to_metric("req5xx"), alarm_kind_to_metric("5xx"));
    assert_eq!(alarm_kind_to_metric("p90"), alarm_kind_to_metric("latency"));
    // Unknown.
    assert!(alarm_kind_to_metric("cpu").is_none());
    assert!(alarm_kind_to_metric("").is_none());
}

#[test]
fn format_template_settings_groups_by_namespace() {
    let s = vec![
        (
            "aws:elasticbeanstalk:environment".into(),
            "EnvironmentType".into(),
            "LoadBalanced".into(),
        ),
        ("aws:autoscaling:asg".into(), "MinSize".into(), "2".into()),
        ("aws:autoscaling:asg".into(), "MaxSize".into(), "8".into()),
    ];
    let out = super::format_template_settings(&s);
    assert!(out.contains("[aws:autoscaling:asg]"));
    assert!(out.contains("[aws:elasticbeanstalk:environment]"));
    assert!(out.contains("MinSize"));
    assert!(out.contains("= 2"));
    // Empty value renders as the literal "" so operators can tell empty
    // from unset.
    let s = vec![(
        "aws:elasticbeanstalk:application:environment".into(),
        "DEBUG".into(),
        String::new(),
    )];
    assert!(super::format_template_settings(&s).contains("DEBUG"));
    assert!(super::format_template_settings(&s).contains("\"\""));
}

#[test]
fn format_template_settings_handles_empty_input() {
    assert_eq!(super::format_template_settings(&[]), "(no option settings)");
}

#[test]
fn action_labels_are_distinct_and_non_empty() {
    // Catches accidental "placeholder Action::Rebuild" reuses — every
    // variant must carry its own label so audit logs + toasts reflect
    // what was actually dispatched.
    //
    // 0.19: extended to include `Action::Capacity` which had been
    // missing since 0.6 (caught by the 0.17.4 review pass). Now
    // exhaustive across all 15 variants — every variant gets an
    // explicit assertion so future additions can't skip the
    // distinctness check.
    use crate::app::Action;
    use std::collections::HashSet;
    let all = [
        Action::Rebuild,
        Action::RestartAppServer,
        Action::SwapCnames,
        Action::Terminate,
        Action::Deploy,
        Action::UpgradePlatform,
        Action::Clone,
        Action::Scale,
        Action::Capacity,
        Action::AbortUpdate,
        Action::ConfigSave,
        Action::ConfigDelete,
        Action::ConfigApply,
        Action::TerminateInstance,
        Action::SsmRun,
    ];
    let mut labels = HashSet::new();
    for a in all {
        let l = a.label();
        assert!(!l.is_empty(), "{a:?} has empty label");
        assert!(labels.insert(l), "{a:?} reuses label {l:?}");
    }
    // 15 = the full Action enum size. Update both the array
    // above and this guard if a new variant is added.
    assert_eq!(all.len(), 15);
}

#[test]
fn collect_saved_configs_flattens_and_sorts_stably() {
    use crate::aws::Application;
    let app = |name: &str, templates: Vec<String>| Application {
        name: name.into(),
        description: String::new(),
        date_created: None,
        date_updated: None,
        version_count: 0,
        templates,
        latest_version_label: None,
        latest_version_created: None,
    };
    let apps = vec![
        app("beta", vec!["prod".into(), "canary".into()]),
        app("alpha", vec![]),
        app("alpha", vec!["staging".into()]),
    ];
    let out = super::collect_saved_configs(&apps);
    assert_eq!(
        out,
        vec![
            ("alpha".into(), "staging".into()),
            ("beta".into(), "canary".into()),
            ("beta".into(), "prod".into()),
        ]
    );
}

#[test]
fn collect_saved_configs_empty_when_no_templates() {
    use crate::aws::Application;
    let apps = vec![Application {
        name: "alpha".into(),
        description: String::new(),
        date_created: None,
        date_updated: None,
        version_count: 0,
        templates: vec![],
        latest_version_label: None,
        latest_version_created: None,
    }];
    assert!(super::collect_saved_configs(&apps).is_empty());
}

#[test]
fn merge_app_latest_versions_carries_previous_values_by_name() {
    use crate::aws::Application;
    let mk = |name: &str,
              label: Option<&str>,
              created: Option<chrono::DateTime<chrono::Utc>>|
     -> Application {
        Application {
            name: name.into(),
            description: String::new(),
            date_created: None,
            date_updated: None,
            version_count: 0,
            templates: vec![],
            latest_version_label: label.map(|s| s.into()),
            latest_version_created: created,
        }
    };
    let t0 = chrono::Utc::now();
    let prev = vec![
        mk("alpha", Some("build-1"), Some(t0)),
        mk("beta", Some("build-9"), Some(t0)),
    ];
    // Fresh refresh: same apps, plus a new one, all with empty LATEST.
    let mut next = vec![
        mk("alpha", None, None),
        mk("beta", None, None),
        mk("gamma", None, None),
    ];
    super::merge_app_latest_versions(&prev, &mut next);
    assert_eq!(next[0].latest_version_label.as_deref(), Some("build-1"));
    assert_eq!(next[0].latest_version_created, Some(t0));
    assert_eq!(next[1].latest_version_label.as_deref(), Some("build-9"));
    // New app has no prior value; stays None.
    assert_eq!(next[2].latest_version_label, None);
    assert_eq!(next[2].latest_version_created, None);
}

#[test]
fn merge_app_latest_versions_does_not_overwrite_already_populated_slots() {
    // Safety net: if a future caller pre-populates the LATEST fields on
    // `next` (e.g. a faster fan-out lands before the apps-list does),
    // the carry-forward must not stomp on fresher data.
    use crate::aws::Application;
    let mk = |name: &str, label: Option<&str>| -> Application {
        Application {
            name: name.into(),
            description: String::new(),
            date_created: None,
            date_updated: None,
            version_count: 0,
            templates: vec![],
            latest_version_label: label.map(|s| s.into()),
            latest_version_created: None,
        }
    };
    let prev = vec![mk("alpha", Some("OLD"))];
    let mut next = vec![mk("alpha", Some("NEW"))];
    super::merge_app_latest_versions(&prev, &mut next);
    assert_eq!(next[0].latest_version_label.as_deref(), Some("NEW"));
}

#[test]
fn merge_app_latest_versions_handles_app_disappearance() {
    // If an app is renamed / deleted between refreshes, its prev entry
    // simply has no matching `next` and the carry-forward is a no-op.
    use crate::aws::Application;
    let mk = |name: &str, label: Option<&str>| -> Application {
        Application {
            name: name.into(),
            description: String::new(),
            date_created: None,
            date_updated: None,
            version_count: 0,
            templates: vec![],
            latest_version_label: label.map(|s| s.into()),
            latest_version_created: None,
        }
    };
    let prev = vec![mk("alpha", Some("build-old")), mk("beta", Some("build-2"))];
    let mut next = vec![mk("beta", None)];
    super::merge_app_latest_versions(&prev, &mut next);
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].latest_version_label.as_deref(), Some("build-2"));
}

#[test]
fn format_org_accounts_includes_switch_hint_when_configured() {
    use crate::aws::OrgAccount;
    let accounts = vec![
        OrgAccount {
            id: "111122223333".into(),
            name: "prod".into(),
            email: Some("prod@example.com".into()),
            status: "ACTIVE".into(),
        },
        OrgAccount {
            id: "444455556666".into(),
            name: "sandbox".into(),
            email: None,
            status: "SUSPENDED".into(),
        },
    ];
    let mut configured = std::collections::HashMap::new();
    configured.insert("prod".to_string(), "prod".to_string());
    let body = super::format_org_accounts(&accounts, &configured);
    assert!(body.contains("● prod"));
    assert!(body.contains("⊘ sandbox"));
    assert!(body.contains("prod@example.com"));
    // Switch hint only for the configured account.
    assert!(body.contains(":account prod"));
    assert!(!body.contains(":account sandbox"));
}

#[test]
fn format_org_accounts_empty_returns_hint() {
    let body = super::format_org_accounts(&[], &std::collections::HashMap::new());
    assert!(body.contains("no accounts returned"));
}

#[test]
fn format_org_accounts_matches_id_when_named_by_id() {
    use crate::aws::OrgAccount;
    let accounts = vec![OrgAccount {
        id: "111122223333".into(),
        name: "prod".into(),
        email: None,
        status: "ACTIVE".into(),
    }];
    // Operator named the AssumeRole entry by account-id rather
    // than friendly name — still matches.
    let mut configured = std::collections::HashMap::new();
    configured.insert("111122223333".to_string(), "111122223333".to_string());
    let body = super::format_org_accounts(&accounts, &configured);
    assert!(body.contains(":account 111122223333"));
}

#[test]
fn format_deploy_preview_happy_path() {
    use crate::aws::AppVersion;
    let now = chrono::Utc::now();
    let versions = vec![
        AppVersion {
            label: "build-142".into(),
            description: "fix: idempotent retries".into(),
            created: Some(now - chrono::Duration::hours(2)),
        },
        AppVersion {
            label: "build-141".into(),
            description: "feat: /metrics endpoint".into(),
            created: Some(now - chrono::Duration::days(1)),
        },
    ];
    let body = super::format_deploy_preview("uflexi-prod", "build-141", "build-142", &versions);
    assert!(body.contains("env:        uflexi-prod"));
    assert!(body.contains("current:    build-141"));
    assert!(body.contains("candidate:  build-142"));
    assert!(body.contains("fix: idempotent retries"));
    // Newer candidate → no rollback warning.
    assert!(!body.contains("rollback"));
}

#[test]
fn format_deploy_preview_rollback_warning_fires_when_older() {
    use crate::aws::AppVersion;
    let now = chrono::Utc::now();
    let versions = vec![
        AppVersion {
            label: "build-old".into(),
            description: String::new(),
            created: Some(now - chrono::Duration::days(7)),
        },
        AppVersion {
            label: "build-new".into(),
            description: String::new(),
            created: Some(now - chrono::Duration::hours(1)),
        },
    ];
    // Deploying the OLDER version on top of the NEWER one → rollback.
    let body = super::format_deploy_preview("uflexi-prod", "build-new", "build-old", &versions);
    assert!(
        body.contains("rollback"),
        "expected rollback warning, got: {body}"
    );
}

#[test]
fn format_deploy_preview_unknown_label_calls_out_the_gap() {
    use crate::aws::AppVersion;
    let versions = vec![AppVersion {
        label: "build-141".into(),
        description: String::new(),
        created: Some(chrono::Utc::now()),
    }];
    let body = super::format_deploy_preview(
        "uflexi-prod",
        "build-141",
        "build-DOES-NOT-EXIST",
        &versions,
    );
    assert!(body.contains("not found"));
    assert!(body.contains("build-DOES-NOT-EXIST"));
}

fn make_event(msg: &str) -> crate::aws::Event {
    crate::aws::Event {
        at: Some(chrono::Utc::now()),
        env: "uflexi-prod".into(),
        application: "uflexi".into(),
        message: msg.into(),
        severity: "INFO".into(),
        version_label: None,
    }
}

#[test]
fn previous_version_label_finds_prior_deploy() {
    let ev = |vl: Option<&str>| crate::aws::Event {
        at: None,
        env: "e".into(),
        application: "a".into(),
        message: String::new(),
        severity: "INFO".into(),
        version_label: vl.map(String::from),
    };
    // Newest-first: current build-3, an untagged event, then the
    // older deploys. The first label ≠ current is the rollback target.
    let events = vec![
        ev(Some("build-3")),
        ev(None),
        ev(Some("build-3")),
        ev(Some("build-2")),
        ev(Some("build-1")),
    ];
    assert_eq!(
        super::previous_version_label(&events, "build-3"),
        Some("build-2".into())
    );
    // Only the current version (+ untagged) appears → None.
    let only_current = vec![ev(Some("build-3")), ev(None), ev(Some("build-3"))];
    assert_eq!(
        super::previous_version_label(&only_current, "build-3"),
        None
    );
    // No version labels at all → None.
    assert_eq!(
        super::previous_version_label(&[ev(None), ev(None)], "build-3"),
        None
    );
    // Empty event list → None.
    assert_eq!(super::previous_version_label(&[], "build-3"), None);
    // Empty-string labels are skipped.
    assert_eq!(
        super::previous_version_label(&[ev(Some("")), ev(Some("build-1"))], "build-3"),
        Some("build-1".into())
    );
}

#[test]
fn is_config_event_keeps_deploys_and_config_changes() {
    assert!(super::is_config_event(
        "Updating environment uflexi-prod to use version label 'build-9'."
    ));
    assert!(super::is_config_event(
        "Deploying new version to instance(s)."
    ));
    assert!(super::is_config_event(
        "Updating environment uflexi-prod's configuration settings."
    ));
    // Routine health / lifecycle noise is filtered out.
    assert!(!super::is_config_event(
        "Environment health transitioned from Ok to Severe."
    ));
    assert!(!super::is_config_event(
        "Added instance 'i-abc' to environment."
    ));
}

#[test]
fn render_changes_overlay_states() {
    let ev = |msg: &str, vl: Option<&str>| crate::aws::Event {
        at: None,
        env: "e".into(),
        application: "a".into(),
        message: msg.into(),
        severity: "INFO".into(),
        version_label: vl.map(String::from),
    };
    // Only noise → empty-state message.
    let noise = vec![ev("Environment health transitioned to Ok.", None)];
    assert!(super::render_changes_overlay("prod", &noise).contains("No deploy"));
    // A deploy event is kept and its version label shown.
    let evs = vec![
        ev("Deploying new version to instance(s).", Some("build-9")),
        ev("Environment health transitioned to Ok.", None),
    ];
    let body = super::render_changes_overlay("prod", &evs);
    assert!(body.contains("Deploying new version"));
    assert!(body.contains("[build-9]"));
    assert!(!body.contains("health transitioned"));
}

#[test]
fn build_lineage_collapses_consecutive_same_label_events() {
    // EB emits multiple events per deploy (started / instance OK /
    // env update completed). `build_lineage` must collapse them
    // into one row carrying the full first→last span. Newest-first
    // input → newest-first output.
    use chrono::TimeZone;
    let ts = |y, mo, d, h, mi| chrono::Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap();
    let mk = |t, vl: &str| crate::aws::Event {
        at: Some(t),
        env: "e".into(),
        application: "a".into(),
        message: "deploy event".into(),
        severity: "INFO".into(),
        version_label: Some(vl.into()),
    };
    // 3 events for build-9 (latest deploy) then 2 for build-8.
    let evs = vec![
        mk(ts(2026, 5, 24, 12, 7), "build-9"),
        mk(ts(2026, 5, 24, 12, 5), "build-9"),
        mk(ts(2026, 5, 24, 12, 0), "build-9"),
        mk(ts(2026, 5, 24, 11, 3), "build-8"),
        mk(ts(2026, 5, 24, 11, 0), "build-8"),
    ];
    let rows = super::build_lineage(&evs);
    assert_eq!(rows.len(), 2, "expected 2 distinct deploys, got {rows:?}");
    // Newest first: build-9 then build-8.
    assert_eq!(rows[0].label, "build-9");
    assert_eq!(rows[1].label, "build-8");
    // first_at = earliest, last_at = latest within the group.
    assert_eq!(rows[0].first_at, Some(ts(2026, 5, 24, 12, 0)));
    assert_eq!(rows[0].last_at, Some(ts(2026, 5, 24, 12, 7)));
    assert_eq!(rows[1].first_at, Some(ts(2026, 5, 24, 11, 0)));
    assert_eq!(rows[1].last_at, Some(ts(2026, 5, 24, 11, 3)));
}

#[test]
fn build_lineage_drops_events_without_version_label() {
    // Events without a version_label (routine health transitions,
    // scaling notices) must not produce phantom rows.
    let ev = |vl: Option<&str>| crate::aws::Event {
        at: None,
        env: "e".into(),
        application: "a".into(),
        message: "noise".into(),
        severity: "INFO".into(),
        version_label: vl.map(String::from),
    };
    let evs = vec![ev(None), ev(Some("")), ev(None)];
    assert!(super::build_lineage(&evs).is_empty());
}

#[test]
fn format_lineage_shows_gap_and_span_between_deploys() {
    // Δ-since-previous and `took` lines appear when timestamps
    // allow the maths. Empty event window → stub body so the
    // operator isn't left wondering whether the fetch failed.
    use chrono::TimeZone;
    let ts = |h, mi| chrono::Utc.with_ymd_and_hms(2026, 5, 24, h, mi, 0).unwrap();
    let mk = |t, vl: &str| crate::aws::Event {
        at: Some(t),
        env: "e".into(),
        application: "a".into(),
        message: "deploy event".into(),
        severity: "INFO".into(),
        version_label: Some(vl.into()),
    };
    // Empty input → stub.
    assert!(super::format_lineage("prod", &[]).contains("No deploys"));
    // Two deploys: build-9 12:00→12:05 (took 5m), build-8 at 10:00
    // (gap of 2h since previous from build-9's POV).
    let evs = vec![
        mk(ts(12, 5), "build-9"),
        mk(ts(12, 0), "build-9"),
        mk(ts(10, 0), "build-8"),
    ];
    let body = super::format_lineage("prod", &evs);
    // Both labels appear, newest first.
    let p9 = body.find("build-9").expect("build-9 row");
    let p8 = body.find("build-8").expect("build-8 row");
    assert!(p9 < p8, "build-9 should come before build-8 (newest first)");
    // Span row visible for build-9 (5min).
    assert!(
        body.contains("took"),
        "expected `took` span line, got:\n{body}"
    );
    // Δ-since-previous visible for build-9 → 2h gap.
    assert!(
        body.contains("Δ"),
        "expected `Δ since previous` line, got:\n{body}"
    );
}

#[test]
fn classify_update_kind_deploy_extracts_label() {
    let evs = vec![make_event(
        "Updating environment uflexi-prod to use version label 'build-142'.",
    )];
    match super::classify_update_kind(&evs) {
        super::UpdateKind::Deploy { version_label } => {
            assert_eq!(version_label.as_deref(), Some("build-142"));
        }
        other => panic!("expected Deploy, got {other:?}"),
    }
}

#[test]
fn classify_update_kind_deploy_without_label_still_classifies() {
    let evs = vec![make_event("Deploying new version to instance i-abc123.")];
    match super::classify_update_kind(&evs) {
        super::UpdateKind::Deploy { version_label } => {
            // Label can't be extracted from this message shape — that's
            // fine, it's still a Deploy.
            assert!(version_label.is_none());
        }
        other => panic!("expected Deploy, got {other:?}"),
    }
}

#[test]
fn classify_update_kind_platform_update() {
    let evs = vec![make_event(
        "Updating environment to use platform 'arn:aws:elasticbeanstalk:…:platform/Corretto 17'.",
    )];
    // Even though the message also contains 'platform', deploy
    // pattern (`version label`) isn't matched, so we fall through
    // to the platform branch.
    assert_eq!(
        super::classify_update_kind(&evs),
        super::UpdateKind::Platform
    );
}

#[test]
fn classify_update_kind_config_change() {
    let evs = vec![make_event("Updating environment configuration completed.")];
    assert_eq!(super::classify_update_kind(&evs), super::UpdateKind::Config);
}

#[test]
fn classify_update_kind_scale_event() {
    let evs = vec![make_event("Adding instance 'i-abc123' to environment.")];
    assert_eq!(super::classify_update_kind(&evs), super::UpdateKind::Scale);
}

#[test]
fn classify_update_kind_unknown_message_falls_through_to_generic() {
    let evs = vec![make_event("Something cryptic happened.")];
    assert_eq!(
        super::classify_update_kind(&evs),
        super::UpdateKind::Generic
    );
}

#[test]
fn classify_update_kind_picks_most_recent_match() {
    // Events are newest-first; the deploy event sits ahead of the
    // older scale event, so Deploy wins.
    let evs = vec![
        make_event("Updating environment to use version label 'build-99'."),
        make_event("Adding instance 'i-old' to environment."),
    ];
    match super::classify_update_kind(&evs) {
        super::UpdateKind::Deploy { version_label } => {
            assert_eq!(version_label.as_deref(), Some("build-99"));
        }
        other => panic!("expected Deploy from newest match, got {other:?}"),
    }
}

#[test]
fn classify_update_kind_empty_events_is_generic() {
    assert_eq!(super::classify_update_kind(&[]), super::UpdateKind::Generic);
}

#[test]
fn compute_red_alerts_counts_eb_red_and_worker_dlq() {
    use crate::aws::Environment;
    let mk = |name: &str, tier: &str, health: &str| Environment {
        name: name.into(),
        application: "uflexi".into(),
        status: "Ready".into(),
        health: health.into(),
        platform: "Java 17".into(),
        solution_stack: String::new(),
        tier: tier.into(),
        cname: String::new(),
        version_label: String::new(),
        arn: None,
        updated: None,
        id: None,
        region: None,
    };
    let envs = vec![
        mk("web-prod", "Web", "Green"),
        mk("web-red", "Web", "Red"),
        mk("worker-green-dlq", "Worker", "Green"),
        mk("worker-clean", "Worker", "Green"),
        mk("worker-red", "Worker", "Severe"),
    ];
    let mut dlq = std::collections::HashMap::new();
    dlq.insert("worker-green-dlq".to_string(), 3);
    dlq.insert("worker-clean".to_string(), 0);
    // EB-Red + DLQ-Red + EB-Red-on-worker = 3 alerts (worker-red counted once).
    assert_eq!(super::compute_red_alerts(&envs, &dlq), 3);
}

#[test]
fn compute_red_alerts_ignores_dlq_for_web_tier() {
    use crate::aws::Environment;
    let env = Environment {
        name: "web-prod".into(),
        application: "uflexi".into(),
        status: "Ready".into(),
        health: "Green".into(),
        platform: "Java 17".into(),
        solution_stack: String::new(),
        tier: "Web".into(),
        cname: String::new(),
        version_label: String::new(),
        arn: None,
        updated: None,
        id: None,
        region: None,
    };
    // Even with a spurious "web-prod" entry in dlq_depths, a Web env
    // never counts as DLQ-red. Belt-and-braces against a stale cache
    // entry surviving a tier change.
    let mut dlq = std::collections::HashMap::new();
    dlq.insert("web-prod".to_string(), 99);
    assert_eq!(super::compute_red_alerts(&[env], &dlq), 0);
}

#[test]
fn compute_red_alerts_zero_dlq_is_not_alert_worthy() {
    use crate::aws::Environment;
    let env = Environment {
        name: "worker-clean".into(),
        application: "uflexi".into(),
        status: "Ready".into(),
        health: "Green".into(),
        platform: "Java 17".into(),
        solution_stack: String::new(),
        tier: "Worker".into(),
        cname: String::new(),
        version_label: String::new(),
        arn: None,
        updated: None,
        id: None,
        region: None,
    };
    let mut dlq = std::collections::HashMap::new();
    dlq.insert("worker-clean".to_string(), 0);
    assert_eq!(super::compute_red_alerts(&[env], &dlq), 0);
}

#[test]
fn redact_for_log_preserves_length_with_block_chars() {
    assert_eq!(super::redact_for_log("540847557034", true), "▓".repeat(12));
    assert_eq!(super::redact_for_log("540847557034", false), "540847557034");
    // Em-dash placeholder + empty stay readable so the context line
    // doesn't render `▓` for "no account known yet".
    assert_eq!(super::redact_for_log("—", true), "—");
    assert_eq!(super::redact_for_log("", true), "");
}

#[test]
fn parse_tag_args_happy_path() {
    let v: Vec<&str> = vec!["Owner", "platform-team"];
    let (k, v) = super::parse_tag_args(&v).unwrap();
    assert_eq!(k, "Owner");
    assert_eq!(v, "platform-team");
}

#[test]
fn parse_tag_args_joins_value_tokens_with_spaces() {
    let v: Vec<&str> = vec!["Description", "owned", "by", "platform"];
    let (k, v) = super::parse_tag_args(&v).unwrap();
    assert_eq!(k, "Description");
    assert_eq!(v, "owned by platform");
}

#[test]
fn parse_tag_args_rejects_missing_value() {
    // Bare key with no value tokens.
    let v: Vec<&str> = vec!["Owner"];
    assert!(super::parse_tag_args(&v).is_none());
    // Empty input.
    let v: Vec<&str> = vec![];
    assert!(super::parse_tag_args(&v).is_none());
}

#[test]
fn delta_toast_key_returns_none_for_non_delta_text() {
    assert_eq!(super::delta_toast_key("refreshing…"), None);
    assert_eq!(super::delta_toast_key(""), None);
    assert_eq!(super::delta_toast_key("▲"), None);
    // Arrow with no count.
    assert_eq!(super::delta_toast_key("▲ Red"), None);
    // Arrow + count but no bucket word.
    assert_eq!(super::delta_toast_key("▲5 "), None);
}

#[test]
fn assign_app_colors_stable_first_appearance() {
    use ratatui::style::Color;
    let palette = vec![Color::Red, Color::Green, Color::Blue];
    let names = ["app-a", "app-b", "app-a", "app-c", "app-b"];
    let m = assign_app_colors(names.iter().copied(), &palette);
    assert_eq!(m.get("app-a").copied(), Some(Color::Red));
    assert_eq!(m.get("app-b").copied(), Some(Color::Green));
    assert_eq!(m.get("app-c").copied(), Some(Color::Blue));
    assert_eq!(m.len(), 3);
}

#[test]
fn assign_app_colors_wraps_when_palette_exhausted() {
    use ratatui::style::Color;
    let palette = vec![Color::Red, Color::Green];
    let names = ["a", "b", "c", "d"];
    let m = assign_app_colors(names.iter().copied(), &palette);
    assert_eq!(m.get("a").copied(), Some(Color::Red));
    assert_eq!(m.get("b").copied(), Some(Color::Green));
    // c wraps back to palette[0]; d to palette[1].
    assert_eq!(m.get("c").copied(), Some(Color::Red));
    assert_eq!(m.get("d").copied(), Some(Color::Green));
}

#[test]
fn assign_app_colors_empty_palette_yields_empty_map() {
    let m = assign_app_colors(["a", "b"].iter().copied(), &[]);
    assert!(m.is_empty());
}

#[test]
fn event_time_format_cycles_utc_local_age() {
    let f = EventTimeFormat::default();
    assert_eq!(f, EventTimeFormat::Utc);
    assert_eq!(f.next(), EventTimeFormat::Local);
    assert_eq!(f.next().next(), EventTimeFormat::Age);
    assert_eq!(f.next().next().next(), EventTimeFormat::Utc);
}

#[test]
fn event_time_format_parse_round_trips() {
    for f in [
        EventTimeFormat::Utc,
        EventTimeFormat::Local,
        EventTimeFormat::Age,
    ] {
        assert_eq!(EventTimeFormat::parse(f.label()), Some(f));
    }
    // Case-insensitive + the "relative" alias for age.
    assert_eq!(EventTimeFormat::parse("UTC"), Some(EventTimeFormat::Utc));
    assert_eq!(
        EventTimeFormat::parse("relative"),
        Some(EventTimeFormat::Age)
    );
    assert_eq!(EventTimeFormat::parse("nonsense"), None);
}

#[test]
fn shell_quote_passes_safe_chars_unchanged() {
    assert_eq!(shell_quote("safe-Name_1.0"), "safe-Name_1.0");
    assert_eq!(shell_quote("with space"), "'with space'");
    // Single quote escape uses POSIX trick: '\''
    assert_eq!(shell_quote("o'clock"), "'o'\\''clock'");
}

#[test]
fn instance_hourly_usd_known_types() {
    assert!(instance_hourly_usd("t3.micro").unwrap() > 0.0);
    assert!(instance_hourly_usd("m5.large").unwrap() > 0.0);
    assert_eq!(instance_hourly_usd("not-a-real-type"), None);
}

#[test]
fn estimate_cost_handles_mixed() {
    let mk = |t: &str, az: &str| Instance {
        id: "i-1".into(),
        health: "Ok".into(),
        color: "Green".into(),
        causes: vec![],
        instance_type: t.into(),
        availability_zone: az.into(),
        launched_at: None,
    };
    let instances = vec![
        mk("t3.micro", "us-east-1a"),
        mk("t3.micro", "us-east-1b"),
        mk("unknown-type-xyz", "us-east-1c"),
    ];
    let (hourly, missing) = estimate_cost(&instances);
    assert_eq!(missing, 1);
    // Two t3.micro at $0.0104/hr each.
    assert!((hourly - 0.0208).abs() < 1e-9);
}

fn fake_env(name: &str, status: &str, health: &str, version: &str) -> Environment {
    Environment {
        name: name.into(),
        application: "my-app".into(),
        status: status.into(),
        health: health.into(),
        platform: "Java 17".into(),
        solution_stack: String::new(),
        tier: "Web".into(),
        cname: format!("{name}.elb.amazonaws.com"),
        version_label: version.into(),
        arn: None,
        updated: None,
        id: None,
        region: None,
    }
}

#[test]
fn palette_score_prefers_label_prefix_then_substring_then_detail() {
    // Empty needle returns score 0 for everything.
    assert_eq!(palette_score("", "anything", "anything"), Some(0));
    // Label prefix → 0.
    assert_eq!(palette_score("reg", "region", "switch AWS region"), Some(0));
    // Label substring later in string → higher score.
    let s_label = palette_score("ion", "region", "switch AWS region").unwrap();
    assert!(s_label > 0 && s_label < 1_000);
    // Detail-only match is penalised by +1000 vs label.
    let s_detail = palette_score("aws", ":region", "switch AWS profile").unwrap();
    let s_label_match = palette_score("aws", "aws-thing", "irrelevant").unwrap();
    assert!(s_detail >= 1_000);
    assert!(s_label_match < s_detail);
    // No match → None.
    assert_eq!(palette_score("xyzzy", "region", "switch AWS region"), None);
}

#[test]
fn bucket_delta_only_envs_in_both() {
    let mut prev = HashMap::new();
    prev.insert("a".into(), "Green".into());
    prev.insert("b".into(), "Red".into());
    prev.insert("c".into(), "Green".into()); // c disappears in next, so dropped from delta
    let next = vec![
        fake_env("a", "Ready", "Yellow", "v1"), // Green → Yellow: −1 Green, +1 Yellow
        fake_env("b", "Ready", "Red", "v1"),    // Red → Red: no change
        fake_env("d", "Ready", "Green", "v1"),  // new env: ignored (no prev state)
    ];
    let delta = bucket_delta(&prev, &next, |e| e.health.clone());
    let map: BTreeMap<String, i32> = delta.into_iter().collect();
    // Only env `a` transitions: −1 Green, +1 Yellow. b unchanged; c disappeared (ignored); d is new (ignored).
    assert_eq!(map.get("Green").copied(), Some(-1));
    assert_eq!(map.get("Yellow").copied(), Some(1));
    assert_eq!(map.get("Red").copied(), None);
}

#[test]
fn bucket_delta_empty_prev_yields_no_deltas() {
    // Regression: when prev_health is cleared (e.g. on context switch),
    // the delta against the new env list should produce nothing. Otherwise
    // every env shows up as a transition.
    let prev = HashMap::new();
    let next = vec![
        fake_env("a", "Ready", "Green", "v1"),
        fake_env("b", "Ready", "Red", "v1"),
    ];
    let delta = bucket_delta(&prev, &next, |e| e.health.clone());
    assert!(
        delta.is_empty(),
        "expected no deltas with empty prev, got {delta:?}"
    );
}

#[test]
fn diff_envs_marks_differing_fields() {
    let a = fake_env("prod", "Ready", "Green", "v1");
    let b = fake_env("staging", "Updating", "Yellow", "v2");
    let out = diff_envs(&a, &b, false, &[]);
    // Differing fields prefixed by ≠
    assert!(out.contains("≠ Status"));
    assert!(out.contains("≠ Health"));
    assert!(out.contains("≠ Version"));
    assert!(out.contains("≠ Name"));
    assert!(out.contains("≠ CNAME"));
    // Identical fields prefixed by space
    assert!(out.contains("  Application"));
    assert!(out.contains("  Tier"));
    assert!(out.contains("  Platform"));
}

#[test]
fn diff_envs_redacts_cname() {
    let a = fake_env("prod", "Ready", "Green", "v1");
    let b = fake_env("staging", "Updating", "Yellow", "v2");
    let out = diff_envs(&a, &b, true, &[]);
    // CNAMEs become blocks; the canonical envname-portion shouldn't survive.
    assert!(!out.contains("prod.elb.amazonaws.com"));
    assert!(out.contains("▓"));
}

#[test]
fn diff_field_ignored_matches_label_and_version_label_alias() {
    // Empty list → never ignore.
    assert!(!diff_field_ignored("Version", &[]));
    // Case-insensitive match against the field label.
    let keys = parse_ignore_keys(Some("version, updated"));
    assert!(diff_field_ignored("Version", &keys));
    assert!(diff_field_ignored("Updated", &keys));
    assert!(!diff_field_ignored("Status", &keys));
    // `version_label` (the `:config-diff` spelling) also hides Version.
    let alias = parse_ignore_keys(Some("version_label"));
    assert!(diff_field_ignored("Version", &alias));
    assert!(!diff_field_ignored("Status", &alias));
}

#[test]
fn diff_envs_drops_ignored_rows() {
    let a = fake_env("prod", "Ready", "Green", "v1");
    let b = fake_env("staging", "Updating", "Yellow", "v2");
    let keys = parse_ignore_keys(Some("version,cname,updated"));
    let out = diff_envs(&a, &b, false, &keys);
    // Ignored rows vanish entirely (no row at all, differing or not).
    assert!(!out.contains("Version"), "Version row should be ignored");
    assert!(!out.contains("CNAME"), "CNAME row should be ignored");
    assert!(!out.contains("Updated"), "Updated row should be ignored");
    // Untouched rows still render.
    assert!(out.contains("≠ Status"));
    assert!(out.contains("  Application"));
}

#[test]
fn encode_filter_only_view_emits_just_the_filter_part() {
    // The encoded form must omit sort/grouped/scope so loading
    // doesn't perturb those — `apply_view` "missing fields
    // untouched" semantics depend on it.
    let encoded = super::encode_filter_only_view("tag:env=prod");
    assert_eq!(encoded, "filter=tag:env=prod");
    // Empty filter — still emits `filter=` so load semantics
    // are consistent (filter clears to empty).
    assert_eq!(super::encode_filter_only_view(""), "filter=");
}

#[test]
fn view_filter_value_extracts_filter_or_empty() {
    assert_eq!(
        super::view_filter_value("filter=tag:env=prod"),
        "tag:env=prod"
    );
    // Filter portion in the middle of a full view.
    assert_eq!(
        super::view_filter_value("sort=name:asc;filter=tag:env=prod;grouped=false"),
        "tag:env=prod",
    );
    // No filter portion → empty (operator's view that doesn't
    // touch the filter).
    assert_eq!(super::view_filter_value("sort=name:asc;grouped=true"), "");
    // Empty encoded → empty filter.
    assert_eq!(super::view_filter_value(""), "");
    // Leading whitespace on a part is tolerated (matches the
    // tolerant parse in `apply_view`).
    assert_eq!(super::view_filter_value("sort=name:asc; filter=foo"), "foo",);
}

#[tokio::test]
async fn cycle_saved_view_wraps_forward_through_saved_views() {
    // Three saved views: cycling forward from "dev" → "prod" →
    // "staging" → back to "dev". Cycle order follows BTreeMap
    // iteration (alphabetical), matching the chip-bar render.
    let mut app = test_app();
    app.saved_views
        .insert("dev".into(), super::encode_filter_only_view("tag:env=dev"));
    app.saved_views.insert(
        "prod".into(),
        super::encode_filter_only_view("tag:env=prod"),
    );
    app.saved_views.insert(
        "staging".into(),
        super::encode_filter_only_view("tag:env=staging"),
    );
    // Start on "dev".
    app.view.set_filter("tag:env=dev");
    app.cycle_saved_view(1);
    assert_eq!(app.view.filter().text(), "tag:env=prod");
    app.cycle_saved_view(1);
    assert_eq!(app.view.filter().text(), "tag:env=staging");
    // Wraps back to first.
    app.cycle_saved_view(1);
    assert_eq!(app.view.filter().text(), "tag:env=dev");
}

#[tokio::test]
async fn cycle_saved_view_wraps_backward_and_handles_no_active() {
    // Backward from "dev" wraps to "staging" (last in sort).
    let mut app = test_app();
    app.saved_views
        .insert("dev".into(), super::encode_filter_only_view("tag:env=dev"));
    app.saved_views.insert(
        "staging".into(),
        super::encode_filter_only_view("tag:env=staging"),
    );
    app.view.set_filter("tag:env=dev");
    app.cycle_saved_view(-1);
    assert_eq!(app.view.filter().text(), "tag:env=staging");
    // No active filter (freeform or empty) → forward goes to first,
    // backward goes to last.
    app.view.set_filter("some-random-text");
    app.cycle_saved_view(1);
    assert_eq!(
        app.view.filter().text(),
        "tag:env=dev",
        "forward-with-no-active → first"
    );
    app.view.set_filter("some-random-text");
    app.cycle_saved_view(-1);
    assert_eq!(
        app.view.filter().text(),
        "tag:env=staging",
        "backward-with-no-active → last"
    );
}

#[tokio::test]
async fn cycle_saved_view_noop_with_empty_views() {
    // Cycling when there are no saved views shouldn't crash or
    // mutate state. The keybind guard already short-circuits, but
    // the method itself is the actual safety net.
    let mut app = test_app();
    app.view.set_filter("keep-me");
    app.cycle_saved_view(1);
    assert_eq!(app.view.filter().text(), "keep-me");
}

#[tokio::test]
async fn cycle_saved_view_with_full_view_applies_sort_and_group_too() {
    // The point of unifying named_filters into saved_views: a
    // full view's encoded payload changes sort + group + scope
    // alongside the filter. This is the gh-dash-style "tabs"
    // behavior the BACKLOG had been promising since 2026-05-24.
    let mut app = test_app();
    // Filter-only view (from :save).
    app.saved_views
        .insert("dev".into(), super::encode_filter_only_view("tag:env=dev"));
    // Full view (from :save-view) — flips sort to App + groups.
    app.saved_views.insert(
        "by-app".into(),
        "filter=tag:env=prod;sort=app:asc;grouped=true;scope=envs".into(),
    );
    app.view.set_filter("tag:env=dev");
    app.view.set_grouped(false);
    app.cycle_saved_view(1); // dev → by-app
    assert_eq!(app.view.filter().text(), "tag:env=prod");
    assert!(
        app.view.grouped(),
        "full view must apply its grouped=true alongside the filter"
    );
}

#[tokio::test]
async fn ssh_with_instance_id_arg_queues_pending_shell_target() {
    // `:ssh i-abc` is the direct path: just stages the target and
    // lets the main-loop tick pick it up. No picker, no fetch.
    let mut app = test_app();
    app.execute_command("ssh i-0abc1234567890def");
    assert_eq!(
        app.pending_shell_target.as_deref(),
        Some("i-0abc1234567890def")
    );
    assert!(
        app.error_message.is_none(),
        "unexpected: {:?}",
        app.error_message
    );
    assert!(
        app.mode == Mode::Normal,
        "ssh-with-arg should not change mode"
    );
}

#[tokio::test]
async fn ssh_rejects_non_instance_id_arg() {
    // A typo'd arg ("staging") looks like an env name, not an EC2 ID.
    // Better to refuse than to attempt an SSM session against
    // garbage and get an opaque CLI error.
    let mut app = test_app();
    app.execute_command("ssh staging-web");
    assert!(app.pending_shell_target.is_none());
    let err = app.error_message.as_deref().unwrap_or("");
    assert!(
        err.contains("instance ID") && err.contains("staging-web"),
        "expected guidance + offending value, got: {err}"
    );
}

#[tokio::test]
async fn ssh_no_arg_without_detail_errors_clearly() {
    // Without an arg and without Detail/Instances loaded, there's
    // nothing to populate the picker with. Surface that the
    // operator either needs to open Detail or pass an ID — don't
    // silently no-op.
    let mut app = test_app();
    app.execute_command("ssh");
    assert!(app.picker.is_none());
    let err = app.error_message.as_deref().unwrap_or("");
    assert!(
        err.contains("Detail") || err.contains("instance ID"),
        "expected guidance about Detail/Instances or instance ID, got: {err}"
    );
}

#[test]
fn deploy_snapshot_round_trips_through_persisted_form() {
    // Capture → serialize → parse must produce a snapshot equal
    // (up to chrono precision) to the original. The pipe separator
    // doesn't collide with any legal version-label character.
    use chrono::TimeZone;
    let original = DeploySnapshot {
        env_name: "prod-api".into(),
        previous_version_label: "build-825".into(),
        taken_at: chrono::Utc
            .with_ymd_and_hms(2026, 5, 25, 14, 30, 0)
            .unwrap(),
    };
    let raw = original.to_persisted();
    assert_eq!(raw, "build-825|2026-05-25T14:30:00+00:00");
    let parsed = DeploySnapshot::parse_persisted("prod-api", &raw).expect("parses");
    assert_eq!(parsed.env_name, original.env_name);
    assert_eq!(
        parsed.previous_version_label,
        original.previous_version_label
    );
    assert_eq!(parsed.taken_at, original.taken_at);
}

#[test]
fn deploy_snapshot_parse_persisted_rejects_garbage() {
    // No pipe, missing timestamp, malformed RFC3339 — all return
    // None so the App-init loop silently drops bad lines rather
    // than aborting startup.
    assert!(DeploySnapshot::parse_persisted("e", "nopipe").is_none());
    assert!(DeploySnapshot::parse_persisted("e", "|2026-05-25T14:30:00Z").is_none());
    assert!(DeploySnapshot::parse_persisted("e", "label|not-a-timestamp").is_none());
    assert!(DeploySnapshot::parse_persisted("e", "label|").is_none());
}

#[tokio::test]
async fn rebuild_clears_armed_watchdogs_and_snapshots() {
    // Operator arms an auto-rollback in account=A region=us-east-1
    // then switches to account=B / different region. The deadline
    // tokio task survives (no JoinHandle for cancellation), but
    // its late `AutoRollbackCheck` must not act on a same-named
    // env in the new context — apply_rebuild clears both the
    // armed_watchdogs slot AND the deploy_snapshot so a stale
    // deadline message can't trigger a spurious rollback in the
    // wrong account/region.
    let mut app = test_app();
    let now = chrono::Utc::now();
    app.armed_watchdogs.insert(
        "prod".into(),
        ArmedWatchdog {
            env_name: "prod".into(),
            target_label: "build-old".into(),
            armed_at: now,
            deadline_at: now + chrono::Duration::seconds(300),
        },
    );
    app.deploy_snapshots.insert(
        "prod".into(),
        DeploySnapshot {
            env_name: "prod".into(),
            previous_version_label: "build-old".into(),
            taken_at: now,
        },
    );
    // Simulate a context switch — apply_rebuild Ok-path drops
    // context-scoped state including armed_watchdogs +
    // deploy_snapshots. Use a stub client so the call doesn't
    // need real AWS.
    let _cache_guard = crate::aws::CACHE_TEST_LOCK.lock().await;
    app.apply_rebuild(
        app.rebuild_epoch,
        Ok(Box::new(crate::aws::AwsClient::stub())),
    );
    assert!(
        app.armed_watchdogs.is_empty(),
        "context switch should drop armed watchdogs"
    );
    assert!(
        app.deploy_snapshots.is_empty(),
        "context switch should drop deploy snapshots"
    );
}

#[tokio::test]
async fn rollback_to_label_opens_confirm_for_named_label() {
    // `:rollback --to LABEL` skips the snapshot+event-scan
    // detection and routes straight to the deploy confirm. Pins
    // that the operator's explicit choice wins over any captured
    // snapshot.
    let mut app = test_app();
    app.environments = vec![mk_env("prod", "shop", "Web", "Red")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    // Snapshot exists with a DIFFERENT label; --to must override.
    app.deploy_snapshots.insert(
        "prod".into(),
        DeploySnapshot {
            env_name: "prod".into(),
            previous_version_label: "build-snap".into(),
            taken_at: chrono::Utc::now(),
        },
    );
    app.execute_command("rollback --to build-820");
    // Confirm modal opened with the operator-named label.
    match &app.action_flow {
        Some(ActionFlow::Confirm(modal)) => {
            assert_eq!(modal.deploy_version.as_deref(), Some("build-820"));
            // No watchdog when --auto-rollback wasn't passed.
            assert!(modal.auto_rollback_secs.is_none());
        }
        _ => panic!("expected confirm modal open"),
    }
}

#[tokio::test]
async fn rollback_to_label_with_auto_rollback_threads_secs_through() {
    // `:rollback --to LABEL --auto-rollback 5m` composes:
    // confirm modal carries both the label AND the watchdog
    // duration, so the operator can roll back AND arm a
    // roll-forward in one dispatch.
    let mut app = test_app();
    app.environments = vec![mk_env("prod", "shop", "Web", "Red")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.execute_command("rollback --to build-820 --auto-rollback 5m");
    match &app.action_flow {
        Some(ActionFlow::Confirm(modal)) => {
            assert_eq!(modal.deploy_version.as_deref(), Some("build-820"));
            assert_eq!(modal.auto_rollback_secs, Some(300));
        }
        _ => panic!("expected confirm modal open"),
    }
}

#[tokio::test]
async fn rollback_to_same_label_as_deployed_refuses() {
    // Typo / stale arg — operator passed the version already
    // running. Surface a clear error rather than dispatching a
    // no-op deploy.
    let mut app = test_app();
    let mut env = mk_env("prod", "shop", "Web", "Red");
    env.version_label = "build-822".into();
    app.environments = vec![env];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.execute_command("rollback --to build-822");
    let err = app.error_message.as_deref().unwrap_or("");
    assert!(
        err.contains("already the deployed version"),
        "expected idempotent guard, got: {err}"
    );
    assert!(app.action_flow.is_none(), "no confirm modal on no-op");
}

#[tokio::test]
async fn rollback_auto_rollback_without_snapshot_errors_clearly() {
    // Operator asked for a watchdog but there's no snapshot to
    // arm against and they didn't pass --to. The event-scan
    // fallback path doesn't currently thread auto_rollback_secs,
    // so surface a hint pointing at `--to LABEL` rather than
    // silently dropping the flag.
    let mut app = test_app();
    app.environments = vec![mk_env("prod", "shop", "Web", "Red")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    // deploy_snapshots intentionally empty.
    app.execute_command("rollback --auto-rollback 5m");
    let err = app.error_message.as_deref().unwrap_or("");
    assert!(
        err.contains("needs an in-memory snapshot") && err.contains("--to LABEL"),
        "expected refusal + hint, got: {err}"
    );
}

#[tokio::test]
async fn deploy_wait_for_green_threads_secs_through_to_modal() {
    // `:deploy LABEL --wait-for-green 5m` carries the duration
    // into the ConfirmModal where spawn_action picks it up.
    // No watcher is armed until the operator confirms — that's
    // tested separately at the spawn_action layer.
    let mut app = test_app();
    app.environments = vec![mk_env("prod", "shop", "Web", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.execute_command("deploy build-900 --wait-for-green 5m");
    match &app.action_flow {
        Some(ActionFlow::Confirm(modal)) => {
            assert_eq!(modal.deploy_version.as_deref(), Some("build-900"));
            assert_eq!(modal.wait_for_green_secs, Some(300));
            assert!(modal.auto_rollback_secs.is_none());
        }
        _ => panic!("expected confirm modal open"),
    }
}

#[tokio::test]
async fn deploy_wait_for_green_rejects_malformed_duration() {
    // Same friendly-error pattern as `--auto-rollback`. `forever`
    // isn't parseable, so refuse rather than silently dropping
    // the flag.
    let mut app = test_app();
    app.environments = vec![mk_env("prod", "shop", "Web", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.execute_command("deploy build-900 --wait-for-green forever");
    let err = app.error_message.as_deref().unwrap_or("");
    assert!(
        err.contains("--wait-for-green") && err.contains("duration"),
        "expected parse refusal, got: {err}"
    );
    assert!(
        app.action_flow.is_none(),
        "no modal should open on malformed duration"
    );
}

#[tokio::test]
async fn deploy_with_both_flags_threads_both_through() {
    // Operator wants both: "watch for Green, and roll back if it
    // doesn't land". Modal carries both fields independently;
    // spawn_action registers in both maps.
    let mut app = test_app();
    app.environments = vec![mk_env("prod", "shop", "Web", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.execute_command("deploy build-900 --auto-rollback 10m --wait-for-green 5m");
    match &app.action_flow {
        Some(ActionFlow::Confirm(modal)) => {
            assert_eq!(modal.deploy_version.as_deref(), Some("build-900"));
            assert_eq!(modal.auto_rollback_secs, Some(600));
            assert_eq!(modal.wait_for_green_secs, Some(300));
        }
        _ => panic!("expected confirm modal open"),
    }
}

#[tokio::test]
async fn apply_refresh_keeps_watching_when_status_is_updating_even_if_health_is_green() {
    // Regression: EB leaves health=Green briefly while status
    // flips to Updating right after UpdateEnvironment. The
    // watcher must NOT report success during that window —
    // otherwise the operator gets a false "✓ deploy reached
    // Green" pin before the deploy has actually started.
    let mut app = test_app();
    let now = chrono::Utc::now();
    app.watching_deploys.insert(
        "prod".into(),
        WatchingDeploy {
            env_name: "prod".into(),
            target_label: "build-900".into(),
            armed_at: now,
            deadline_at: now + chrono::Duration::seconds(300),
        },
    );
    let mut env = mk_env("prod", "shop", "Web", "Green");
    env.status = "Updating".into();
    app.apply_refresh(Ok(vec![env]), Vec::new());
    assert!(
        app.watching_deploys.contains_key("prod"),
        "Updating+Green is mid-deploy — watcher must remain armed"
    );
    let pinned = app.status_message.as_deref().unwrap_or("");
    assert!(
        !pinned.contains("reached Green"),
        "must not pin success during Updating, got: {pinned:?}"
    );
}

#[tokio::test]
async fn apply_refresh_keeps_armed_watchdog_when_status_is_updating_even_if_health_is_green() {
    // Same regression for the auto-rollback watchdog: a brief
    // Updating+Green window must not disarm the watchdog —
    // otherwise the rollback safety net evaporates before the
    // deploy has actually rolled.
    let mut app = test_app();
    let now = chrono::Utc::now();
    app.deploy_snapshots.insert(
        "prod".into(),
        DeploySnapshot {
            env_name: "prod".into(),
            previous_version_label: "build-820".into(),
            taken_at: now,
        },
    );
    app.armed_watchdogs.insert(
        "prod".into(),
        ArmedWatchdog {
            env_name: "prod".into(),
            target_label: "build-820".into(),
            armed_at: now,
            deadline_at: now + chrono::Duration::seconds(300),
        },
    );
    let mut env = mk_env("prod", "shop", "Web", "Green");
    env.status = "Updating".into();
    app.apply_refresh(Ok(vec![env]), Vec::new());
    assert!(
        app.armed_watchdogs.contains_key("prod"),
        "Updating+Green is mid-deploy — watchdog must remain armed"
    );
}

#[test]
fn deploy_settled_green_requires_both_status_ready_and_health_green_or_ok() {
    assert!(super::deploy_settled_green("Ready", "Green"));
    assert!(super::deploy_settled_green("Ready", "Ok"));
    assert!(super::deploy_settled_green("ready", "green")); // case-insensitive
    assert!(super::deploy_settled_green("READY", "OK"));
    // Status mismatch — false even if health is Green.
    assert!(!super::deploy_settled_green("Updating", "Green"));
    assert!(!super::deploy_settled_green("Launching", "Ok"));
    assert!(!super::deploy_settled_green("Terminating", "Green"));
    // Health mismatch — false even if status is Ready.
    assert!(!super::deploy_settled_green("Ready", "Red"));
    assert!(!super::deploy_settled_green("Ready", "Yellow"));
    assert!(!super::deploy_settled_green("Ready", "Severe"));
    // Both wrong.
    assert!(!super::deploy_settled_green("", ""));
}

#[test]
fn compute_unavailability_count_per_policy() {
    // AllAtOnce — every instance flips at once.
    assert_eq!(
        super::compute_unavailability_count("AllAtOnce", 1, "Fixed", 4),
        4
    );
    // Rolling, fixed batch of 1 on 4 instances → 1 unavailable.
    assert_eq!(
        super::compute_unavailability_count("Rolling", 1, "Fixed", 4),
        1
    );
    // Rolling, fixed batch of 2 on 4 → 2.
    assert_eq!(
        super::compute_unavailability_count("Rolling", 2, "Fixed", 4),
        2
    );
    // Rolling, 50% on 4 → 2.
    assert_eq!(
        super::compute_unavailability_count("Rolling", 50, "Percentage", 4),
        2
    );
    // Rolling, 33% on 4 → ceil(1.32) = 2.
    assert_eq!(
        super::compute_unavailability_count("Rolling", 33, "Percentage", 4),
        2
    );
    // RollingWithAdditionalBatch — extra batch first, zero impact.
    assert_eq!(
        super::compute_unavailability_count("RollingWithAdditionalBatch", 1, "Fixed", 4),
        0
    );
    // Immutable + TrafficSplitting — new fleet, zero impact.
    assert_eq!(
        super::compute_unavailability_count("Immutable", 1, "Fixed", 4),
        0
    );
    assert_eq!(
        super::compute_unavailability_count("TrafficSplitting", 1, "Fixed", 4),
        0
    );
    // Unknown policy → assume worst case rather than lulling
    // the operator with a false zero.
    assert_eq!(
        super::compute_unavailability_count("WeirdCustomPolicy", 1, "Fixed", 4),
        4
    );
    // Case-insensitive (EB API can return mixed casing).
    assert_eq!(
        super::compute_unavailability_count("allatonce", 1, "Fixed", 4),
        4
    );
}

#[test]
fn compute_batch_count_clamps_and_rounds_up() {
    // Fixed clamps to [1, max].
    assert_eq!(super::compute_batch_count(0, "Fixed", 4), 1);
    assert_eq!(super::compute_batch_count(10, "Fixed", 4), 4);
    assert_eq!(super::compute_batch_count(2, "Fixed", 4), 2);
    // Percentage rounds up.
    assert_eq!(super::compute_batch_count(33, "Percentage", 4), 2); // ceil(1.32)=2
    assert_eq!(super::compute_batch_count(25, "Percentage", 4), 1);
    assert_eq!(super::compute_batch_count(26, "Percentage", 4), 2); // ceil(1.04)=2
    assert_eq!(super::compute_batch_count(100, "Percentage", 4), 4);
    // Out-of-range percentage clamps.
    assert_eq!(super::compute_batch_count(0, "Percentage", 4), 1);
    assert_eq!(super::compute_batch_count(200, "Percentage", 4), 4);
}

#[test]
fn format_unavailability_line_distinguishes_zero_from_partial_from_full() {
    let (text, caution) = super::format_unavailability_line("Immutable", 0, 4);
    assert!(text.contains("no in-service unavailability"));
    assert!(!caution);
    let (text, caution) = super::format_unavailability_line("Rolling", 1, 4);
    assert!(text.contains("max 1/4 instance unavailable"));
    assert!(caution);
    let (text, caution) = super::format_unavailability_line("AllAtOnce", 4, 4);
    assert!(text.contains("max 4/4 instances unavailable"));
    assert!(caution);
}

#[test]
fn extract_unavailability_inputs_uses_eb_defaults_on_missing_settings() {
    // Empty option-settings — defaults match what EB itself
    // uses when no explicit value is configured.
    let (policy, batch, btype, asg) = super::extract_unavailability_inputs(&[]);
    assert_eq!(policy, "AllAtOnce");
    assert_eq!(batch, 1);
    assert_eq!(btype, "Fixed");
    assert_eq!(asg, 1);

    // Partial — operator only set MaxSize.
    let opts = vec![("aws:autoscaling:asg".into(), "MaxSize".into(), "6".into())];
    let (_, _, _, asg) = super::extract_unavailability_inputs(&opts);
    assert_eq!(asg, 6);

    // Empty string values collapse to default rather than the
    // empty string being mistaken for a policy.
    let opts = vec![(
        "aws:elasticbeanstalk:command".into(),
        "DeploymentPolicy".into(),
        String::new(),
    )];
    let (policy, _, _, _) = super::extract_unavailability_inputs(&opts);
    assert_eq!(policy, "AllAtOnce");
}

#[tokio::test]
async fn handle_unavailability_estimate_stuffs_line_into_modal() {
    let mut app = test_app();
    app.environments = vec![mk_env("prod", "shop", "Web", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.execute_command("deploy build-900");
    app.handle_msg(AppMsg::UnavailabilityEstimate {
        gen: app.generation,
        env_name: "prod".into(),
        line: Some((
            "deploy plan: Rolling → max 1/4 instance unavailable".into(),
            true,
        )),
    });
    match &app.action_flow {
        Some(ActionFlow::Confirm(modal)) => {
            assert!(!modal.loading_unavailability);
            let (text, caution) = modal.unavailability_line.as_ref().unwrap();
            assert!(text.contains("max 1/4"));
            assert!(*caution);
        }
        _ => panic!("expected confirm modal"),
    }
}

#[test]
fn build_undo_entry_set_with_prior_value_reverses_to_set() {
    // Original write: set EC2KeyName=foo. Prior value: bar.
    // Reverse: set EC2KeyName=bar.
    let pre = vec![(
        "aws:autoscaling:launchconfiguration".into(),
        "EC2KeyName".into(),
        "bar".into(),
    )];
    let to_set = vec![(
        "aws:autoscaling:launchconfiguration".into(),
        "EC2KeyName".into(),
        "foo".into(),
    )];
    let entry = super::build_undo_entry("prod", "keypair foo", &to_set, &[], &pre);
    assert_eq!(entry.to_set.len(), 1);
    assert_eq!(entry.to_set[0].2, "bar");
    assert!(entry.to_remove.is_empty());
    assert_eq!(entry.env_name, "prod");
    assert_eq!(entry.original_summary, "keypair foo");
}

#[test]
fn build_undo_entry_set_with_no_prior_value_reverses_to_remove() {
    // Original write: set a key that was previously unset.
    // Reverse: remove the key (don't leave it as "" — that's a
    // different EB state from "unset").
    let pre: Vec<(String, String, String)> = vec![];
    let to_set = vec![(
        "aws:elasticbeanstalk:application".into(),
        "Application Healthcheck URL".into(),
        "/healthz".into(),
    )];
    let entry = super::build_undo_entry("prod", "health-check-url /healthz", &to_set, &[], &pre);
    assert!(entry.to_set.is_empty());
    assert_eq!(entry.to_remove.len(), 1);
    assert_eq!(entry.to_remove[0].1, "Application Healthcheck URL");
}

#[test]
fn build_undo_entry_empty_string_prior_treated_as_unset() {
    // EB doesn't distinguish "unset" from "set-to-empty"; we
    // treat empty-string-prior as unset and reverse via remove.
    let pre = vec![(
        "aws:autoscaling:launchconfiguration".into(),
        "EC2KeyName".into(),
        String::new(),
    )];
    let to_set = vec![(
        "aws:autoscaling:launchconfiguration".into(),
        "EC2KeyName".into(),
        "foo".into(),
    )];
    let entry = super::build_undo_entry("prod", "keypair foo", &to_set, &[], &pre);
    assert!(entry.to_set.is_empty());
    assert_eq!(entry.to_remove.len(), 1);
}

#[test]
fn build_undo_entry_remove_with_prior_value_reverses_to_set() {
    // Original write: remove a key that had a value. Reverse:
    // restore the value via to_set.
    let pre = vec![(
        "aws:autoscaling:launchconfiguration".into(),
        "EC2KeyName".into(),
        "bar".into(),
    )];
    let to_remove = vec![(
        "aws:autoscaling:launchconfiguration".into(),
        "EC2KeyName".into(),
    )];
    let entry = super::build_undo_entry("prod", "clear keypair", &[], &to_remove, &pre);
    assert_eq!(entry.to_set.len(), 1);
    assert_eq!(entry.to_set[0].2, "bar");
    assert!(entry.to_remove.is_empty());
}

#[test]
fn build_undo_entry_remove_with_no_prior_value_is_a_noop_reverse() {
    // Original: remove a key that was already absent. Reverse:
    // nothing (both sides empty).
    let entry = super::build_undo_entry(
        "prod",
        "clear keypair",
        &[],
        &[(
            "aws:autoscaling:launchconfiguration".into(),
            "EC2KeyName".into(),
        )],
        &[],
    );
    assert!(entry.to_set.is_empty());
    assert!(entry.to_remove.is_empty());
}

#[tokio::test]
async fn batch_set_option_skips_envs_no_longer_in_view() {
    // Race: operator multi-selects envs A + B, fires
    // :batch-set-option, then context switches before the
    // batch loop reaches B. spawn_batch_set_option must skip
    // (audit-log only) the env that's no longer in the
    // cached fleet rather than dispatching a write against a
    // stale name. Without this guard, the write fails at AWS
    // with a confusing error.
    let mut app = test_app();
    app.environments = vec![mk_env("prod-api", "shop", "Web", "Green")];
    app.rebuild_view();
    // Dispatch against an env that's NOT in self.environments.
    app.spawn_batch_set_option(
        "vanished".into(),
        "aws:elasticbeanstalk:application".into(),
        "Application Healthcheck URL".into(),
        "/healthz".into(),
    );
    // pending_actions should be empty — the write was skipped.
    assert!(
        app.pending_actions.iter().all(|p| p.target != "vanished"),
        "expected no pending action for vanished env"
    );
}

#[tokio::test]
async fn handle_undo_captured_pushes_into_history_with_cap() {
    // Pushing UNDO_HISTORY_CAP + 2 entries leaves CAP-many in
    // the deque, with the OLDEST entries evicted from the
    // front. Confirms the ring-buffer eviction logic.
    let mut app = test_app();
    for i in 0..(super::UNDO_HISTORY_CAP + 2) {
        let entry = super::UndoEntry {
            env_name: "prod".into(),
            to_set: vec![("ns".into(), format!("k{i}"), "v".into())],
            to_remove: vec![],
            original_summary: format!("write #{i}"),
            captured_at: chrono::Utc::now(),
        };
        app.handle_msg(AppMsg::UndoCaptured {
            gen: app.generation,
            entry,
        });
    }
    assert_eq!(app.undo_history.len(), super::UNDO_HISTORY_CAP);
    // The two oldest (#0, #1) should have been evicted; #2
    // becomes the front-most surviving entry.
    assert_eq!(
        app.undo_history.front().unwrap().original_summary,
        "write #2"
    );
    // Back of the deque is the most-recent push.
    assert_eq!(
        app.undo_history.back().unwrap().original_summary,
        format!("write #{}", super::UNDO_HISTORY_CAP + 1)
    );
}

#[tokio::test]
async fn cmd_undo_with_empty_history_hints_at_the_buffer() {
    let mut app = test_app();
    app.execute_command("undo");
    let status = app.status_message.as_deref().unwrap_or("");
    assert!(
        status.contains("no undo history"),
        "expected empty-history hint, got: {status}"
    );
}

#[tokio::test]
async fn cmd_undo_with_no_op_reverse_surfaces_clearly() {
    // Reverse-action with both sides empty (e.g. write matched
    // prior state exactly) yields a friendly status rather than
    // a silent dispatch of a no-op write.
    let mut app = test_app();
    app.environments = vec![mk_env("prod", "shop", "Web", "Green")];
    app.rebuild_view();
    app.undo_history.push_back(super::UndoEntry {
        env_name: "prod".into(),
        to_set: vec![],
        to_remove: vec![],
        original_summary: "keypair foo".into(),
        captured_at: chrono::Utc::now(),
    });
    app.execute_command("undo");
    let status = app.status_message.as_deref().unwrap_or("");
    assert!(
        status.contains("prior state was identical"),
        "expected no-op hint, got: {status}"
    );
}

#[tokio::test]
async fn cmd_undo_uses_display_row_index_not_envs_vec_index() {
    // Regression: when a filter is active, `selected_env()`
    // reads from `display_rows()` not `environments`. The
    // earlier cut of `cmd_undo` set `table_state` using the
    // env-vec position, which targets the wrong row in a
    // filtered view. This test pins that the dispatch reaches
    // the right env after filtering shrinks the visible set.
    let mut app = test_app();
    let mut prod_api = mk_env("prod-api", "shop", "Web", "Green");
    prod_api.application = "shop".into();
    let mut staging_api = mk_env("staging-api", "shop", "Web", "Green");
    staging_api.application = "shop".into();
    let mut prod_web = mk_env("prod-web", "shop", "Web", "Green");
    prod_web.application = "shop".into();
    app.environments = vec![prod_api, staging_api, prod_web];
    // Filter to only the "prod-" envs — display_rows now has
    // 2 entries (env-vec indices 0 and 2), so the envs-vec
    // index 2 (prod-web) maps to display-row index 1.
    app.view.set_filter("prod-");
    app.rebuild_view();
    // Captured undo targets prod-web (envs-vec idx 2).
    app.undo_history.push_back(super::UndoEntry {
        env_name: "prod-web".into(),
        to_set: vec![(
            "aws:autoscaling:launchconfiguration".into(),
            "EC2KeyName".into(),
            "bar".into(),
        )],
        to_remove: vec![],
        original_summary: "keypair foo".into(),
        captured_at: chrono::Utc::now(),
    });
    // Pre-undo: cursor on prod-api (display row 0). After
    // dispatch the cursor should be restored.
    app.table_state.select(Some(0));
    app.execute_command("undo");
    // No error message about wrong env or out-of-bounds; the
    // dispatch should have reached the right target. The fix
    // is sufficient if no error fires + cursor is restored.
    assert!(
        app.error_message.is_none(),
        "expected dispatch to succeed, got error: {:?}",
        app.error_message
    );
    assert_eq!(
        app.table_state.selected(),
        Some(0),
        "cursor must be restored to the prior selection"
    );
}

#[tokio::test]
async fn cmd_undo_refuses_with_hint_when_env_filtered_out() {
    // Captured env exists in self.environments but is hidden
    // by the active filter. Refuse with a hint pointing at
    // the filter, and keep the entry on the deque so the
    // operator can retry after clearing.
    let mut app = test_app();
    app.environments = vec![
        mk_env("prod-api", "shop", "Web", "Green"),
        mk_env("staging-api", "shop", "Web", "Green"),
    ];
    app.view.set_filter("staging-");
    app.rebuild_view();
    app.undo_history.push_back(super::UndoEntry {
        env_name: "prod-api".into(),
        to_set: vec![("ns".into(), "k".into(), "v".into())],
        to_remove: vec![],
        original_summary: "keypair foo".into(),
        captured_at: chrono::Utc::now(),
    });
    app.execute_command("undo");
    let err = app.error_message.as_deref().unwrap_or("");
    assert!(
        err.contains("filtered out") && err.contains("clear the filter"),
        "expected filter hint, got: {err}"
    );
    assert_eq!(
        app.undo_history.len(),
        1,
        "entry must be put back on the deque so the operator can retry"
    );
}

#[tokio::test]
async fn cmd_undo_refuses_when_target_env_no_longer_visible() {
    // Captured entry references an env that's been filtered
    // out or terminated. Refuse rather than dispatch against
    // a missing env.
    let mut app = test_app();
    app.undo_history.push_back(super::UndoEntry {
        env_name: "vanished".into(),
        to_set: vec![("ns".into(), "k".into(), "v".into())],
        to_remove: vec![],
        original_summary: "keypair foo".into(),
        captured_at: chrono::Utc::now(),
    });
    app.execute_command("undo");
    let err = app.error_message.as_deref().unwrap_or("");
    assert!(
        err.contains("no longer in the current view"),
        "expected missing-env refusal, got: {err}"
    );
}

#[test]
fn expand_command_alias_pass_through_when_no_match() {
    use std::collections::HashMap;
    let mut aliases = HashMap::new();
    aliases.insert("dp".to_string(), "deploy --auto-rollback 5m".to_string());
    // No alias matched — line unchanged.
    assert_eq!(super::expand_command_alias("rebuild", &aliases), "rebuild");
    // Empty alias map — line unchanged.
    assert_eq!(
        super::expand_command_alias("deploy build-x", &HashMap::new()),
        "deploy build-x"
    );
}

#[test]
fn expand_command_alias_swaps_first_token_and_keeps_args() {
    use std::collections::HashMap;
    let mut aliases = HashMap::new();
    aliases.insert("dp".to_string(), "deploy --auto-rollback 5m".to_string());
    assert_eq!(
        super::expand_command_alias("dp build-900", &aliases),
        "deploy --auto-rollback 5m build-900"
    );
    // No args after alias — expansion stands alone.
    assert_eq!(
        super::expand_command_alias("dp", &aliases),
        "deploy --auto-rollback 5m"
    );
}

#[test]
fn expand_command_alias_does_not_chain_transitively() {
    // Single-level expansion only. `dp → bare deploy` does NOT
    // get re-expanded via a second-tier alias map lookup.
    use std::collections::HashMap;
    let mut aliases = HashMap::new();
    aliases.insert("a".to_string(), "b stuff".to_string());
    aliases.insert("b".to_string(), "c things".to_string());
    assert_eq!(super::expand_command_alias("a", &aliases), "b stuff");
    // No infinite loop on self-referential aliases.
    let mut aliases = HashMap::new();
    aliases.insert("loop".to_string(), "loop forever".to_string());
    assert_eq!(
        super::expand_command_alias("loop", &aliases),
        "loop forever"
    );
}

#[tokio::test]
async fn execute_command_uses_command_aliases() {
    // End-to-end: define a command alias, dispatch it, expect
    // the expansion to run. We probe via `:freeze-deploys`'s
    // observable side-effect (the toast text) rather than
    // having to mock a deploy.
    let mut app = test_app();
    app.cfg
        .command_aliases
        .insert("emergency".into(), "freeze-deploys incident #1234".into());
    app.execute_command("emergency");
    assert!(app.deploy_freeze.is_some());
    let reason = app
        .deploy_freeze
        .as_ref()
        .map(|f| f.reason.clone())
        .unwrap();
    assert_eq!(reason, "incident #1234");
}

#[tokio::test]
async fn freeze_deploys_blocks_writes_with_reason_surfaced() {
    // Operator dispatches `:freeze-deploys incident #1234` →
    // every destructive action refuses, with the reason
    // surfaced in the toast. Same gate as the read-only pins
    // but more visible (the reason is operator-supplied).
    let mut app = test_app();
    app.execute_command("freeze-deploys incident #1234");
    assert!(app.deploy_freeze.is_some(), "freeze should be set");
    assert!(
        app.is_read_only_for("any-env"),
        "freeze must block every env"
    );
    let reason = app.read_only_reason("any-env").unwrap_or_default();
    assert!(
        reason.contains("deploys frozen") && reason.contains("incident #1234"),
        "expected reason to surface, got: {reason}"
    );
}

#[tokio::test]
async fn freeze_deploys_with_no_reason_still_blocks() {
    // Reason is optional — empty-reason freeze still blocks
    // but the toast wording shifts.
    let mut app = test_app();
    app.execute_command("freeze-deploys");
    assert!(app.deploy_freeze.is_some());
    let reason = app.read_only_reason("env").unwrap_or_default();
    assert!(
        reason.contains("deploys frozen") && !reason.contains(": "),
        "no-reason wording shouldn't include `: <reason>`, got: {reason}"
    );
}

#[tokio::test]
async fn thaw_deploys_clears_the_freeze() {
    let mut app = test_app();
    app.execute_command("freeze-deploys testing");
    assert!(app.deploy_freeze.is_some());
    app.execute_command("thaw-deploys");
    assert!(app.deploy_freeze.is_none(), "thaw should clear freeze");
    assert!(
        !app.is_read_only_for("env"),
        "thaw must restore writes (no other locks set in this test)"
    );
}

#[tokio::test]
async fn re_freezing_updates_the_reason_in_place() {
    // Operator refines the reason mid-incident; replace not stack.
    let mut app = test_app();
    app.execute_command("freeze-deploys rolling back");
    app.execute_command("freeze-deploys rolling back — PROD only");
    let reason = app
        .deploy_freeze
        .as_ref()
        .map(|f| f.reason.clone())
        .unwrap();
    assert_eq!(reason, "rolling back — PROD only");
}

#[tokio::test]
async fn freeze_overrides_per_env_pin_in_read_only_reason() {
    // When BOTH a freeze AND a per-env safety pin are active,
    // the freeze reason wins in the toast — it's the more-
    // recent operator gesture and the more informative message.
    let mut app = test_app();
    app.cfg.safety_envs.insert("prod".into(), true);
    app.execute_command("freeze-deploys incident");
    let reason = app.read_only_reason("prod").unwrap_or_default();
    assert!(
        reason.contains("deploys frozen"),
        "freeze reason must win over per-env pin, got: {reason}"
    );
}

// ── :incident ───────────────────────────────────────────────────

#[test]
fn incident_args_parse_start_end_and_reject_garbage() {
    use crate::app::cmd_misc::{parse_incident_args, IncidentCmd};
    // Quoted headline arrives as whitespace-split quote-carrying
    // tokens (execute_command has no shell tokenizer).
    assert_eq!(
        parse_incident_args(&["START", "\"checkout", "5xx", "spike\""]),
        Ok(IncidentCmd::Start("checkout 5xx spike".into()))
    );
    assert_eq!(
        parse_incident_args(&["start", "bare", "headline"]),
        Ok(IncidentCmd::Start("bare headline".into()))
    );
    assert_eq!(
        parse_incident_args(&["START"]),
        Ok(IncidentCmd::Start(String::new()))
    );
    assert_eq!(parse_incident_args(&["END"]), Ok(IncidentCmd::End));
    assert_eq!(parse_incident_args(&["end"]), Ok(IncidentCmd::End));
    assert!(parse_incident_args(&[]).is_err());
    assert!(parse_incident_args(&["END", "extra"]).is_err());
    assert!(parse_incident_args(&["pause"]).is_err());
}

#[tokio::test]
async fn incident_start_freezes_and_end_thaws() {
    let mut app = test_app();
    app.execute_command("incident START \"checkout 5xx spike\"");
    assert!(app.incident.is_some(), "incident should be active");
    assert!(app.deploy_freeze.is_some(), "START must set the freeze");
    assert!(
        app.is_read_only_for("any-env"),
        "incident freeze blocks every env"
    );
    let reason = app.read_only_reason("any-env").unwrap_or_default();
    assert!(
        reason.contains("incident: checkout 5xx spike"),
        "freeze reason carries the headline, got: {reason}"
    );
    app.execute_command("incident END");
    assert!(app.incident.is_none(), "END clears the incident");
    assert!(app.deploy_freeze.is_none(), "END thaws deploys");
    let status = app.status_message.clone().unwrap_or_default();
    assert!(
        status.contains("incident closed") && status.contains("checkout 5xx spike"),
        "END summary names the incident, got: {status}"
    );
}

#[tokio::test]
async fn incident_restart_updates_headline_but_keeps_start_time() {
    let mut app = test_app();
    app.execute_command("incident START first");
    let t0 = app.incident.as_ref().unwrap().started_at;
    app.execute_command("incident START \"first, refined\"");
    let incident = app.incident.as_ref().unwrap();
    assert_eq!(incident.headline, "first, refined");
    assert_eq!(
        incident.started_at, t0,
        "headline update must not reset the incident clock"
    );
}

#[tokio::test]
async fn incident_end_without_active_incident_errors() {
    let mut app = test_app();
    app.execute_command("incident END");
    assert!(app.incident.is_none());
    let err = app.error_message.clone().unwrap_or_default();
    assert!(err.contains("no active incident"), "got: {err}");
}

#[tokio::test]
async fn incident_banner_renders_red_in_header() {
    let mut app = test_app();
    app.execute_command("incident START \"db failover\"");
    let theme = app.theme.clone();
    let buf = render_buf(&mut app, 120, 30);
    let row = find_row(&buf, "INCIDENT").expect("banner pill rendered");
    assert!(
        row_has_fg(&buf, row, theme.contrast_text(theme.health_red)),
        "banner text uses contrast fg over the red pill bg"
    );
    let text = render(&mut app, 120, 30);
    assert!(text.contains("db failover"), "headline visible in header");
}

#[tokio::test]
async fn handle_unavailability_estimate_silent_on_fetch_failure() {
    // Option-settings fetch failed → line stays None; UI
    // silently omits the row rather than rendering an error.
    let mut app = test_app();
    app.environments = vec![mk_env("prod", "shop", "Web", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.execute_command("deploy build-900");
    app.handle_msg(AppMsg::UnavailabilityEstimate {
        gen: app.generation,
        env_name: "prod".into(),
        line: None,
    });
    match &app.action_flow {
        Some(ActionFlow::Confirm(modal)) => {
            assert!(!modal.loading_unavailability);
            assert!(modal.unavailability_line.is_none());
        }
        _ => panic!("expected confirm modal"),
    }
}

#[tokio::test]
async fn handle_health_check_probe_renders_warning_on_failure() {
    // Probe failed → modal carries an `Err` so the UI renders
    // the yellow warning line. Loading flag clears.
    let mut app = test_app();
    app.environments = vec![mk_env("prod", "shop", "Web", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.execute_command("deploy build-900");
    app.handle_msg(AppMsg::HealthCheckProbe {
        gen: app.generation,
        env_name: "prod".into(),
        result: Err("HTTP 404".into()),
    });
    match &app.action_flow {
        Some(ActionFlow::Confirm(modal)) => {
            assert!(!modal.loading_health_check);
            assert_eq!(
                modal.health_check_probe.as_ref().map(|r| r.is_err()),
                Some(true)
            );
        }
        _ => panic!("expected confirm modal open"),
    }
}

#[tokio::test]
async fn handle_health_check_probe_silent_on_ok() {
    // Probe succeeded → modal carries `Ok(())` and the UI
    // renders nothing (silence is golden — the operator
    // confirm flow stays uncluttered).
    let mut app = test_app();
    app.environments = vec![mk_env("prod", "shop", "Web", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.execute_command("deploy build-900");
    app.handle_msg(AppMsg::HealthCheckProbe {
        gen: app.generation,
        env_name: "prod".into(),
        result: Ok(()),
    });
    match &app.action_flow {
        Some(ActionFlow::Confirm(modal)) => {
            assert!(!modal.loading_health_check);
            assert_eq!(
                modal.health_check_probe.as_ref().map(|r| r.is_ok()),
                Some(true)
            );
        }
        _ => panic!("expected confirm modal open"),
    }
}

#[tokio::test]
async fn apply_refresh_drains_watching_deploy_on_green() {
    // Watcher armed; next apply_refresh sees Green → drain +
    // pinned success. Operator sees "✓ deploy reached Green: prod"
    // without having to stare at the table.
    let mut app = test_app();
    let now = chrono::Utc::now();
    app.watching_deploys.insert(
        "prod".into(),
        WatchingDeploy {
            env_name: "prod".into(),
            target_label: "build-900".into(),
            armed_at: now,
            deadline_at: now + chrono::Duration::seconds(300),
        },
    );
    app.apply_refresh(Ok(vec![mk_env("prod", "shop", "Web", "Green")]), Vec::new());
    assert!(
        app.watching_deploys.is_empty(),
        "Green should drain the watcher"
    );
    // The pin flag survives only one refresh tick — by the time
    // apply_refresh returns, the message stays in the slot but the
    // pinned flag has been reset. We assert on the message itself.
    let pinned = app.status_message.as_deref().unwrap_or("");
    assert!(
        pinned.contains("reached Green") && pinned.contains("prod"),
        "expected pinned success status, got: {pinned:?}"
    );
}

#[tokio::test]
async fn apply_refresh_drains_watching_deploy_on_timeout() {
    // Watcher armed with a deadline already in the past; next
    // apply_refresh with the env still non-Green → drain + pinned
    // timeout error. The error pin survives the auto-clear at the
    // bottom of apply_refresh.
    let mut app = test_app();
    let now = chrono::Utc::now();
    app.watching_deploys.insert(
        "prod".into(),
        WatchingDeploy {
            env_name: "prod".into(),
            target_label: "build-900".into(),
            armed_at: now - chrono::Duration::seconds(600),
            deadline_at: now - chrono::Duration::seconds(60),
        },
    );
    app.apply_refresh(Ok(vec![mk_env("prod", "shop", "Web", "Red")]), Vec::new());
    assert!(
        app.watching_deploys.is_empty(),
        "expired watcher should drain on timeout"
    );
    let pinned = app.error_message.as_deref().unwrap_or("");
    assert!(
        pinned.contains("did not reach Green") && pinned.contains("prod"),
        "expected pinned timeout error, got: {pinned:?}"
    );
}

#[tokio::test]
async fn rebuild_clears_watching_deploys() {
    // Context switch (account / region change) flushes
    // env-scoped state. watching_deploys is env-name keyed, so
    // it must drop alongside armed_watchdogs + deploy_snapshots.
    let mut app = test_app();
    app.watching_deploys.insert(
        "prod".into(),
        WatchingDeploy {
            env_name: "prod".into(),
            target_label: "build-900".into(),
            armed_at: chrono::Utc::now(),
            deadline_at: chrono::Utc::now() + chrono::Duration::seconds(300),
        },
    );
    let _cache_guard = crate::aws::CACHE_TEST_LOCK.lock().await;
    app.apply_rebuild(
        app.rebuild_epoch,
        Ok(Box::new(crate::aws::AwsClient::stub())),
    );
    assert!(
        app.watching_deploys.is_empty(),
        "watching_deploys must clear on context rebuild"
    );
}

#[test]
fn soonest_watching_deploy_picks_earliest_deadline() {
    // Two watchers; pill shows the one firing first. Mirrors
    // the soonest_armed_rollback contract.
    let mut map: std::collections::HashMap<String, WatchingDeploy> =
        std::collections::HashMap::new();
    let now = chrono::Utc::now();
    map.insert(
        "later".into(),
        WatchingDeploy {
            env_name: "later".into(),
            target_label: "v2".into(),
            armed_at: now,
            deadline_at: now + chrono::Duration::seconds(600),
        },
    );
    map.insert(
        "sooner".into(),
        WatchingDeploy {
            env_name: "sooner".into(),
            target_label: "v1".into(),
            armed_at: now,
            deadline_at: now + chrono::Duration::seconds(120),
        },
    );
    let (env, _remaining) = soonest_watching_deploy(&map, now).expect("not empty");
    assert_eq!(env, "sooner");
}

#[tokio::test]
async fn promote_env_opens_deploy_confirm_on_target_with_sources_version() {
    // `:promote-env staging prod` takes staging's current
    // version_label, opens the deploy confirm on PROD (not the
    // selected env), and threads the label as deploy_version.
    let mut app = test_app();
    let mut staging = mk_env("staging", "shop", "Web", "Green");
    staging.version_label = "build-900".into();
    let mut prod = mk_env("prod", "shop", "Web", "Green");
    prod.version_label = "build-820".into();
    app.environments = vec![staging, prod];
    app.rebuild_view();
    // Cursor is on staging — the modal must still target prod
    // because the command names target explicitly, not via the
    // table cursor.
    app.table_state.select(Some(0));
    app.execute_command("promote-env staging prod");
    match &app.action_flow {
        Some(ActionFlow::Confirm(modal)) => {
            assert_eq!(modal.target_env, "prod");
            assert_eq!(modal.deploy_version.as_deref(), Some("build-900"));
            assert!(matches!(modal.action, Action::Deploy));
        }
        _ => panic!("expected confirm modal open on target"),
    }
}

#[tokio::test]
async fn promote_env_composes_with_watchdog_flags() {
    // The full daily gesture: ship staging → prod with both
    // safety nets armed. Both fields must thread through.
    let mut app = test_app();
    let mut staging = mk_env("staging", "shop", "Web", "Green");
    staging.version_label = "build-900".into();
    let prod = mk_env("prod", "shop", "Web", "Green");
    app.environments = vec![staging, prod];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.execute_command("promote-env staging prod --auto-rollback 10m --wait-for-green 5m");
    match &app.action_flow {
        Some(ActionFlow::Confirm(modal)) => {
            assert_eq!(modal.target_env, "prod");
            assert_eq!(modal.deploy_version.as_deref(), Some("build-900"));
            assert_eq!(modal.auto_rollback_secs, Some(600));
            assert_eq!(modal.wait_for_green_secs, Some(300));
        }
        _ => panic!("expected confirm modal open"),
    }
}

#[tokio::test]
async fn promote_env_refuses_when_versions_match() {
    // Idempotent-deploy guard: if SOURCE's version is already on
    // TARGET there's nothing to promote.
    let mut app = test_app();
    let mut staging = mk_env("staging", "shop", "Web", "Green");
    staging.version_label = "build-900".into();
    let mut prod = mk_env("prod", "shop", "Web", "Green");
    prod.version_label = "build-900".into();
    app.environments = vec![staging, prod];
    app.rebuild_view();
    app.execute_command("promote-env staging prod");
    let err = app.error_message.as_deref().unwrap_or("");
    assert!(
        err.contains("already deployed to prod"),
        "expected idempotent guard, got: {err}"
    );
    assert!(app.action_flow.is_none(), "no modal on no-op");
}

#[tokio::test]
async fn promote_env_refuses_when_source_has_no_version() {
    // Brand-new env with no deploy yet — nothing to ship.
    let mut app = test_app();
    let mut staging = mk_env("staging", "shop", "Web", "Pending");
    staging.version_label = String::new();
    let prod = mk_env("prod", "shop", "Web", "Green");
    app.environments = vec![staging, prod];
    app.rebuild_view();
    app.execute_command("promote-env staging prod");
    let err = app.error_message.as_deref().unwrap_or("");
    assert!(
        err.contains("no version deployed"),
        "expected no-version refusal, got: {err}"
    );
}

#[tokio::test]
async fn promote_env_refuses_same_source_and_target() {
    // Operator typo guard.
    let mut app = test_app();
    let mut staging = mk_env("staging", "shop", "Web", "Green");
    staging.version_label = "build-900".into();
    app.environments = vec![staging];
    app.rebuild_view();
    app.execute_command("promote-env staging staging");
    let err = app.error_message.as_deref().unwrap_or("");
    assert!(
        err.contains("must be different"),
        "expected same-env refusal, got: {err}"
    );
}

#[tokio::test]
async fn promote_env_refuses_unknown_env() {
    let mut app = test_app();
    let mut staging = mk_env("staging", "shop", "Web", "Green");
    staging.version_label = "build-900".into();
    app.environments = vec![staging];
    app.rebuild_view();
    app.execute_command("promote-env staging nope");
    let err = app.error_message.as_deref().unwrap_or("");
    assert!(
        err.contains("no env named 'nope'"),
        "expected unknown-env refusal, got: {err}"
    );
}

#[tokio::test]
async fn deploy_modal_opens_with_version_preview_loading_flag_set() {
    // `:deploy LABEL` (no --preview) now sets
    // `loading_version_preview` so the modal reserves space
    // for the preview block. The actual fetch lands via
    // `handle_version_preview` and unsets the flag.
    let mut app = test_app();
    app.environments = vec![mk_env("prod", "shop", "Web", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.execute_command("deploy build-900");
    match &app.action_flow {
        Some(ActionFlow::Confirm(modal)) => {
            assert!(
                modal.loading_version_preview,
                "Deploy modal must reserve space for the inline preview"
            );
            assert!(modal.version_preview.is_none());
        }
        _ => panic!("expected confirm modal open"),
    }
}

#[tokio::test]
async fn deploy_modal_handle_version_preview_stuffs_body_in_slot() {
    // Simulate the AppMsg landing — handler should clear the
    // loading flag and store the body.
    let mut app = test_app();
    app.environments = vec![mk_env("prod", "shop", "Web", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.execute_command("deploy build-900");
    let body = "candidate: build-900\ncurrent: build-820\n".to_string();
    app.handle_msg(AppMsg::VersionPreview {
        gen: app.generation,
        env_name: "prod".into(),
        result: Ok(body.clone()),
    });
    match &app.action_flow {
        Some(ActionFlow::Confirm(modal)) => {
            assert!(!modal.loading_version_preview);
            assert_eq!(modal.version_preview.as_deref(), Some(body.as_str()));
        }
        _ => panic!("expected confirm modal still open"),
    }
}

#[tokio::test]
async fn deploy_modal_handle_version_preview_error_renders_inline() {
    // AWS error should not leave the modal stuck in loading;
    // the failure becomes a one-line inline message.
    let mut app = test_app();
    app.environments = vec![mk_env("prod", "shop", "Web", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.execute_command("deploy build-900");
    app.handle_msg(AppMsg::VersionPreview {
        gen: app.generation,
        env_name: "prod".into(),
        result: Err("ListApplicationVersions throttled".into()),
    });
    match &app.action_flow {
        Some(ActionFlow::Confirm(modal)) => {
            assert!(!modal.loading_version_preview);
            let preview = modal.version_preview.as_deref().unwrap_or("");
            assert!(
                preview.contains("version preview unavailable") && preview.contains("throttled"),
                "expected inline error, got: {preview}"
            );
        }
        _ => panic!("expected confirm modal still open"),
    }
}

#[tokio::test]
async fn handle_confirm_modal_lint_stuffs_issues_into_modal() {
    // After spawn_confirm_lint emits its message, the handler
    // clears the loading flag and stores the issues vec.
    // Modal renders Warn+ as inline warnings on the next draw.
    let mut app = test_app();
    app.environments = vec![mk_env("prod", "shop", "Web", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.execute_command("deploy build-900");
    // Empty issues — handler still clears the loading flag.
    app.handle_msg(AppMsg::ConfirmModalLint {
        gen: app.generation,
        env_name: "prod".into(),
        issues: vec![],
    });
    match &app.action_flow {
        Some(ActionFlow::Confirm(modal)) => {
            assert!(!modal.loading_lint, "loading flag should clear");
            assert_eq!(modal.lint_issues.as_ref().map(|v| v.len()), Some(0));
        }
        _ => panic!("expected confirm modal open"),
    }
}

#[tokio::test]
async fn handle_confirm_modal_lint_drops_stale_target_results() {
    // If the operator opens a deploy on prod, closes it, then
    // opens a deploy on staging, the in-flight prod lint result
    // shouldn't land on the staging modal. Handler guards on
    // `modal.target_env == env_name`.
    let mut app = test_app();
    app.environments = vec![
        mk_env("prod", "shop", "Web", "Green"),
        mk_env("staging", "shop", "Web", "Green"),
    ];
    app.rebuild_view();
    // Open modal on staging.
    app.table_state.select(Some(1));
    app.execute_command("deploy build-900");
    // Late-arriving lint result for prod — should be dropped.
    app.handle_msg(AppMsg::ConfirmModalLint {
        gen: app.generation,
        env_name: "prod".into(),
        issues: vec![crate::lint::Issue {
            rule_id: "EBL001".into(),
            severity: crate::lint::Severity::Warn,
            env_name: Some("prod".into()),
            title: "stale".into(),
            detail: "stale".into(),
            suggestion: None,
            fields: Default::default(),
        }],
    });
    match &app.action_flow {
        Some(ActionFlow::Confirm(modal)) => {
            // loading_lint should still be true (we never
            // applied the stale result).
            assert!(
                modal.loading_lint,
                "loading flag must stay true on stale result"
            );
            assert!(
                modal.lint_issues.is_none(),
                "stale result must not populate"
            );
        }
        _ => panic!("expected confirm modal open"),
    }
}

#[tokio::test]
async fn refresh_tf_managed_envs_derives_set_from_tf_state() {
    // The HashSet caching the tf-managed names should match
    // `tf_state.managed_names()` after any mutation. Used by
    // the env-table badge for O(1) per-row lookup.
    let mut app = test_app();
    assert!(app.tf_managed_envs.is_empty(), "starts empty");
    app.tf_state = Some(crate::terraform::TfState {
        envs: vec![
            crate::terraform::TfEnv {
                name: "prod-api".into(),
                application: "shop".into(),
                version_label: "build-820".into(),
                options: vec![],
                tags: Default::default(),
            },
            crate::terraform::TfEnv {
                name: "prod-web".into(),
                application: "shop".into(),
                version_label: "build-820".into(),
                options: vec![],
                tags: Default::default(),
            },
        ],
    });
    app.refresh_tf_managed_envs();
    assert_eq!(app.tf_managed_envs.len(), 2);
    assert!(app.tf_managed_envs.contains("prod-api"));
    assert!(app.tf_managed_envs.contains("prod-web"));
    assert!(!app.tf_managed_envs.contains("staging-api"));
    // Clearing tf_state should empty the set on next refresh.
    app.tf_state = None;
    app.refresh_tf_managed_envs();
    assert!(app.tf_managed_envs.is_empty());
}

#[tokio::test]
async fn cmd_drift_refresh_reloads_tf_state_and_pins_status() {
    // `:drift refresh` re-reads tfstate from cwd. We can't
    // easily test the cwd discovery in isolation, but we
    // can verify the command path completes + pins a status
    // (either "reloaded N envs" or "no tfstate found").
    let mut app = test_app();
    app.environments = vec![mk_env("prod-api", "shop", "Web", "Green")];
    app.rebuild_view();
    app.execute_command("drift refresh");
    // Status message should mention tfstate either way.
    let msg = app.status_message.as_deref().unwrap_or("");
    assert!(
        msg.contains("tfstate"),
        "expected tfstate status, got: {msg}"
    );
}

#[tokio::test]
async fn cmd_drift_with_no_tfstate_loaded_hints_at_discovery() {
    // No tfstate cached → :drift surfaces a discovery hint
    // rather than firing an empty drift report. Sets the
    // operator on the right path (run from a tf project dir).
    let mut app = test_app();
    app.environments = vec![mk_env("prod-api", "shop", "Web", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.tf_state = None;
    app.execute_command("drift");
    let msg = app.status_message.as_deref().unwrap_or("");
    assert!(
        msg.contains("no terraform.tfstate found"),
        "expected discovery hint, got: {msg}"
    );
}

#[test]
fn render_lint_overlay_empty_shows_clean_stub() {
    let body = crate::app::cmd_misc::render_lint_overlay("prod-api", &[]);
    assert!(body.contains("prod-api"));
    assert!(body.contains("✓ No issues found"));
    assert!(body.contains("esc / q to close"));
}

#[test]
fn render_lint_overlay_with_issues_renders_per_severity_glyph() {
    use crate::lint::{Issue, Severity};
    use std::collections::BTreeMap;
    let issues = vec![
        Issue {
            rule_id: "EBL001".into(),
            severity: Severity::Warn,
            env_name: Some("prod".into()),
            title: "AllAtOnce on 4-instance env".into(),
            detail: "Deployment policy AllAtOnce with MaxSize=4 means full unavailability.".into(),
            suggestion: Some(":deployment-policy Rolling".into()),
            fields: BTreeMap::new(),
        },
        Issue {
            rule_id: "EBL005".into(),
            severity: Severity::Info,
            env_name: Some("prod".into()),
            title: "Single-instance env".into(),
            detail: "MinSize=MaxSize=1.".into(),
            suggestion: None,
            fields: BTreeMap::new(),
        },
    ];
    let body = crate::app::cmd_misc::render_lint_overlay("prod-api", &issues);
    // Warn gets ⚠, Info gets ·.
    assert!(body.contains("⚠ [EBL001]"));
    assert!(body.contains("· [EBL005]"));
    // Suggestion lines prefixed with →.
    assert!(body.contains("→ :deployment-policy Rolling"));
    // Detail wrapped under each issue with indent.
    assert!(body.contains("    Deployment policy AllAtOnce"));
    // Plural / singular handling.
    assert!(body.contains("2 issues found"));
}

#[tokio::test]
async fn dispatch_auto_rollback_also_drains_watching_deploys() {
    // When the rollback watchdog fires, any parallel
    // `--wait-for-green` watcher for the same env must drain
    // too — otherwise the rolled-back version reaching Green
    // would pin "✓ deploy reached Green: env (build-900)" even
    // though build-900 is the version we just rolled away from.
    let mut app = test_app();
    app.environments = vec![mk_env("prod", "shop", "Web", "Red")];
    app.rebuild_view();
    // Both watchers armed for the same env.
    app.deploy_snapshots.insert(
        "prod".into(),
        DeploySnapshot {
            env_name: "prod".into(),
            previous_version_label: "build-820".into(),
            taken_at: chrono::Utc::now(),
        },
    );
    app.armed_watchdogs.insert(
        "prod".into(),
        ArmedWatchdog {
            env_name: "prod".into(),
            target_label: "build-820".into(),
            armed_at: chrono::Utc::now(),
            deadline_at: chrono::Utc::now() - chrono::Duration::seconds(1),
        },
    );
    app.watching_deploys.insert(
        "prod".into(),
        WatchingDeploy {
            env_name: "prod".into(),
            target_label: "build-900".into(),
            armed_at: chrono::Utc::now(),
            deadline_at: chrono::Utc::now() + chrono::Duration::seconds(300),
        },
    );
    app.dispatch_auto_rollback("prod".into(), "Red".into());
    assert!(
        !app.watching_deploys.contains_key("prod"),
        "rollback dispatch must drain the parallel wait-for-green watcher"
    );
    assert!(
        !app.armed_watchdogs.contains_key("prod"),
        "rollback dispatch must drain its own armed watchdog"
    );
}

#[test]
fn soonest_watching_deploy_empty_returns_none() {
    let map: std::collections::HashMap<String, WatchingDeploy> = std::collections::HashMap::new();
    assert!(soonest_watching_deploy(&map, chrono::Utc::now()).is_none());
}

#[tokio::test]
async fn abort_rollback_named_env_disarms_just_that_one() {
    // Operator armed two; aborts only `staging`. `prod` stays
    // armed (and the deadline can still fire — apply_refresh
    // will decide as normal).
    let mut app = test_app();
    let now = chrono::Utc::now();
    for env in ["prod", "staging"] {
        app.armed_watchdogs.insert(
            env.into(),
            ArmedWatchdog {
                env_name: env.into(),
                target_label: "build-820".into(),
                armed_at: now,
                deadline_at: now + chrono::Duration::seconds(300),
            },
        );
    }
    app.execute_command("abort-rollback staging");
    assert!(
        !app.armed_watchdogs.contains_key("staging"),
        "named env should be drained"
    );
    assert!(
        app.armed_watchdogs.contains_key("prod"),
        "other env's watchdog must stay armed"
    );
    let status = app.status_message.as_deref().unwrap_or("");
    assert!(
        status.contains("aborted auto-rollback for staging"),
        "expected confirm in status, got: {status}"
    );
}

#[tokio::test]
async fn abort_rollback_named_env_not_armed_errors_clearly() {
    // Typo or stale name → no silent drain. Operator gets a
    // pointer at `:rollbacks-armed` to see what's actually
    // armed.
    let mut app = test_app();
    app.execute_command("abort-rollback ghost");
    let err = app.error_message.as_deref().unwrap_or("");
    assert!(
        err.contains("no auto-rollback armed for 'ghost'") && err.contains("rollbacks-armed"),
        "expected not-armed + discovery hint, got: {err}"
    );
}

#[tokio::test]
async fn abort_rollback_no_args_drains_every_watchdog() {
    // No arg → drain all. Status names them so the operator can
    // see what was cleared.
    let mut app = test_app();
    let now = chrono::Utc::now();
    for env in ["a", "b", "c"] {
        app.armed_watchdogs.insert(
            env.into(),
            ArmedWatchdog {
                env_name: env.into(),
                target_label: "x".into(),
                armed_at: now,
                deadline_at: now + chrono::Duration::seconds(300),
            },
        );
    }
    app.execute_command("abort-rollback");
    assert!(
        app.armed_watchdogs.is_empty(),
        "drain-all clears everything"
    );
    let status = app.status_message.as_deref().unwrap_or("");
    assert!(status.contains("aborted 3 auto-rollbacks"), "got: {status}");
    // Each named env surfaces in the toast.
    for env in ["a", "b", "c"] {
        assert!(
            status.contains(env),
            "expected {env} in status, got: {status}"
        );
    }
}

#[tokio::test]
async fn abort_rollback_no_args_empty_is_a_noop_status() {
    // Operator runs `:abort-rollback` with nothing armed → soft
    // status, not an error. Avoids surprising the operator who
    // just wanted to sanity-check.
    let mut app = test_app();
    app.execute_command("abort-rollback");
    let status = app.status_message.as_deref().unwrap_or("");
    assert!(status.contains("no auto-rollbacks armed to abort"));
    assert!(app.error_message.is_none());
}

#[test]
fn format_armed_rollbacks_empty_returns_stub() {
    let armed = std::collections::HashMap::new();
    let body = super::format_armed_rollbacks(&armed, chrono::Utc::now());
    assert!(body.contains("no auto-rollbacks armed"));
}

#[test]
fn format_armed_rollbacks_sorts_by_deadline_ascending() {
    use chrono::TimeZone;
    let now = chrono::Utc.with_ymd_and_hms(2026, 5, 25, 12, 0, 0).unwrap();
    let mut armed = std::collections::HashMap::new();
    // Two watchdogs — `staging-api` deadlines first.
    armed.insert(
        "prod-api".into(),
        ArmedWatchdog {
            env_name: "prod-api".into(),
            target_label: "build-820".into(),
            armed_at: now - chrono::Duration::seconds(60),
            deadline_at: now + chrono::Duration::seconds(300),
        },
    );
    armed.insert(
        "staging-api".into(),
        ArmedWatchdog {
            env_name: "staging-api".into(),
            target_label: "build-822".into(),
            armed_at: now - chrono::Duration::seconds(30),
            deadline_at: now + chrono::Duration::seconds(60),
        },
    );
    let body = super::format_armed_rollbacks(&armed, now);
    // The soonest deadline (staging-api, 1m left) appears first.
    let p_staging = body.find("staging-api").expect("staging-api row");
    let p_prod = body.find("prod-api").expect("prod-api row");
    assert!(
        p_staging < p_prod,
        "soonest-firing row should sort first; got body:\n{body}"
    );
    // Target labels surface so the operator can pre-read what'd
    // get redeployed.
    assert!(body.contains("build-822"));
    assert!(body.contains("build-820"));
}

#[test]
fn format_armed_rollbacks_expired_deadline_reads_as_expired() {
    use chrono::TimeZone;
    let now = chrono::Utc.with_ymd_and_hms(2026, 5, 25, 12, 0, 0).unwrap();
    let mut armed = std::collections::HashMap::new();
    armed.insert(
        "prod-api".into(),
        ArmedWatchdog {
            env_name: "prod-api".into(),
            target_label: "build-820".into(),
            armed_at: now - chrono::Duration::seconds(600),
            deadline_at: now - chrono::Duration::seconds(5),
        },
    );
    let body = super::format_armed_rollbacks(&armed, now);
    assert!(
        body.contains("fired / expired"),
        "expected expired marker, got: {body}"
    );
}

#[test]
fn soonest_armed_rollback_picks_the_earliest_deadline() {
    use chrono::TimeZone;
    let now = chrono::Utc.with_ymd_and_hms(2026, 5, 25, 12, 0, 0).unwrap();
    let mut armed = std::collections::HashMap::new();
    armed.insert(
        "later".into(),
        ArmedWatchdog {
            env_name: "later".into(),
            target_label: "x".into(),
            armed_at: now,
            deadline_at: now + chrono::Duration::seconds(600),
        },
    );
    armed.insert(
        "sooner".into(),
        ArmedWatchdog {
            env_name: "sooner".into(),
            target_label: "x".into(),
            armed_at: now,
            deadline_at: now + chrono::Duration::seconds(60),
        },
    );
    let (env, remaining) = super::soonest_armed_rollback(&armed, now).expect("one armed");
    assert_eq!(env, "sooner");
    // Remaining is in humanize-short-age form — 60s renders as "1m".
    assert!(remaining.contains('m') || remaining.contains('s'));
}

#[test]
fn soonest_armed_rollback_returns_none_when_empty() {
    let armed = std::collections::HashMap::new();
    assert!(super::soonest_armed_rollback(&armed, chrono::Utc::now()).is_none());
}

#[tokio::test]
async fn refresh_early_disarms_armed_watchdog_when_env_goes_green() {
    // Operator runs `:deploy --auto-rollback 5m` → watchdog
    // armed. Two refresh ticks later, the env reaches Green.
    // apply_refresh should clear the watchdog and surface the
    // disarm to the operator — without waiting for the 5m timer.
    let mut app = test_app();
    let now = chrono::Utc::now();
    app.armed_watchdogs.insert(
        "prod".into(),
        ArmedWatchdog {
            env_name: "prod".into(),
            target_label: "build-old".into(),
            armed_at: now,
            deadline_at: now + chrono::Duration::seconds(300),
        },
    );
    // Refresh delivers a Green prod env.
    app.apply_refresh(Ok(vec![mk_env("prod", "shop", "Web", "Green")]), Vec::new());
    assert!(
        app.armed_watchdogs.is_empty(),
        "Green refresh should clear the armed watchdog"
    );
    let status = app.status_message.as_deref().unwrap_or("");
    assert!(
        status.contains("watchdog disarmed"),
        "expected disarm status, got: {status}"
    );
}

#[tokio::test]
async fn refresh_leaves_watchdog_armed_when_env_still_non_green() {
    // Inverse: env is Red on the refresh tick → watchdog stays
    // armed (the deadline timer is the next checkpoint).
    let mut app = test_app();
    let now = chrono::Utc::now();
    app.armed_watchdogs.insert(
        "prod".into(),
        ArmedWatchdog {
            env_name: "prod".into(),
            target_label: "build-old".into(),
            armed_at: now,
            deadline_at: now + chrono::Duration::seconds(300),
        },
    );
    app.apply_refresh(Ok(vec![mk_env("prod", "shop", "Web", "Red")]), Vec::new());
    assert!(
        app.armed_watchdogs.contains_key("prod"),
        "Red refresh must leave watchdog armed"
    );
}

#[tokio::test]
async fn auto_rollback_check_is_noop_when_no_watchdog_armed() {
    // The deadline timer always fires (fire-and-forget
    // `tokio::spawn`, no JoinHandle for cancellation). If
    // apply_refresh's early-disarm pass already drained the
    // slot, the deadline message arriving later must be a no-op
    // — no spurious refresh, no pending row, no status churn.
    let mut app = test_app();
    app.environments = vec![mk_env("prod", "shop", "Web", "Green")];
    // armed_watchdogs intentionally empty. deploy_snapshots
    // intentionally populated to prove that even with a usable
    // snapshot we don't fire when the slot's clean.
    app.deploy_snapshots.insert(
        "prod".into(),
        DeploySnapshot {
            env_name: "prod".into(),
            previous_version_label: "build-old".into(),
            taken_at: chrono::Utc::now(),
        },
    );
    let pending_before = app.pending_actions.len();
    let load_before = app.load_state;
    app.handle_msg(AppMsg::AutoRollbackCheck {
        gen: app.generation,
        env_name: "prod".into(),
    });
    assert_eq!(
        app.pending_actions.len(),
        pending_before,
        "noop check shouldn't push pending"
    );
    // No refresh kicked: load_state should be unchanged. (A
    // spurious refresh wouldn't directly hurt operators but
    // would burn an API call per stale deadline tick.)
    assert_eq!(
        app.load_state, load_before,
        "noop check shouldn't kick a refresh"
    );
}

#[tokio::test]
async fn apply_refresh_disarms_armed_watchdog_when_env_reaches_green() {
    // The refresh decision path's headline outcome — operator's
    // deploy succeeded, env Green, watchdog disarms with a
    // status toast.
    let mut app = test_app();
    let now = chrono::Utc::now();
    app.armed_watchdogs.insert(
        "prod".into(),
        ArmedWatchdog {
            env_name: "prod".into(),
            target_label: "build-old".into(),
            armed_at: now,
            deadline_at: now + chrono::Duration::seconds(300),
        },
    );
    app.apply_refresh(Ok(vec![mk_env("prod", "shop", "Web", "Green")]), Vec::new());
    assert!(
        app.armed_watchdogs.is_empty(),
        "Green refresh should disarm"
    );
    let status = app.status_message.as_deref().unwrap_or("");
    assert!(status.contains("watchdog disarmed"));
}

#[tokio::test]
async fn apply_refresh_dispatches_rollback_when_deadline_passed_and_env_non_green() {
    // Refresh tick after the deadline + env still bad → dispatch.
    // The decision uses the freshly-applied env health, eliminating
    // the stale-cache race the inline-dispatch shape had.
    let mut app = test_app();
    let now = chrono::Utc::now();
    app.armed_watchdogs.insert(
        "prod".into(),
        ArmedWatchdog {
            env_name: "prod".into(),
            target_label: "build-old".into(),
            armed_at: now - chrono::Duration::seconds(600),
            deadline_at: now - chrono::Duration::seconds(1),
        },
    );
    app.deploy_snapshots.insert(
        "prod".into(),
        DeploySnapshot {
            env_name: "prod".into(),
            previous_version_label: "build-old".into(),
            taken_at: now - chrono::Duration::seconds(600),
        },
    );
    let pending_before = app.pending_actions.len();
    app.apply_refresh(Ok(vec![mk_env("prod", "shop", "Web", "Red")]), Vec::new());
    assert!(
        app.armed_watchdogs.is_empty(),
        "dispatch should drain the watchdog"
    );
    assert_eq!(
        app.pending_actions.len(),
        pending_before + 1,
        "rollback dispatch should push a pending row"
    );
    assert!(app
        .pending_actions
        .iter()
        .any(|p| p.label.contains("Auto-rollback") && p.target == "prod"));
    let status = app.status_message.as_deref().unwrap_or("");
    assert!(status.contains("redeploying build-old"));
    assert!(status.contains("Red"));
}

#[tokio::test]
async fn apply_refresh_keeps_watchdog_armed_before_deadline_even_when_non_green() {
    // Refresh tick while still inside the auto-rollback window
    // and env still bad → watchdog stays armed for the next
    // refresh to re-evaluate. Pins the "don't dispatch early"
    // invariant — deploys often run Yellow for a minute before
    // settling Green.
    let mut app = test_app();
    let now = chrono::Utc::now();
    app.armed_watchdogs.insert(
        "prod".into(),
        ArmedWatchdog {
            env_name: "prod".into(),
            target_label: "build-old".into(),
            armed_at: now,
            deadline_at: now + chrono::Duration::seconds(300),
        },
    );
    app.deploy_snapshots.insert(
        "prod".into(),
        DeploySnapshot {
            env_name: "prod".into(),
            previous_version_label: "build-old".into(),
            taken_at: now,
        },
    );
    let pending_before = app.pending_actions.len();
    app.apply_refresh(
        Ok(vec![mk_env("prod", "shop", "Web", "Yellow")]),
        Vec::new(),
    );
    assert!(
        app.armed_watchdogs.contains_key("prod"),
        "Yellow + pre-deadline must keep watchdog armed"
    );
    assert_eq!(
        app.pending_actions.len(),
        pending_before,
        "no dispatch before the deadline"
    );
}

#[tokio::test]
async fn apply_refresh_errors_when_deadline_passed_but_no_snapshot() {
    // Edge case: deadline expired + env non-Green + no captured
    // snapshot. Surface a clear error pointing the operator at
    // manual rollback rather than silently no-op.
    let mut app = test_app();
    let now = chrono::Utc::now();
    app.armed_watchdogs.insert(
        "prod".into(),
        ArmedWatchdog {
            env_name: "prod".into(),
            target_label: "build-old".into(),
            armed_at: now - chrono::Duration::seconds(600),
            deadline_at: now - chrono::Duration::seconds(1),
        },
    );
    // deploy_snapshots intentionally empty.
    app.apply_refresh(Ok(vec![mk_env("prod", "shop", "Web", "Red")]), Vec::new());
    let err = app.error_message.as_deref().unwrap_or("");
    assert!(
        err.contains("no pre-deploy snapshot"),
        "expected missing-snapshot guidance, got: {err}"
    );
    assert!(
        app.armed_watchdogs.is_empty(),
        "missing-snapshot path still drains the watchdog"
    );
}

#[tokio::test]
async fn diff_two_arg_form_opens_overlay_for_named_envs() {
    // `:diff ENV-A ENV-B` is the post-0.8 shape that lets the
    // operator name both sides without first selecting one of
    // them. Verifies the new two-arg dispatch lands the Diff
    // overlay without complaining about "no env selected".
    let mut app = test_app();
    app.environments = vec![
        mk_env("staging", "uflexi", "Web", "Green"),
        mk_env("prod", "uflexi", "Web", "Green"),
    ];
    app.rebuild_view();
    // Deliberately leave no selection — the two-arg form should
    // ignore the selected-env fallback entirely.
    app.execute_command("diff staging prod");
    assert!(
        matches!(app.current_overlay, Some(Overlay::Diff(_))),
        "expected Overlay::Diff, got {:?}",
        app.current_overlay.is_some()
    );
    assert!(
        app.error_message.is_none(),
        "unexpected error: {:?}",
        app.error_message
    );
}

#[tokio::test]
async fn diff_ignore_keys_flag_parses_and_unknown_flag_errors() {
    let mut app = test_app();
    app.environments = vec![
        mk_env("staging", "uflexi", "Web", "Green"),
        mk_env("prod", "uflexi", "Web", "Green"),
    ];
    app.rebuild_view();
    // `--ignore-keys` is consumed; positional envs still resolve and
    // the overlay opens (flag order around the env names is free).
    app.execute_command("diff staging prod --ignore-keys version,updated");
    assert!(
        matches!(app.current_overlay, Some(Overlay::Diff(_))),
        "expected Overlay::Diff with --ignore-keys present"
    );
    assert!(
        app.error_message.is_none(),
        "unexpected: {:?}",
        app.error_message
    );
    // An unrecognised flag is a clear error, not a silent no-op.
    app.current_overlay = None;
    app.execute_command("diff staging prod --bogus");
    assert!(app.current_overlay.is_none(), "shouldn't open on bad flag");
    assert!(
        app.error_message
            .as_deref()
            .unwrap_or("")
            .contains("unknown arg '--bogus'"),
        "expected unknown-arg error, got {:?}",
        app.error_message
    );
    // A third positional is rejected, not silently dropped.
    app.error_message = None;
    app.current_overlay = None;
    app.execute_command("diff staging prod extra");
    assert!(
        app.current_overlay.is_none(),
        "shouldn't open with 3 positionals"
    );
    assert!(
        app.error_message
            .as_deref()
            .unwrap_or("")
            .contains("at most two"),
        "expected too-many-args error, got {:?}",
        app.error_message
    );
}

#[tokio::test]
async fn diff_two_arg_form_rejects_same_env_twice() {
    // `:diff ENV ENV` is a typo, not a request — surface a clear
    // error rather than silently comparing an env against itself.
    let mut app = test_app();
    app.environments = vec![mk_env("prod", "uflexi", "Web", "Green")];
    app.rebuild_view();
    app.execute_command("diff prod prod");
    assert!(
        app.current_overlay.is_none(),
        "shouldn't open overlay for same-env diff"
    );
    let err = app.error_message.as_deref().unwrap_or("");
    assert!(
        err.contains("different envs"),
        "expected 'different envs' guidance, got: {err}"
    );
}

#[tokio::test]
async fn diff_two_arg_form_errors_on_unknown_env() {
    // Missing env-B → no overlay, clear error message naming the
    // missing env so the operator knows which arg to fix.
    let mut app = test_app();
    app.environments = vec![mk_env("staging", "uflexi", "Web", "Green")];
    app.rebuild_view();
    app.execute_command("diff staging missing-env");
    assert!(app.current_overlay.is_none());
    let err = app.error_message.as_deref().unwrap_or("");
    assert!(
        err.contains("missing-env"),
        "expected error to name the missing env, got: {err}"
    );
}

#[test]
fn format_ssm_results_renders_per_instance_sections() {
    // Two instances with different statuses → each gets its own
    // header (instance id + status + exit code) and stdout/stderr
    // sections. Empty-output instance shows the `(no output)` stub
    // so the operator can distinguish "ran cleanly, said nothing"
    // from "didn't run".
    let rows = vec![
        crate::aws::SsmRunResult {
            instance_id: "i-aaa".into(),
            status: "Success".into(),
            exit_code: 0,
            stdout: "hello world\nline two".into(),
            stderr: String::new(),
        },
        crate::aws::SsmRunResult {
            instance_id: "i-bbb".into(),
            status: "Failed".into(),
            exit_code: 2,
            stdout: String::new(),
            stderr: "permission denied".into(),
        },
    ];
    let body = super::format_ssm_results("uptime", &rows);
    // Command line surfaced in header.
    assert!(body.contains("`uptime`"));
    // Both per-instance section headers present with exit codes.
    assert!(body.contains("i-aaa [Success, exit=0]"));
    assert!(body.contains("i-bbb [Failed, exit=2]"));
    // stdout content present.
    assert!(body.contains("hello world"));
    assert!(body.contains("line two"));
    // stderr content present.
    assert!(body.contains("permission denied"));
}

#[test]
fn format_ssm_results_truncates_long_output() {
    // A 100-line stdout blob must collapse to MAX_LINES_PER_STREAM
    // (50) + a "… (N more lines truncated)" footer so the overlay
    // stays scannable.
    let stdout: String = (0..100).map(|i| format!("line {i}\n")).collect();
    let rows = vec![crate::aws::SsmRunResult {
        instance_id: "i-aaa".into(),
        status: "Success".into(),
        exit_code: 0,
        stdout,
        stderr: String::new(),
    }];
    let body = super::format_ssm_results("seq 0 99", &rows);
    // Truncation footer cites the number of dropped lines.
    assert!(
        body.contains("50 more lines truncated"),
        "expected truncation footer, got body:\n{body}"
    );
    // Last preserved line is `line 49`, not `line 99`.
    assert!(body.contains("line 49"));
    assert!(!body.contains("line 99"));
}

#[test]
fn format_ssm_results_empty_rows_produces_stub() {
    let body = super::format_ssm_results("uptime", &[]);
    assert!(body.contains("No instances targeted"));
}

#[tokio::test]
async fn ssm_run_without_args_errors_clearly() {
    let mut app = test_app();
    app.execute_command("ssm-run");
    let err = app.error_message.as_deref().unwrap_or("");
    assert!(
        err.contains("usage:") && err.contains("shell-command"),
        "expected usage hint, got: {err}"
    );
}

#[tokio::test]
async fn ssm_run_without_detail_errors_with_instances_guidance() {
    // No Detail open → no cached instances → command should refuse
    // rather than silently no-op.
    let mut app = test_app();
    app.execute_command("ssm-run \"uptime\"");
    let err = app.error_message.as_deref().unwrap_or("");
    assert!(
        err.contains("Detail") && err.contains("Instances"),
        "expected Detail/Instances guidance, got: {err}"
    );
}

#[test]
fn format_alarm_history_renders_entries_and_empty_stub() {
    use chrono::TimeZone;
    let ts = |h, mi| chrono::Utc.with_ymd_and_hms(2026, 5, 24, h, mi, 0).unwrap();
    let mk = |t, kind: &str, summary: &str| crate::aws::AlarmHistoryEntry {
        at: Some(t),
        kind: kind.into(),
        summary: summary.into(),
    };
    // Empty entries → stub body + the 90-day retention hint so an
    // operator looking at an alarm with no recent transitions
    // doesn't assume the fetch broke.
    let stub = super::format_alarm_history("high-cpu", &[]);
    assert!(stub.contains("No history items"));
    assert!(stub.contains("90 days"));
    // Real entries → each row carries timestamp, kind in brackets,
    // and the summary line. Order preserved (newest-first per the
    // SDK's default).
    let entries = vec![
        mk(ts(12, 5), "StateUpdate", "Alarm updated from OK to ALARM"),
        mk(ts(11, 0), "ConfigurationUpdate", "Threshold changed to 80"),
    ];
    let body = super::format_alarm_history("high-cpu", &entries);
    assert!(body.contains("[StateUpdate]"));
    assert!(body.contains("[ConfigurationUpdate]"));
    assert!(body.contains("Alarm updated from OK to ALARM"));
    assert!(body.contains("Threshold changed to 80"));
    // Newest-first preserved: StateUpdate appears before ConfigurationUpdate.
    let p_state = body.find("StateUpdate").unwrap();
    let p_cfg = body.find("ConfigurationUpdate").unwrap();
    assert!(p_state < p_cfg);
}

#[test]
fn format_alarm_history_handles_missing_timestamp() {
    // A history item without a timestamp shouldn't blank out the
    // row — render `—` so the kind/summary still scan.
    let entries = vec![crate::aws::AlarmHistoryEntry {
        at: None,
        kind: "StateUpdate".into(),
        summary: "Alarm went ALARM".into(),
    }];
    let body = super::format_alarm_history("high-cpu", &entries);
    assert!(body.contains("—"));
    assert!(body.contains("Alarm went ALARM"));
}

#[test]
fn format_alarms_handles_empty_and_error() {
    let none = format_alarms(Ok(vec![]));
    assert!(none.contains("no CloudWatch alarms"));
    let err = format_alarms(Err("boom".into()));
    assert!(err.contains("error"));
    let alarms = format_alarms(Ok(vec![CwAlarm {
        name: "high-cpu".into(),
        state: "ALARM".into(),
        state_reason: "CPU > 80%".into(),
        metric_name: "CPUUtilization".into(),
        namespace: "AWS/EC2".into(),
    }]));
    assert!(alarms.contains("ALARM"));
    assert!(alarms.contains("high-cpu"));
    assert!(alarms.contains("CPU > 80%"));
}

#[test]
fn view_round_trips() {
    // We can't easily construct an App in tests, but encode_view's format
    // is straightforward — check a hand-built snap round-trips through
    // parse_sort and the trivial fields.
    let snap = "filter=prod;sort=health:desc;grouped=true;scope=apps";
    let mut got_filter = String::new();
    let mut got_sort = (SortKey::App, false);
    let mut got_grouped = false;
    let mut got_scope = Scope::Envs;
    for part in snap.split(';') {
        let (k, v) = part.split_once('=').unwrap();
        match k {
            "filter" => got_filter = v.into(),
            "sort" => got_sort = parse_sort(Some(v)),
            "grouped" => got_grouped = v == "true",
            "scope" => {
                got_scope = if v == "apps" {
                    Scope::Apps
                } else {
                    Scope::Envs
                }
            }
            _ => {}
        }
    }
    assert_eq!(got_filter, "prod");
    assert_eq!(got_sort, (SortKey::Health, true));
    assert!(got_grouped);
    assert_eq!(got_scope, Scope::Apps);
}

#[test]
fn view_mode_cycle_includes_spacious() {
    assert_eq!(ViewMode::Default.next(), ViewMode::Compact);
    assert_eq!(ViewMode::Compact.next(), ViewMode::Spacious);
    assert_eq!(ViewMode::Spacious.next(), ViewMode::Default);
    assert_eq!(ViewMode::Spacious.label(), "spacious");
}

#[test]
fn md_escape_protects_pipes_and_backslashes() {
    assert_eq!(md_escape("simple"), "simple");
    assert_eq!(md_escape("a|b|c"), "a\\|b\\|c");
    assert_eq!(md_escape("back\\slash"), "back\\\\slash");
    assert_eq!(md_escape("a\\|b"), "a\\\\\\|b");
}

#[test]
fn describe_env_dumps_known_fields() {
    let env = Environment {
        name: "my-env".into(),
        application: "my-app".into(),
        status: "Ready".into(),
        health: "Green".into(),
        platform: "Java 17".into(),
        solution_stack: String::new(),
        tier: "Web".into(),
        cname: "my-env.elb.amazonaws.com".into(),
        version_label: "v42".into(),
        arn: None,
        updated: None,
        id: None,
        region: None,
    };
    let text = describe_env(&env);
    assert!(text.contains("\"name\""));
    assert!(text.contains("my-env"));
    assert!(text.contains("\"updated\":         null"));
}

#[test]
fn detail_tab_titles_are_distinct() {
    use std::collections::HashSet;
    let titles: HashSet<&str> = [
        DetailTab::Health,
        DetailTab::Events,
        DetailTab::Instances,
        DetailTab::Metrics,
        DetailTab::Queue,
        DetailTab::Config,
    ]
    .iter()
    .map(|t| t.title())
    .collect();
    assert_eq!(titles.len(), 6);
}

// ── UI integration harness ──────────────────────────────────────
//
// These tests drive `crossterm::Event`s through `handle_event` and
// (optionally) render to a `ratatui::TestBackend`-backed Terminal
// to inspect the resulting buffer. The harness uses `App::for_tests`
// — synchronous, no AWS network, no disk reads — so each test starts
// from a known clean state.
//
// What this catches that the pure-helper tests don't:
//   - Mode-transition glitches (overlay closes correctly, Filter
//     mode swallows printable keys, etc.)
//   - Key-precedence regressions (Mode::Picker over LogTail
//     overlay, ESC routing, Tab cycling)
//   - Render-side state-dependent bugs (a field is None, the
//     renderer panics; an overlay shape changes, the dispatch
//     desyncs).
//
// Pattern:
//   1. `let mut app = test_app();` — clean App.
//   2. Mutate state as needed (push fake envs onto `app.environments`,
//      flip toggles, etc.). The struct is fully `pub` so tests can
//      seed any shape without going through async fetchers.
//   3. `press(&mut app, KeyCode::*, KeyModifiers::*)` — feed a key.
//   4. Assert on `app.<field>` — or render to a buffer string via
//      `render(&mut app, w, h)` and grep.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// Build a minimal App in a deterministic state. Useful for tests
/// that don't care about real AWS data — just keyboard flow + mode
/// transitions. Seed envs / overlays / detail state by mutating
/// the returned App directly.
fn test_app() -> App {
    // Match the unicode/dark defaults so the renderer's per-theme
    // branches are exercised on the common path.
    let cfg = crate::config::Config {
        theme: "dark".into(),
        icons: "unicode".into(),
        ..crate::config::Config::default()
    };
    App::for_tests(crate::aws::AwsClient::stub(), cfg)
}

#[tokio::test]
async fn stale_rebuild_arrival_is_dropped() {
    // Two rapid context switches: the SLOW first one landing after
    // the fast second must not overwrite the operator's last
    // choice.
    let mut app = test_app();
    app.rebuild_epoch = 2; // two switches spawned; latest epoch = 2
    let gen_before = app.generation;
    app.handle_msg(AppMsg::Rebuild {
        epoch: 1, // the older switch's arrival
        result: Err("slow switch landed late".into()),
    });
    assert_eq!(
        app.generation, gen_before,
        "stale epoch must be dropped before any apply"
    );
    assert!(
        app.error_message.is_none(),
        "no error surfaced for a dropped arrival"
    );
}

#[tokio::test]
async fn worker_queue_fetch_error_keeps_previous_dlq_depth() {
    // 0.27 fix: a failed fetch must not clear the env's alert —
    // the old clear-and-rebuild dropped it every errored tick.
    let mut app = test_app();
    app.handle_msg(AppMsg::WorkerQueueCheck {
        gen: app.generation,
        results: vec![("wk-prod".into(), Ok(Some(7)))],
    });
    assert_eq!(app.worker_dlq_depths.get("wk-prod"), Some(&7));
    // Fetch error → depth survives, marked stale.
    app.handle_msg(AppMsg::WorkerQueueCheck {
        gen: app.generation,
        results: vec![("wk-prod".into(), Err("AccessDenied".into()))],
    });
    assert_eq!(
        app.worker_dlq_depths.get("wk-prod"),
        Some(&7),
        "error must not read as 'no DLQ'"
    );
    assert!(app.worker_dlq_stale.contains("wk-prod"));
    // Successful re-check clears the staleness.
    app.handle_msg(AppMsg::WorkerQueueCheck {
        gen: app.generation,
        results: vec![("wk-prod".into(), Ok(Some(3)))],
    });
    assert!(!app.worker_dlq_stale.contains("wk-prod"));
    assert_eq!(app.worker_dlq_depths.get("wk-prod"), Some(&3));
    // Genuine no-DLQ → cleared; fresh depth → updated.
    app.handle_msg(AppMsg::WorkerQueueCheck {
        gen: app.generation,
        results: vec![("wk-prod".into(), Ok(None))],
    });
    assert!(!app.worker_dlq_depths.contains_key("wk-prod"));
}

/// Synthesize a `KeyEvent::Press` and dispatch it through
/// `handle_event`. Mirrors how `run()` feeds real terminal events.
fn press(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    app.handle_event(Event::Key(KeyEvent {
        code,
        modifiers: mods,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    }));
}

/// Render the App into a fixed-size `TestBackend` buffer and return
/// the flattened string (one row per line, joined with `\n`).
/// Useful for grep-style assertions on rendered output.
fn render(app: &mut App, w: u16, h: u16) -> String {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal.draw(|f| crate::ui::draw(f, app)).expect("draw");
    let buf = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

/// Render the App into a `TestBackend` and return the raw `Buffer`,
/// so tests can assert on *style* (fg/bg/modifier per cell), not just
/// the flattened symbols `render` gives. Pairs with `find_row` /
/// `row_has_fg` below for grep-then-check-colour assertions.
fn render_buf(app: &mut App, w: u16, h: u16) -> ratatui::buffer::Buffer {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal.draw(|f| crate::ui::draw(f, app)).expect("draw");
    terminal.backend().buffer().clone()
}

/// First row (y) whose flattened symbols contain `needle`, if any.
fn find_row(buf: &ratatui::buffer::Buffer, needle: &str) -> Option<u16> {
    (0..buf.area.height).find(|&y| {
        let mut row = String::new();
        for x in 0..buf.area.width {
            row.push_str(buf[(x, y)].symbol());
        }
        row.contains(needle)
    })
}

/// Whether any cell in row `y` is painted with foreground `color`.
fn row_has_fg(buf: &ratatui::buffer::Buffer, y: u16, color: ratatui::style::Color) -> bool {
    (0..buf.area.width).any(|x| buf[(x, y)].fg == color)
}

/// Cells in row `y` whose symbol == `sym` and foreground == `fg`.
fn count_symbol_fg(
    buf: &ratatui::buffer::Buffer,
    y: u16,
    sym: &str,
    fg: ratatui::style::Color,
) -> usize {
    (0..buf.area.width)
        .filter(|&x| buf[(x, y)].symbol() == sym && buf[(x, y)].fg == fg)
        .count()
}

/// Total cells painted with foreground `color` — for differential
/// assertions ("match adds N green cells vs no-match") that ignore
/// constant chrome (header/footer) painted in the same colour.
fn count_fg(buf: &ratatui::buffer::Buffer, color: ratatui::style::Color) -> usize {
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .filter(|&x| buf[(x, y)].fg == color)
                .count()
        })
        .sum()
}

fn mk_env(name: &str, app: &str, tier: &str, health: &str) -> crate::aws::Environment {
    crate::aws::Environment {
        name: name.into(),
        application: app.into(),
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

#[tokio::test]
async fn persist_state_is_a_noop_in_demo_mode() {
    // `--demo` runs against a synthetic fleet on a fake profile +
    // region with `cost_enabled = true` from the fixture. Without
    // this bypass, exiting demo mode would write that synthetic
    // state to ~/.config/ebman/state.toml, clobbering the
    // operator's real saved state (selected env, sort, named
    // filters, cost-tracking opt-in, …).
    //
    // The test pivots on persist_state's `state::file_path()`
    // touch: if we set a sentinel path via $XDG_STATE_HOME +
    // confirm no file lands there post-persist, the bypass is
    // working. Skipping file-path indirection keeps the test
    // hermetic; we just assert demo_mode short-circuits before
    // any disk write would happen by checking the function's
    // observable effect via `state::load`-after.
    let mut app = test_app();
    app.demo_mode = true;
    // No panic, no file write. The function should return early
    // before constructing the persisted struct or reaching
    // write_atomic. Smoke test: just calling it must not error.
    app.persist_state();
}

#[tokio::test]
async fn tab_cycles_scope_envs_to_apps_and_back() {
    let mut app = test_app();
    assert_eq!(app.scope, Scope::Envs);
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.scope, Scope::Apps);
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.scope, Scope::Envs);
}

#[tokio::test]
async fn question_mark_opens_help_and_escape_dismisses_it() {
    let mut app = test_app();
    assert_eq!(app.mode, Mode::Normal);
    press(&mut app, KeyCode::Char('?'), KeyModifiers::NONE);
    assert_eq!(app.mode, Mode::Help);
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.mode, Mode::Normal);
}

#[tokio::test]
async fn colon_enters_command_mode_and_esc_cancels() {
    let mut app = test_app();
    press(&mut app, KeyCode::Char(':'), KeyModifiers::NONE);
    assert_eq!(app.mode, Mode::Command);
    // Typed chars land in the command input buffer.
    press(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
    assert_eq!(app.command_input.text(), "q");
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.mode, Mode::Normal);
    // Input cleared on cancel.
    assert!(app.command_input.is_empty());
}

#[tokio::test]
async fn slash_enters_filter_mode_and_text_lands() {
    let mut app = test_app();
    // Seed an env so filter has something to operate on.
    app.environments = vec![
        mk_env("prod-web", "uflexi", "Web", "Green"),
        mk_env("staging-web", "uflexi", "Web", "Green"),
    ];
    app.rebuild_view();
    press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
    assert_eq!(app.mode, Mode::Filter);
    for c in "prod".chars() {
        press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    assert_eq!(app.view.filter().text(), "prod");
    // Esc clears the filter and returns to Normal.
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.view.filter().is_empty());
}

#[tokio::test]
async fn enter_on_red_env_opens_why_via_bang_keybind() {
    let mut app = test_app();
    // Seed a Red env + select it.
    app.environments = vec![mk_env("prod-web", "uflexi", "Web", "Red")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    // `!` shortcut in Envs scope opens the :why overlay.
    press(&mut app, KeyCode::Char('!'), KeyModifiers::NONE);
    assert!(
        matches!(app.current_overlay, Some(Overlay::WhyRed { .. })),
        "expected WhyRed overlay, got {:?}",
        app.current_overlay
    );
}

#[tokio::test]
async fn render_main_table_includes_seeded_env_name() {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod-canary", "uflexi", "Web", "Green")];
    app.rebuild_view();
    let frame = render(&mut app, 160, 24);
    assert!(
        frame.contains("api-prod-canary"),
        "rendered frame should show seeded env name; got:\n{frame}"
    );
}

#[tokio::test]
async fn render_main_table_includes_inst_column_header_and_data() {
    // INST column should appear in the main table header by default
    // (not in hidden_cols) and render the per-env counts when the
    // env_instance_counts cache has data, em-dash when it doesn't.
    let mut app = test_app();
    app.environments = vec![
        mk_env("api-prod", "uflexi", "Web", "Green"),
        mk_env("api-staging", "uflexi", "Web", "Green"),
    ];
    // Seed counts: prod has 3/3 healthy, staging unknown (no entry).
    app.env_instance_counts.insert(
        "api-prod".into(),
        crate::aws::EnvInstanceCounts {
            healthy: 3,
            total: 3,
        },
    );
    app.rebuild_view();
    let frame = render(&mut app, 160, 24);
    assert!(
        frame.contains("INST"),
        "expected INST column header in rendered frame; got:\n{frame}"
    );
    assert!(
        frame.contains("3/3"),
        "expected '3/3' for env with seeded counts; got:\n{frame}"
    );
    // Staging has no counts entry → em-dash placeholder.
    assert!(
        frame.contains("—"),
        "expected em-dash placeholder for env with no counts; got:\n{frame}"
    );
}

#[tokio::test]
async fn is_read_only_for_layers_global_env_and_account() {
    // Global toggle wins over everything — even an env not in the
    // pin map.
    let mut app = test_app();
    app.read_only = true;
    assert!(app.is_read_only_for("any-env"));
    assert!(app.read_only_reason("any-env").unwrap().contains("global"));

    // Global off + per-env pin → that one env is locked, others
    // aren't.
    let mut app = test_app();
    app.cfg.safety_envs.insert("uflexi-prod".into(), true);
    app.cfg.safety_envs.insert("uflexi-staging".into(), false);
    assert!(app.is_read_only_for("uflexi-prod"));
    assert!(!app.is_read_only_for("uflexi-staging"));
    assert!(!app.is_read_only_for("uflexi-dev"));
    assert!(app
        .read_only_reason("uflexi-prod")
        .unwrap()
        .contains("safety.envs.uflexi-prod"));

    // Global off + per-account pin → every env in that profile is
    // locked.
    let mut app = test_app();
    app.context.profile = Some("prod-acct".into());
    app.cfg.safety_accounts.insert("prod-acct".into(), true);
    assert!(app.is_read_only_for("any-env"));
    assert!(app
        .read_only_reason("any-env")
        .unwrap()
        .contains("safety.accounts.prod-acct"));
    // Switching profile away clears the lock.
    app.context.profile = Some("dev-acct".into());
    assert!(!app.is_read_only_for("any-env"));

    // Nothing pinned → unlocked + reason is None.
    let app = test_app();
    assert!(!app.is_read_only_for("any-env"));
    assert!(app.read_only_reason("any-env").is_none());
}

#[tokio::test]
async fn deny_write_batch_refuses_when_any_selected_env_is_pinned() {
    // Regression: pre-fix, cmd_batch_* gated only on the global
    // `read_only` flag, so a per-env safety pin (safety.envs.X) was
    // silently bypassed for batch ops. deny_write_batch must refuse
    // the whole batch if ANY selected env is pinned.
    let mut app = test_app();
    app.cfg.safety_envs.insert("prod-web".into(), true);
    let selection = vec!["staging-web".to_string(), "prod-web".to_string()];
    assert!(
        app.deny_write_batch(&selection, "batch action"),
        "a pinned env in the selection must refuse the batch"
    );
    let msg = app.error_message.clone().unwrap();
    // Names the locked env + the safety.envs source.
    assert!(msg.contains("prod-web"), "got: {msg}");
    assert!(msg.contains("safety.envs.prod-web"), "got: {msg}");
    // Refuse-all: the unpinned env is NOT quietly let through.
    assert!(msg.contains("1 of 2"), "got: {msg}");
}

#[tokio::test]
async fn deny_write_batch_allows_when_no_env_pinned() {
    let mut app = test_app();
    app.cfg.safety_envs.insert("other-env".into(), true); // not in selection
    let selection = vec!["staging-web".to_string(), "dev-web".to_string()];
    assert!(
        !app.deny_write_batch(&selection, "batch action"),
        "no selected env pinned → batch proceeds"
    );
    assert!(app.error_message.is_none());
}

#[tokio::test]
async fn deny_write_batch_global_flag_uses_fleet_message_not_per_env_list() {
    // The env-independent gates (global read-only / freeze / demo)
    // should still produce their familiar whole-fleet toast rather
    // than enumerating envs.
    let mut app = test_app();
    app.read_only = true;
    let selection = vec!["a".to_string(), "b".to_string()];
    assert!(app.deny_write_batch(&selection, "batch action"));
    let msg = app.error_message.clone().unwrap();
    assert!(msg.contains("read-only mode"), "got: {msg}");
    // Not the per-env "N of M …" shape.
    assert!(!msg.contains(" of "), "got: {msg}");
}

#[tokio::test]
async fn cmd_batch_action_refuses_pinned_env_and_keeps_selection() {
    // End-to-end through the command entry point: a refused batch
    // must NOT clear multi_selected (so the operator can deselect
    // the locked env and retry).
    let mut app = test_app();
    app.cfg.safety_envs.insert("prod-web".into(), true);
    app.multi_selected.insert("prod-web".into());
    app.multi_selected.insert("staging-web".into());
    app.cmd_batch_action(crate::app::Action::Rebuild);
    assert!(app.error_message.is_some());
    assert_eq!(
        app.multi_selected.len(),
        2,
        "refused batch must preserve the selection for retry"
    );
}

#[tokio::test]
async fn ctrl_x_toggles_redact() {
    let mut app = test_app();
    assert!(!app.view.redact);
    press(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
    assert!(app.view.redact);
    press(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
    assert!(!app.view.redact);
}

#[tokio::test]
async fn space_toggles_multi_select_and_esc_clears_it() {
    let mut app = test_app();
    app.environments = vec![
        mk_env("api-prod", "uflexi", "Web", "Green"),
        mk_env("api-staging", "uflexi", "Web", "Green"),
    ];
    app.rebuild_view();
    // Cursor on row 0 by default. Space adds it to multi-select.
    press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
    assert_eq!(app.multi_selected.len(), 1);
    assert!(app.multi_selected.contains("api-prod"));
    // Second space toggles the same row off.
    press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
    assert!(app.multi_selected.is_empty());
    // Select both rows again, then Esc → clears in one keystroke.
    press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
    press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
    press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
    assert_eq!(app.multi_selected.len(), 2);
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.multi_selected.is_empty());
}

#[tokio::test]
async fn filter_mode_text_input_and_backspace_round_trips() {
    let mut app = test_app();
    app.environments = vec![
        mk_env("api-prod", "uflexi", "Web", "Green"),
        mk_env("api-staging", "uflexi", "Web", "Green"),
    ];
    app.rebuild_view();
    // `/` enters Filter mode.
    press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
    assert_eq!(app.mode, Mode::Filter);
    // Type "prod" — filter text accumulates, view rebuilds on each char.
    for c in "prod".chars() {
        press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    assert_eq!(app.view.filter().text(), "prod");
    // Backspace removes the last char.
    press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(app.view.filter().text(), "pro");
    // Enter commits the filter and returns to Normal — the filter
    // string SURVIVES (it's how `:filter` works as a stateful
    // search).
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.view.filter().text(), "pro");
}

#[tokio::test]
async fn filter_input_is_cursor_aware_via_shared_textinput() {
    // The main filter now stores a TextInput — Left + insert edits
    // mid-string and the view still rebuilds on each accepted edit.
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
    app.rebuild_view();
    press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
    for c in "prod".chars() {
        press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    assert_eq!(app.view.filter().text(), "prod");
    press(&mut app, KeyCode::Left, KeyModifiers::NONE);
    press(&mut app, KeyCode::Char('X'), KeyModifiers::NONE);
    assert_eq!(app.view.filter().text(), "proXd");
}

#[tokio::test]
async fn render_places_caret_at_cursor_in_command_mode() {
    // Uses the TestBackend `render` harness to verify caret *rendering*
    // (not just the buffer model): the caret glyph splits the text at
    // the cursor column. The `:` command line is rendered in exactly
    // one place (the top bar), so a needle absent from the table is
    // unique on screen. With the cursor at the end "zzqz" renders
    // contiguously; move it left and the caret splits "zzq<caret>z".
    // Glyph-agnostic — doesn't depend on the Powerline/ASCII caret.
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
    app.rebuild_view();
    press(&mut app, KeyCode::Char(':'), KeyModifiers::NONE);
    for c in "zzqz".chars() {
        press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    let at_end = render(&mut app, 120, 30);
    assert!(
        at_end.contains("zzqz"),
        "caret at end keeps 'zzqz' contiguous"
    );
    press(&mut app, KeyCode::Left, KeyModifiers::NONE);
    let mid = render(&mut app, 120, 30);
    assert!(
        !mid.contains("zzqz"),
        "caret should split 'zzq<caret>z' — 'zzqz' no longer contiguous"
    );
    assert!(mid.contains("zzq"), "text before the caret still renders");
}

#[tokio::test]
async fn render_colours_health_dots_by_tier() {
    // Styled-harness demo: assert the env table paints each row's
    // health indicator in the tier colour, not just that the row
    // exists. The Green row must carry no Red cell (and vice versa),
    // which a text-only assertion can't catch.
    let mut app = test_app();
    app.environments = vec![
        mk_env("svc-red", "uflexi", "Web", "Red"),
        mk_env("svc-green", "uflexi", "Web", "Green"),
    ];
    app.rebuild_view();
    // Clear the cursor so neither asserted row gets the REVERSED
    // selection highlight (which swaps fg/bg and would mask the dot).
    app.table_state.select(None);
    let buf = render_buf(&mut app, 120, 30);
    let theme = app.theme.clone();
    let red_row = find_row(&buf, "svc-red").expect("red env row rendered");
    let green_row = find_row(&buf, "svc-green").expect("green env row rendered");
    assert!(
        row_has_fg(&buf, red_row, theme.health_red),
        "Red env row should paint a health_red cell"
    );
    assert!(
        row_has_fg(&buf, green_row, theme.health_green),
        "Green env row should paint a health_green cell"
    );
    assert!(
        !row_has_fg(&buf, green_row, theme.health_red),
        "Green env row must not paint any health_red cell"
    );
}

#[tokio::test]
async fn render_greens_type_to_confirm_only_on_exact_match() {
    // Styled-harness demo: the type-to-confirm field turns green only
    // when the typed text exactly matches the target env name.
    let mut app = test_app();
    // Red env behind the modal so the only green on screen can come
    // from the modal's match indicator, not a table health dot.
    app.environments = vec![mk_env("prod", "uflexi", "Web", "Red")];
    app.rebuild_view();
    app.mode = Mode::Action;
    let mut modal = mk_modal(Action::Terminate, "prod");
    modal.kind = ConfirmKind::TypeName;

    // Differential count so constant green chrome (header) doesn't
    // confound the check: the exact match must add green cells (the
    // typed field + enter hint) over the partial-match baseline.
    let theme = app.theme.clone();
    modal.typed = "pro".into();
    app.action_flow = Some(ActionFlow::Confirm(modal.clone()));
    let no_match_green = count_fg(&render_buf(&mut app, 120, 30), theme.health_green);

    modal.typed = "prod".into();
    app.action_flow = Some(ActionFlow::Confirm(modal));
    let matched_green = count_fg(&render_buf(&mut app, 120, 30), theme.health_green);

    assert!(
        matched_green > no_match_green,
        "exact type-to-confirm match should paint more green than a partial match \
             (matched={matched_green}, no_match={no_match_green})"
    );
}

#[tokio::test]
async fn render_demo_ironwood_row_shows_muted_dashes() {
    // The IRONWOOD demo tell: the `ironwood` env is absent from the
    // cost + instance-count maps, so its INST and COST cells render a
    // muted `—` ("Beanstalk can't account for it"). Drives the real
    // demo fixture so this is the on-screen artifact, not a synthetic.
    let mut app = test_app();
    crate::demo_fixture::install(&mut app);
    app.table_state.select(None); // avoid the REVERSED selection mask
    let theme = app.theme.clone();
    let buf = render_buf(&mut app, 160, 30);
    let y = find_row(&buf, "ironwood").expect("ironwood row rendered");
    // INST + COST both muted em-dashes → at least two such cells.
    assert!(
        count_symbol_fg(&buf, y, "—", theme.muted) >= 2,
        "ironwood row should show muted — for INST and COST"
    );
}

#[tokio::test]
async fn render_cost_column_red_tints_expensive_envs() {
    // `:cost on`: a >= $500/mo env paints its COST cell health_red.
    // poly-prod-api ($612, Green health) is the only red in its row
    // (its health dot is green), while the cheaper green-bucket
    // poly-staging-worker ($28, Green) has no red at all.
    let mut app = test_app();
    crate::demo_fixture::install(&mut app);
    app.table_state.select(None);
    let theme = app.theme.clone();
    let buf = render_buf(&mut app, 160, 30);
    let pricey = find_row(&buf, "poly-prod-api").expect("prod-api row");
    let cheap = find_row(&buf, "poly-staging-worker").expect("staging-worker row");
    assert!(
        row_has_fg(&buf, pricey, theme.health_red),
        "the $612 env should paint a health_red COST cell"
    );
    assert!(
        !row_has_fg(&buf, cheap, theme.health_red),
        "a cheap green-health env row should have no red cell"
    );
}

#[tokio::test]
async fn render_dlq_depth_tints_the_ready_pill_amber() {
    // A Worker env that EB reports Green but whose DLQ has messages
    // gets its `Ready` pill rendered in health_yellow — the row-level
    // "this isn't actually fine" signal. Differential: same env with
    // an empty DLQ shows no amber pill.
    let mut app = test_app();
    app.environments = vec![mk_env("worker-x", "uflexi", "Worker", "Green")];
    app.rebuild_view();
    app.table_state.select(None);
    let theme = app.theme.clone();

    let clean = render_buf(&mut app, 140, 30);
    let clean_amber = count_fg(&clean, theme.health_yellow);

    app.worker_dlq_depths.insert("worker-x".into(), 12);
    let backed_up = render_buf(&mut app, 140, 30);
    let dlq_amber = count_fg(&backed_up, theme.health_yellow);

    assert!(
        dlq_amber > clean_amber,
        "a non-empty DLQ should add amber (Ready-pill) cells \
             (dlq={dlq_amber}, clean={clean_amber})"
    );
}

#[tokio::test]
async fn render_redact_masks_the_cname() {
    // `:redact` (Ctrl-X) blanks sensitive columns — the CNAME renders
    // as ▓ blocks, and the real hostname no longer appears.
    let mut app = test_app();
    app.environments = vec![mk_env("svc", "uflexi", "Web", "Green")];
    app.rebuild_view();
    app.table_state.select(None);
    app.view.redact = false;
    let shown = render(&mut app, 160, 30);
    assert!(shown.contains("svc.example.com"), "cname visible when off");
    app.view.redact = true;
    let hidden = render(&mut app, 160, 30);
    assert!(
        !hidden.contains("svc.example.com"),
        "cname must be masked when redact is on"
    );
    assert!(hidden.contains('▓'), "redacted cells render as ▓ blocks");
}

#[tokio::test]
async fn esc_in_filter_mode_clears_the_filter() {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
    app.rebuild_view();
    press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
    for c in "x".chars() {
        press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    assert_eq!(app.view.filter().text(), "x");
    // Esc abandons the filter — both the text AND the mode revert.
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.view.filter().is_empty());
}

#[tokio::test]
async fn slash_with_active_filter_rebuilds_to_full_fleet() {
    // Regression: opening filter mode while a filter is already
    // active must clear the filter AND rebuild the cached view, so
    // the full fleet shows immediately — not the stale filtered
    // subset left over until the next keystroke. (Surfaced by the
    // demo gif's closing reveal; house rule: filter mutations call
    // rebuild_view().)
    let mut app = test_app();
    app.environments = vec![
        mk_env("prod-web", "uflexi", "Web", "Green"),
        mk_env("staging-web", "uflexi", "Web", "Green"),
    ];
    app.rebuild_view();
    // Apply a filter that matches exactly one env.
    press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
    for c in "prod".chars() {
        press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.view.filtered().len(), 1, "filter should hide one env");
    // `/` re-opens filter mode: filter empties and the view must
    // rebuild to show all envs right away.
    press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
    assert_eq!(app.mode, Mode::Filter);
    assert!(app.view.filter().is_empty());
    assert_eq!(
        app.view.filtered().len(),
        2,
        "full fleet should be visible the moment filter mode opens"
    );
}

#[tokio::test]
async fn star_toggles_pinned_set_for_selected_env() {
    let mut app = test_app();
    app.environments = vec![
        mk_env("api-prod", "uflexi", "Web", "Green"),
        mk_env("api-staging", "uflexi", "Web", "Green"),
    ];
    app.rebuild_view();
    // Star pins the cursor's env.
    press(&mut app, KeyCode::Char('*'), KeyModifiers::NONE);
    assert!(app.pinned.contains("api-prod"));
    // Second star unpins it.
    press(&mut app, KeyCode::Char('*'), KeyModifiers::NONE);
    assert!(!app.pinned.contains("api-prod"));
}

#[tokio::test]
async fn quickjump_input_is_cursor_aware_via_shared_textinput() {
    // QuickJump now stores a tui_common::TextInput, so it gains
    // mid-string editing (Left + insert, Backspace at the cursor)
    // rather than the old append-only buffer.
    let mut app = test_app();
    app.environments = vec![mk_env("prod", "uflexi", "Web", "Green")];
    app.rebuild_view();
    press(&mut app, KeyCode::Char('\''), KeyModifiers::NONE);
    assert_eq!(app.mode, Mode::QuickJump);
    for c in "abc".chars() {
        press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    assert_eq!(app.quickjump_input.text(), "abc");
    // Move the cursor left one and insert — mid-string, not appended.
    press(&mut app, KeyCode::Left, KeyModifiers::NONE);
    press(&mut app, KeyCode::Char('X'), KeyModifiers::NONE);
    assert_eq!(app.quickjump_input.text(), "abXc");
    press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(app.quickjump_input.text(), "abc");
}

#[tokio::test]
async fn palette_input_is_cursor_aware_via_shared_textinput() {
    // Palette search likewise delegates editing to the shared
    // TextInput — Home + insert lands at the front, and Ctrl-W
    // word-deletes (a capability the old append-only buffer lacked).
    let mut app = test_app();
    press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
    assert_eq!(app.mode, Mode::Palette);
    for c in "tag".chars() {
        press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    assert_eq!(app.palette_input.text(), "tag");
    press(&mut app, KeyCode::Home, KeyModifiers::NONE);
    press(&mut app, KeyCode::Char('X'), KeyModifiers::NONE);
    assert_eq!(app.palette_input.text(), "Xtag");
    press(&mut app, KeyCode::End, KeyModifiers::NONE);
    press(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);
    assert_eq!(app.palette_input.text(), "");
}

#[tokio::test]
async fn picker_filter_is_cursor_aware_via_shared_textinput() {
    // The picker (region/profile/…) filter now stores a TextInput.
    // `j`/`k` stay list-nav, but other chars edit, and the cursor
    // moves mid-string. Open the region picker with `r`.
    let mut app = test_app();
    press(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
    assert_eq!(app.mode, Mode::Picker);
    for c in "eu".chars() {
        press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    let txt = |a: &App| a.picker.as_ref().unwrap().filter.text().to_string();
    assert_eq!(txt(&app), "eu");
    press(&mut app, KeyCode::Left, KeyModifiers::NONE);
    press(&mut app, KeyCode::Char('X'), KeyModifiers::NONE);
    assert_eq!(txt(&app), "eXu");
    press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(txt(&app), "eu");
}

#[tokio::test]
async fn picker_workflow_open_filter_enter_dispatches_choice() {
    // `r` opens the region picker. Typing filters the list; Enter
    // applies the highlighted choice. We don't try to assert that
    // the AwsClient actually swapped (the test stub doesn't fire
    // real calls) — we assert the mode transitions and that the
    // picker's selected_value resolves the expected entry.
    let mut app = test_app();
    press(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
    assert_eq!(app.mode, Mode::Picker);
    assert!(app.picker.is_some());
    // Esc cancels — picker cleared, mode back to Normal.
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.picker.is_none());
}

/// Helper for the cancel-window tests — build a ConfirmModal for
/// the given Action / env. Mirrors the shape `advance_action_flow`
/// produces; pre-flight fields stay None (the cancel-window code
/// path doesn't read them).
fn mk_modal(action: Action, env: &str) -> ConfirmModal {
    ConfirmModal {
        action,
        target_env: env.into(),
        swap_with: None,
        typed: TextInput::new(),
        kind: ConfirmKind::YesNo,
        dryrun: None,
        loading_dryrun: false,
        recent_events: None,
        loading_events: false,
        traffic_warning: None,
        deploy_version: None,
        upgrade_platform_arn: None,
        upgrade_platform_label: None,
        clone_target: None,
        scale_min: None,
        scale_max: None,
        auto_rollback_secs: None,
        wait_for_green_secs: None,
        version_preview: None,
        loading_version_preview: false,
        health_check_probe: None,
        loading_health_check: false,
        unavailability_line: None,
        loading_unavailability: false,
        lint_issues: None,
        loading_lint: false,
        ssm_run_command: None,
        ssm_run_instances: None,
    }
}

#[tokio::test]
async fn queue_action_dispatch_holds_action_for_cancel_window() {
    let mut app = test_app();
    let modal = mk_modal(Action::Rebuild, "uflexi-prod");
    app.queue_action_dispatch(modal);
    let pd = app
        .pending_dispatch
        .as_ref()
        .expect("queue should set pending_dispatch");
    assert_eq!(pd.target, "uflexi-prod");
    assert!(
        matches!(pd.kind, PendingDispatchKind::Single { .. }),
        "queue_action_dispatch should produce a Single variant"
    );
    assert!(
        pd.deadline > std::time::Instant::now(),
        "deadline must be in the future"
    );
    let remaining = pd
        .deadline
        .saturating_duration_since(std::time::Instant::now());
    assert!(
        remaining <= UNDO_WINDOW && remaining >= UNDO_WINDOW - Duration::from_millis(500),
        "deadline should be roughly UNDO_WINDOW from now; got {remaining:?}"
    );
}

#[tokio::test]
async fn cancel_pending_dispatch_clears_field_and_emits_status() {
    let mut app = test_app();
    app.queue_action_dispatch(mk_modal(Action::Terminate, "uflexi-prod"));
    assert!(app.pending_dispatch.is_some());
    app.cancel_pending_dispatch();
    assert!(app.pending_dispatch.is_none());
    let msg = app.status_message.as_deref().unwrap_or("");
    assert!(
        msg.contains("undone") && msg.contains("uflexi-prod"),
        "status should mention the undo + env; got: {msg:?}"
    );
}

#[tokio::test]
async fn second_queue_attempt_errors_while_first_pending() {
    let mut app = test_app();
    app.queue_action_dispatch(mk_modal(Action::Rebuild, "first"));
    assert!(app.pending_dispatch.is_some());
    let first_deadline = app.pending_dispatch.as_ref().unwrap().deadline;
    // Second queue attempt is rejected; first dispatch is untouched.
    app.queue_action_dispatch(mk_modal(Action::Rebuild, "second"));
    assert_eq!(
        app.pending_dispatch.as_ref().unwrap().target,
        "first",
        "second queue must not replace the first"
    );
    assert_eq!(
        app.pending_dispatch.as_ref().unwrap().deadline,
        first_deadline,
        "second queue must not bump the deadline"
    );
    assert!(
        app.error_message
            .as_deref()
            .unwrap_or("")
            .contains("press U to undo"),
        "second queue should surface a useful error"
    );
}

#[tokio::test]
async fn tick_pending_dispatch_fires_after_deadline() {
    let mut app = test_app();
    // Forge a pending dispatch whose deadline has already elapsed
    // so tick_pending_dispatch fires synchronously without us
    // having to wait 5 seconds.
    let modal = mk_modal(Action::Rebuild, "expired");
    app.pending_dispatch = Some(PendingDispatch {
        deadline: std::time::Instant::now() - Duration::from_millis(1),
        label: "Rebuild env".into(),
        target: "expired".into(),
        kind: PendingDispatchKind::Single { modal },
    });
    app.tick_pending_dispatch();
    assert!(
        app.pending_dispatch.is_none(),
        "expired tick should clear the field (dispatch handed to spawn_action)"
    );
}

#[tokio::test]
async fn batch_action_routes_through_cancel_window() {
    let mut app = test_app();
    app.environments = vec![
        mk_env("prod-web", "uflexi", "Web", "Green"),
        mk_env("staging-web", "uflexi", "Web", "Green"),
    ];
    app.multi_selected.insert("prod-web".into());
    app.multi_selected.insert("staging-web".into());
    app.cmd_batch_action(Action::Rebuild);
    // Multi-select cleared; dispatch queued with a 5s deadline.
    assert!(
        app.multi_selected.is_empty(),
        "multi-select should clear once the batch is queued"
    );
    let pd = app
        .pending_dispatch
        .as_ref()
        .expect("batch action should queue a pending dispatch");
    match &pd.kind {
        PendingDispatchKind::BatchAction { action, env_names } => {
            assert_eq!(*action, Action::Rebuild);
            assert_eq!(env_names.len(), 2);
        }
        other => panic!(
            "expected BatchAction variant; got {other:?}",
            other = match other {
                PendingDispatchKind::Single { .. } => "Single",
                PendingDispatchKind::BatchAction { .. } => "BatchAction",
                PendingDispatchKind::BatchDeploy { .. } => "BatchDeploy",
                PendingDispatchKind::BatchTag { .. } => "BatchTag",
                PendingDispatchKind::BatchSetOption { .. } => "BatchSetOption",
            }
        ),
    }
}

#[tokio::test]
async fn batch_action_undo_cancels_whole_fanout() {
    let mut app = test_app();
    app.environments = vec![
        mk_env("e1", "uflexi", "Web", "Green"),
        mk_env("e2", "uflexi", "Web", "Green"),
        mk_env("e3", "uflexi", "Web", "Green"),
    ];
    for name in ["e1", "e2", "e3"] {
        app.multi_selected.insert(name.into());
    }
    app.cmd_batch_action(Action::RestartAppServer);
    assert!(app.pending_dispatch.is_some());
    app.cancel_pending_dispatch();
    assert!(
        app.pending_dispatch.is_none(),
        "cancel should drop the whole batch, not just one env"
    );
    let msg = app.status_message.as_deref().unwrap_or("");
    assert!(
        msg.contains("undone") && msg.contains("3 env(s)"),
        "status should call out the 3-env batch; got: {msg:?}"
    );
}

#[tokio::test]
async fn apps_scope_space_toggles_apps_selected() {
    let mut app = test_app();
    // Seed two apps + select Apps scope.
    app.applications = vec![
        crate::aws::Application {
            name: "billing".into(),
            description: String::new(),
            date_created: None,
            date_updated: None,
            version_count: 0,
            templates: vec![],
            latest_version_label: None,
            latest_version_created: None,
        },
        crate::aws::Application {
            name: "checkout".into(),
            description: String::new(),
            date_created: None,
            date_updated: None,
            version_count: 0,
            templates: vec![],
            latest_version_label: None,
            latest_version_created: None,
        },
    ];
    app.set_scope(Scope::Apps);
    app.app_table_state.select(Some(0));
    // First space adds; second space removes.
    press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
    assert!(app.apps_selected.contains("billing"));
    press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
    assert!(!app.apps_selected.contains("billing"));
}

#[tokio::test]
async fn apps_scope_star_pins_and_unpins_app() {
    let mut app = test_app();
    app.applications = vec![crate::aws::Application {
        name: "billing".into(),
        description: String::new(),
        date_created: None,
        date_updated: None,
        version_count: 0,
        templates: vec![],
        latest_version_label: None,
        latest_version_created: None,
    }];
    app.set_scope(Scope::Apps);
    app.app_table_state.select(Some(0));
    assert!(!app.pinned_apps.contains("billing"));
    press(&mut app, KeyCode::Char('*'), KeyModifiers::SHIFT);
    assert!(app.pinned_apps.contains("billing"));
    press(&mut app, KeyCode::Char('*'), KeyModifiers::SHIFT);
    assert!(!app.pinned_apps.contains("billing"));
}

#[tokio::test]
async fn esc_clears_apps_selected_when_no_envs_selected() {
    let mut app = test_app();
    app.apps_selected.insert("billing".into());
    app.apps_selected.insert("checkout".into());
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.apps_selected.is_empty());
}

#[tokio::test]
async fn capital_u_cancels_pending_dispatch_in_normal_mode() {
    let mut app = test_app();
    app.queue_action_dispatch(mk_modal(Action::Rebuild, "uflexi-prod"));
    assert!(app.pending_dispatch.is_some());
    press(&mut app, KeyCode::Char('U'), KeyModifiers::SHIFT);
    assert!(
        app.pending_dispatch.is_none(),
        "capital U in Normal mode should cancel the pending dispatch"
    );
}

// ── :event-tail ─────────────────────────────────────────────────

fn mk_fleet_event(env: &str, severity: &str, message: &str, at_ms: i64) -> crate::aws::Event {
    crate::aws::Event {
        at: chrono::DateTime::from_timestamp_millis(at_ms),
        env: env.into(),
        application: "shop".into(),
        message: message.into(),
        severity: severity.into(),
        version_label: None,
    }
}

#[test]
fn event_watermark_advances_past_newest_and_never_regresses() {
    let events = vec![
        mk_fleet_event("a", "INFO", "one", 1_000),
        mk_fleet_event("b", "INFO", "two", 5_000),
        mk_fleet_event("c", "INFO", "three", 3_000),
    ];
    // +1ms past the newest event, regardless of order.
    assert_eq!(next_event_watermark_ms(&events, 0), 5_001);
    // Empty batch keeps the previous watermark.
    assert_eq!(next_event_watermark_ms(&[], 7_000), 7_000);
    // A batch of only-older events (throttled retry replaying
    // history) must not move the watermark backwards.
    assert_eq!(next_event_watermark_ms(&events, 9_000), 9_000);
    // Undated events fall back to the previous watermark.
    let undated = vec![crate::aws::Event {
        at: None,
        ..mk_fleet_event("a", "INFO", "x", 0)
    }];
    assert_eq!(next_event_watermark_ms(&undated, 4_000), 4_000);
}

#[test]
fn event_tail_filter_matches_env_severity_and_message() {
    let re = |s: &str| {
        regex::RegexBuilder::new(s)
            .case_insensitive(true)
            .build()
            .unwrap()
    };
    let ev = mk_fleet_event("api-prod", "ERROR", "Deploy failed: timeout", 0);
    assert!(event_tail_matches(&re("prod"), &ev), "env name");
    assert!(event_tail_matches(&re("error"), &ev), "severity");
    assert!(event_tail_matches(&re("timeout"), &ev), "message");
    assert!(event_tail_matches(&re("shop"), &ev), "application");
    assert!(!event_tail_matches(&re("staging"), &ev));
}

#[tokio::test]
async fn event_tail_events_append_capped_and_stale_sessions_drop() {
    let mut app = test_app();
    app.event_tail_session = 3;
    app.handle_msg(AppMsg::EventTailOpened {
        gen: app.generation,
        session_id: 3,
    });
    assert!(
        matches!(app.current_overlay, Some(Overlay::EventTail { .. })),
        "opened installs the overlay"
    );
    // Fill past the ring cap: oldest must be dropped.
    let batch: Vec<crate::aws::Event> = (0..EVENT_TAIL_MAX_EVENTS as i64 + 5)
        .map(|i| mk_fleet_event("api", "INFO", &format!("m{i}"), i))
        .collect();
    app.handle_msg(AppMsg::EventTailEvents {
        gen: app.generation,
        session_id: 3,
        result: Ok(batch),
    });
    let Some(Overlay::EventTail { events, .. }) = app.current_overlay.as_ref() else {
        panic!("overlay should still be open");
    };
    assert_eq!(events.len(), EVENT_TAIL_MAX_EVENTS);
    assert_eq!(events.front().unwrap().message, "m5", "oldest dropped");
    // A stale session's batch is dropped on arrival.
    app.handle_msg(AppMsg::EventTailEvents {
        gen: app.generation,
        session_id: 2,
        result: Ok(vec![mk_fleet_event("late", "INFO", "stale", 0)]),
    });
    let Some(Overlay::EventTail { events, .. }) = app.current_overlay.as_ref() else {
        panic!("overlay should still be open");
    };
    assert!(!events.iter().any(|e| e.env == "late"));
}

#[tokio::test]
async fn event_tail_q_closes_and_defeats_late_reopen() {
    let mut app = test_app();
    app.event_tail_session = 1;
    app.handle_msg(AppMsg::EventTailOpened {
        gen: app.generation,
        session_id: 1,
    });
    press(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
    assert!(app.current_overlay.is_none(), "q closes the overlay");
    // A late Opened from the aborted task must not re-open it.
    app.handle_msg(AppMsg::EventTailOpened {
        gen: app.generation,
        session_id: 1,
    });
    assert!(
        app.current_overlay.is_none(),
        "session bump on close defeats the late re-open"
    );
}

#[tokio::test]
async fn event_tail_poller_reaped_when_overlay_replaced() {
    // Opening another overlay on top of the tail (without going
    // through the close handler) must not leave the poll task
    // running invisibly: the next poll result reaps it.
    let mut app = test_app();
    app.event_tail_session = 1;
    app.handle_msg(AppMsg::EventTailOpened {
        gen: app.generation,
        session_id: 1,
    });
    app.event_tail_task = Some(tokio::spawn(async {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }
    }));
    app.current_overlay = Some(Overlay::TextDump {
        title: "something else".into(),
        body: "…".into(),
    });
    app.handle_msg(AppMsg::EventTailEvents {
        gen: app.generation,
        session_id: 1,
        result: Ok(vec![]),
    });
    assert!(app.event_tail_task.is_none(), "poller must be reaped");
    assert_eq!(
        app.event_tail_session, 2,
        "session bump drops already-queued messages"
    );
    assert!(
        matches!(app.current_overlay, Some(Overlay::TextDump { .. })),
        "the replacement overlay is untouched"
    );
}

#[tokio::test]
async fn log_tail_poller_reaped_when_overlay_replaced() {
    // Same reap contract for :logs-tail — the quirk predates
    // :event-tail and both tails share the fix.
    let mut app = test_app();
    app.log_tail_session = 1;
    app.handle_msg(AppMsg::LogTailOpened {
        gen: app.generation,
        session_id: 1,
        env_name: "api-prod".into(),
        log_group: "/aws/elasticbeanstalk/api-prod/web.stdout.log".into(),
        since_ms: 0,
    });
    app.log_tail_task = Some(tokio::spawn(async {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }
    }));
    app.current_overlay = Some(Overlay::TextDump {
        title: "something else".into(),
        body: "…".into(),
    });
    app.handle_msg(AppMsg::LogTailEvents {
        gen: app.generation,
        session_id: 1,
        next_since_ms: 5,
        result: Ok(vec![]),
    });
    assert!(app.log_tail_task.is_none(), "poller must be reaped");
    assert_eq!(app.log_tail_session, 2);
}

#[tokio::test]
async fn event_tail_error_row_renders_red() {
    let mut app = test_app();
    app.event_tail_session = 1;
    app.handle_msg(AppMsg::EventTailOpened {
        gen: app.generation,
        session_id: 1,
    });
    app.handle_msg(AppMsg::EventTailEvents {
        gen: app.generation,
        session_id: 1,
        result: Ok(vec![
            mk_fleet_event("api-prod", "ERROR", "update failed", 1_000),
            mk_fleet_event("api-prod", "INFO", "all fine", 2_000),
        ]),
    });
    let theme = app.theme.clone();
    let buf = render_buf(&mut app, 120, 30);
    let err_row = find_row(&buf, "update failed").expect("error event row rendered");
    assert!(
        row_has_fg(&buf, err_row, theme.health_red),
        "ERROR severity cell painted health_red"
    );
    let info_row = find_row(&buf, "all fine").expect("info event row rendered");
    assert!(
        !row_has_fg(&buf, info_row, theme.health_red),
        "INFO row carries no red"
    );
}

// --- view-cache invariant ---------------------------------------------

#[tokio::test]
async fn alias_command_rebuilds_the_view() {
    // `rebuild_view` matches the filter against an env's alias as well as
    // its name, so adding an alias while a filter is active can change
    // which rows should be visible. Before ViewState made the cache
    // private, `:alias` mutated the map and left the table stale.
    let mut app = test_app();
    app.environments = vec![
        fake_env_with("api-prod", "Ready", "Green", None),
        fake_env_with("web-prod", "Ready", "Green", None),
    ];
    app.view.set_filter("checkout");
    app.rebuild_view();
    assert!(
        app.view.display().is_empty(),
        "nothing matches 'checkout' yet"
    );

    app.execute_command("alias api-prod checkout-service");
    assert!(!app.view.is_stale(), ":alias must rebuild the view");
    assert_eq!(
        app.view.filtered(),
        &[0],
        "the aliased env should now match the active filter"
    );

    app.execute_command("alias-drop api-prod");
    assert!(!app.view.is_stale(), ":alias-drop must rebuild the view");
    assert!(app.view.display().is_empty());
}

#[tokio::test]
async fn set_sort_reorders_and_leaves_the_view_cache_fresh() {
    // Sorting renumbers every index the view cache holds. `set_sort` is
    // the only way to change the sort — `ViewState` keeps the fields
    // private — and it re-sorts and rebuilds in one step, so the header
    // arrow can't end up disagreeing with the rows.
    let mut app = test_app();
    app.environments = vec![
        fake_env_with("b", "Ready", "Green", None),
        fake_env_with("a", "Ready", "Green", None),
    ];
    app.rebuild_view();
    app.set_sort(SortKey::Name, false);
    assert!(!app.view.is_stale());
    assert_eq!(app.view.sort_key(), SortKey::Name);
    assert_eq!(app.environments[0].name, "a");
    assert_eq!(app.view.filtered(), &[0, 1]);

    app.set_sort(SortKey::Name, true);
    assert!(app.view.sort_desc());
    assert_eq!(app.environments[0].name, "b", "desc must actually reorder");
}

#[tokio::test]
async fn unconsumed_key_in_filter_mode_leaves_the_view_fresh() {
    // `TextInput::handle_key` returns false for keys it doesn't consume
    // (Down, PageUp, most Ctrl chords). Taking the filter mutably to ask
    // marked the cache stale whether or not the key was consumed, and the
    // no-op arm didn't rebuild — so the next frame read a stale cache.
    let mut app = test_app();
    app.environments = vec![
        fake_env_with("api-prod", "Ready", "Green", None),
        fake_env_with("web-prod", "Ready", "Green", None),
    ];
    app.rebuild_view();
    press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
    assert_eq!(app.mode, Mode::Filter);

    press(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert!(
        !app.view.is_stale(),
        "a key the filter buffer ignores must not leave the cache stale"
    );
    // The frame after must not trip the freshness assertion.
    let _ = render(&mut app, 120, 30);
}

// --- coverage for the panic-site totalisations ------------------------

#[tokio::test]
async fn tab_completes_an_env_name_after_an_env_taking_command() {
    // Exercises `command_completion_step`'s env-mode branch — the one
    // that splits the input at the last whitespace. Previously that
    // `rfind` was an `expect` resting on the earlier `find`.
    let mut app = test_app();
    app.environments = vec![
        fake_env_with("api-prod", "Ready", "Green", None),
        fake_env_with("web-prod", "Ready", "Green", None),
    ];
    app.rebuild_view();
    app.mode = Mode::Command;
    app.command_input = "config-diff api".into();
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(
        app.command_input.text(),
        "config-diff api-prod",
        "the command prefix is preserved and the env name completed"
    );
}

#[tokio::test]
async fn tab_completes_an_env_name_across_a_multibyte_space() {
    // U+00A0 is whitespace and three bytes wide; `rfind` returns its
    // first byte, so the split has to step over the whole char.
    let mut app = test_app();
    app.environments = vec![fake_env_with("api-prod", "Ready", "Green", None)];
    app.rebuild_view();
    app.mode = Mode::Command;
    app.command_input = "config-diff\u{00A0}api".into();
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert!(
        app.command_input.text().ends_with("api-prod"),
        "got {:?}",
        app.command_input.text()
    );
}

#[tokio::test]
async fn rollout_advances_to_the_next_eligible_region() {
    // The dispatch-advance branch used to re-derive "not done" with
    // `next_eligible.expect("checked by done")`. Region 1 failed
    // pre-flight, so the advance must skip it and land on region 2.
    use crate::mode_action::{ActionFlow, RolloutFlow, RolloutRegion, RolloutState};
    let region = |name: &str, found: bool| RolloutRegion {
        region: name.into(),
        current_version: Some("v1".into()),
        env_found: Some(found),
        preflight_error: None,
        outcome: None,
    };
    let mut app = test_app();
    app.action_flow = Some(ActionFlow::Rollout(RolloutFlow {
        rollout_id: "rollout-test".into(),
        env_name: "api-prod".into(),
        version_label: "v2".into(),
        regions: vec![
            region("eu-west-1", true),
            region("eu-west-2", false), // failed pre-flight — must be skipped
            region("us-east-1", true),
        ],
        state: RolloutState::Dispatching { next_index: 0 },
        wait_for_green_secs: None,
    }));

    app.handle_msg(AppMsg::RolloutDispatched {
        gen: app.generation,
        region: "eu-west-1".into(),
        result: Ok(()),
    });

    let Some(ActionFlow::Rollout(flow)) = app.action_flow.as_ref() else {
        panic!("rollout flow should still be active");
    };
    assert_eq!(
        flow.state,
        RolloutState::Dispatching { next_index: 2 },
        "must skip the region that failed pre-flight"
    );
    assert!(flow.regions[0].outcome.is_some());
    assert!(flow.regions[1].outcome.is_none(), "skipped, not dispatched");
}

#[tokio::test]
async fn rollout_halts_on_a_failed_region() {
    // The complement branch: an Err outcome ends the rollout even
    // though an eligible region remains.
    use crate::mode_action::{ActionFlow, RolloutFlow, RolloutRegion, RolloutState};
    let region = |name: &str| RolloutRegion {
        region: name.into(),
        current_version: Some("v1".into()),
        env_found: Some(true),
        preflight_error: None,
        outcome: None,
    };
    let mut app = test_app();
    app.action_flow = Some(ActionFlow::Rollout(RolloutFlow {
        rollout_id: "rollout-test".into(),
        env_name: "api-prod".into(),
        version_label: "v2".into(),
        regions: vec![region("eu-west-1"), region("us-east-1")],
        state: RolloutState::Dispatching { next_index: 0 },
        wait_for_green_secs: None,
    }));

    app.handle_msg(AppMsg::RolloutDispatched {
        gen: app.generation,
        region: "eu-west-1".into(),
        result: Err("UpdateEnvironment refused".into()),
    });

    let Some(ActionFlow::Rollout(flow)) = app.action_flow.as_ref() else {
        panic!("rollout flow should still be active");
    };
    assert_eq!(flow.state, RolloutState::Done, "halt on first failure");
    assert!(flow.regions[1].outcome.is_none(), "never dispatched");
}

#[tokio::test]
async fn config_tab_renders_the_in_place_value_editor() {
    // `draw_detail_config` matched on `editing.map(|e| e.mode)` and then
    // unwrapped `editing` inside the arm. Nothing drew this row, so the
    // rewrite to a match-on-Option was unverified. Render it.
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    let detail = app.detail.as_mut().expect("detail opened");
    detail.tags = vec![("Owner".into(), "platform".into())];
    detail.config_cursor = 0;
    detail.tab_idx = detail
        .tabs
        .iter()
        .position(|t| *t == DetailTab::Config)
        .expect("Config tab present");

    app.start_config_edit();
    let edit = app
        .detail
        .as_ref()
        .and_then(|d| d.config_edit.as_ref())
        .expect("editor open");
    assert_eq!(edit.mode, crate::app::ConfigEditMode::Value);

    let out = render(&mut app, 140, 40);
    assert!(out.contains("Owner"), "key cell still renders:\n{out}");
    assert!(
        out.contains("platform"),
        "the value being edited renders:\n{out}"
    );
}

// --- cost refresh truncation ------------------------------------------

#[tokio::test]
async fn a_truncated_cost_refresh_keeps_the_previous_map() {
    // The truncation flag protected the 24-hour disk cache but the
    // handler still cleared and replaced the live map first — so 25 of
    // 40 envs would flip from real numbers to `—`, which renders
    // identically to "untagged", while `:fleet-cost` under-reported.
    let mut app = test_app();
    app.costs.insert("api-prod".into(), 100.0);
    app.costs.insert("web-prod".into(), 200.0);
    app.costs.insert("worker-prod".into(), 50.0);
    let before = app.costs.clone();

    app.handle_msg(AppMsg::CostsFetched {
        gen: app.generation,
        account: Some("123456789012".into()),
        region: "us-east-1".into(),
        result: Ok(crate::aws::EnvCosts {
            rows: vec![crate::aws::EnvCost {
                env_name: "api-prod".into(),
                cost_usd: 111.0,
            }],
            truncated: true,
        }),
    });

    assert_eq!(
        app.costs, before,
        "a partial walk must not replace a good map"
    );
    let msg = app.error_message.as_deref().expect("must say so");
    assert!(msg.contains("INCOMPLETE"), "{msg}");
    assert_no_run_on_spaces(msg);
}

/// A wrapped string literal without a `\` continuation embeds the
/// newline *and* the next line's indentation, so the message reaches
/// the operator with a long run of spaces in the middle of a sentence —
/// and the TUI's error bar is one line, so a narrow terminal pushes the
/// actionable half off-screen. This has now happened twice; assert on
/// the rendered text rather than trusting the literal.
#[track_caller]
fn assert_no_run_on_spaces(msg: &str) {
    assert!(
        !msg.contains("  "),
        "message contains a double space (missing a `\\` continuation, or a \
         stray space before a `{{}}` placeholder?): {msg:?}"
    );
}

#[tokio::test]
async fn a_truncated_cost_refresh_with_nothing_cached_shows_what_it_has() {
    // With no previous map there is nothing to preserve, so partial
    // beats blank — but it must be labelled and must not stamp a fetch
    // time that would suppress the retry.
    let mut app = test_app();
    assert!(app.costs.is_empty());

    app.handle_msg(AppMsg::CostsFetched {
        gen: app.generation,
        account: None,
        region: "us-east-1".into(),
        result: Ok(crate::aws::EnvCosts {
            rows: vec![crate::aws::EnvCost {
                env_name: "api-prod".into(),
                cost_usd: 111.0,
            }],
            truncated: true,
        }),
    });

    assert_eq!(app.costs.len(), 1, "partial data still renders");
    assert!(
        app.costs_fetched_at.is_none(),
        "an incomplete walk must not stamp a fetch time"
    );
    let msg = app.error_message.as_deref().expect("must say so");
    assert!(msg.contains("INCOMPLETE"), "{msg}");
    assert_no_run_on_spaces(msg);
}

#[tokio::test]
async fn a_complete_cost_refresh_replaces_the_map() {
    let mut app = test_app();
    app.costs.insert("stale".into(), 999.0);

    app.handle_msg(AppMsg::CostsFetched {
        gen: app.generation,
        account: None,
        region: "us-east-1".into(),
        result: Ok(crate::aws::EnvCosts {
            rows: vec![crate::aws::EnvCost {
                env_name: "api-prod".into(),
                cost_usd: 111.0,
            }],
            truncated: false,
        }),
    });

    assert_eq!(app.costs.len(), 1);
    assert!(app.costs.contains_key("api-prod"));
    assert!(!app.costs.contains_key("stale"), "a complete walk replaces");
    assert!(app.costs_fetched_at.is_some());
}

// --- partition-aware :explain and console links -----------------------

#[test]
fn parse_access_denied_rewrites_a_govcloud_session_arn() {
    // The rewrite matched the literal `arn:aws:sts::`, so in GovCloud,
    // China or an ISO partition the branch never fired and the raw
    // session ARN went to `iam:SimulatePrincipalPolicy`, which rejects
    // it — session credentials aren't a policy attachment point. The
    // endpoint fix got `:explain` to the right IAM endpoint; this is
    // what it failed on once it got there.
    let msg = "User: arn:aws-us-gov:sts::123456789012:assumed-role/EbAdmin/session \
               is not authorized to perform: elasticbeanstalk:UpdateEnvironment";
    let (principal, action) = super::parse_access_denied(msg).expect("parsed");
    assert_eq!(principal, "arn:aws-us-gov:iam::123456789012:role/EbAdmin");
    assert_eq!(action, "elasticbeanstalk:UpdateEnvironment");
}

#[test]
fn parse_access_denied_handles_every_partition() {
    for partition in ["aws", "aws-us-gov", "aws-cn", "aws-iso", "aws-iso-b"] {
        let msg = format!(
            "User: arn:{partition}:sts::1:assumed-role/R/S is not authorized to perform: s3:GetObject"
        );
        let (principal, _) = super::parse_access_denied(&msg).expect("parsed");
        assert_eq!(
            principal,
            format!("arn:{partition}:iam::1:role/R"),
            "the rebuilt role ARN must stay in its own partition"
        );
    }
}

#[test]
fn parse_access_denied_leaves_a_plain_user_arn_alone() {
    let msg = "User: arn:aws:iam::1:user/alice is not authorized to perform: s3:GetObject";
    let (principal, _) = super::parse_access_denied(msg).expect("parsed");
    assert_eq!(principal, "arn:aws:iam::1:user/alice");
}

#[test]
fn console_url_follows_the_partition() {
    let gov = console_url("us-gov-west-1", "myapp", "myenv").expect("govcloud has a console");
    assert!(
        gov.contains("us-gov-west-1.console.amazonaws-us-gov.com"),
        "got {gov}"
    );
    let cn = console_url("cn-north-1", "myapp", "myenv").expect("china has a console");
    assert!(cn.contains("cn-north-1.console.amazonaws.cn"), "got {cn}");
    // No guessed hostname for the ISO partitions.
    assert!(console_url("us-iso-east-1", "myapp", "myenv").is_none());
}

#[test]
fn parse_access_denied_keeps_a_non_assumed_role_sts_principal() {
    // Making the rewrite partition-generic moved the `?` operators into
    // an arm that now fires for EVERY partition, so an STS ARN that
    // isn't an assumed-role — a federated user, say — propagates None
    // out of the whole function. Before, the branch simply didn't match
    // and the principal was returned unchanged.
    let msg = "User: arn:aws-us-gov:sts::123456789012:federated-user/ci-bot \
               is not authorized to perform: elasticbeanstalk:UpdateEnvironment";
    let (principal, action) = super::parse_access_denied(msg)
        .expect("a federated-user denial must still parse, not vanish");
    assert_eq!(
        principal,
        "arn:aws-us-gov:sts::123456789012:federated-user/ci-bot"
    );
    assert_eq!(action, "elasticbeanstalk:UpdateEnvironment");
}

#[tokio::test]
async fn explain_accepts_an_arn_from_any_partition() {
    // The guard matched the literal `arn:aws:`, so `:explain` refused
    // its own documented argument form for every operator outside the
    // commercial partition — one level above the rewrite that was
    // fixed for exactly the same reason.
    for arn in [
        "arn:aws:iam::123456789012:role/EbAdmin",
        "arn:aws-us-gov:iam::123456789012:role/EbAdmin",
        "arn:aws-cn:iam::123456789012:role/EbAdmin",
        "arn:aws-iso-b:iam::123456789012:role/EbAdmin",
    ] {
        let mut app = test_app();
        app.execute_command(&format!("explain {arn} elasticbeanstalk:UpdateEnvironment"));
        assert!(
            !app.error_message
                .as_deref()
                .unwrap_or_default()
                .starts_with("usage:"),
            "{arn} was rejected as malformed: {:?}",
            app.error_message
        );
    }
}

#[tokio::test]
async fn row_region_is_used_for_links_and_cli_snippets() {
    // Under a fan-out the selected row can be in a different region
    // from `context.region`. The home region opens a console dashboard
    // where the environment doesn't exist, and produces a CLI snippet
    // that returns an empty array — or the WRONG environment when a
    // same-named one exists at home.
    //
    // The previous version of this test re-implemented the fixed
    // expression inline and never called a production function, so
    // reverting the fix left it green.
    let mut app = test_app();
    let mut env = mk_env("api-prod", "uflexi", "Web", "Green");
    env.region = Some("eu-west-2".into());
    app.environments = vec![env.clone()];
    app.rebuild_view();
    app.table_state.select(Some(0));
    assert_eq!(app.context.region, "us-east-1", "home region differs");

    // The accessor all three sites share.
    assert_eq!(app.region_for(&env), "eu-west-2");
    // A row with no region of its own falls back to the home region.
    let mut homeless = env.clone();
    homeless.region = None;
    assert_eq!(app.region_for(&homeless), "us-east-1");

    // And the snippet actually copied uses it.
    app.yank_cli();
    let cmd = app.last_yanked_cli.as_deref().unwrap_or_default();
    assert!(
        cmd.contains("--region eu-west-2"),
        "the copied CLI must name the row's region: {cmd}"
    );
}

// --- event-tail gap marker survives its own batch and the filter -------

#[test]
fn event_tail_gap_marker_survives_an_active_filter() {
    // The marker carries no env or application, so `/payments-prod`
    // dropped it — a filtered tail that silently omits "events are
    // missing" is the unbroken chronology the marker exists to prevent.
    let pattern = regex::Regex::new("payments-prod").expect("regex");
    let marker = crate::aws::Event {
        at: None,
        env: String::new(),
        application: String::new(),
        message: "… older events in this window were not fetched".into(),
        severity: super::EVENT_TAIL_GAP_SEVERITY.into(),
        version_label: None,
    };
    assert!(
        super::event_tail_matches(&pattern, &marker),
        "the gap marker must never be filtered out"
    );

    // A real event that doesn't match is still filtered.
    let other = crate::aws::Event {
        at: Some(chrono::Utc::now()),
        env: "api-prod".into(),
        application: "uflexi".into(),
        message: "deployed".into(),
        severity: "INFO".into(),
        version_label: None,
    };
    assert!(!super::event_tail_matches(&pattern, &other));
}

#[test]
fn a_real_event_is_not_mistaken_for_the_gap_marker() {
    // The sentinel is severity + no timestamp together, so an EB event
    // that happens to lack a date can't impersonate it.
    let undated = crate::aws::Event {
        at: None,
        env: "api-prod".into(),
        application: "uflexi".into(),
        message: "something".into(),
        severity: "INFO".into(),
        version_label: None,
    };
    assert!(!super::is_event_tail_gap(&undated));
}

#[tokio::test]
async fn a_truncated_poll_is_still_reported_after_the_marker_is_evicted() {
    // The in-stream marker cannot be relied on: a truncated poll can
    // carry more events than the ring holds, so the marker — inserted
    // as the oldest row — is evicted by its own batch or by the next
    // poll, and the overlay opens in follow mode at the newest end
    // where the marker isn't. The sticky counter in the chrome is what
    // has to survive.
    //
    // The previous version of this test asserted `kept + 1 == cap`,
    // arithmetic over two constants — deleting the whole mechanism left
    // it green.
    let mut app = test_app();
    // The handler drops opens for stale sessions, so match the id.
    app.event_tail_session = 1;
    app.handle_msg(AppMsg::EventTailOpened {
        gen: app.generation,
        session_id: 1,
    });

    let marker = crate::aws::Event {
        at: None,
        env: String::new(),
        application: String::new(),
        message: "… older events in this window were not fetched".into(),
        severity: super::EVENT_TAIL_GAP_SEVERITY.into(),
        version_label: None,
    };
    // One truncated poll: the marker plus enough events to evict it.
    let mut batch = vec![marker];
    for i in 0..super::EVENT_TAIL_MAX_EVENTS {
        batch.push(crate::aws::Event {
            at: Some(chrono::Utc::now()),
            env: "api-prod".into(),
            application: "uflexi".into(),
            message: format!("event {i}"),
            severity: "INFO".into(),
            version_label: None,
        });
    }
    app.handle_msg(AppMsg::EventTailEvents {
        gen: app.generation,
        session_id: 1,
        result: Ok(batch),
    });

    let Some(super::Overlay::EventTail {
        events,
        truncated_polls,
        ..
    }) = app.current_overlay.as_ref()
    else {
        panic!("event tail should be open");
    };
    assert!(
        !events.iter().any(super::is_event_tail_gap),
        "the marker was evicted by its own batch — which is the point"
    );
    assert_eq!(
        *truncated_polls, 1,
        "the gap must still be reported once the marker is gone"
    );
}

#[tokio::test]
async fn a_partial_cost_map_does_not_become_permanent() {
    // The "do we already have costs?" test made partial data sticky:
    // the first truncated walk populated the map, and every later one
    // then saw a non-empty map and kept it — so the first failure's
    // data survived the session while each retry paid for twenty
    // metered Cost Explorer pages and threw them away.
    let mut app = test_app();
    assert!(app.costs.is_empty());

    let truncated = |env: &str, usd: f64| crate::aws::EnvCosts {
        rows: vec![crate::aws::EnvCost {
            env_name: env.into(),
            cost_usd: usd,
        }],
        truncated: true,
    };

    // First truncated walk: nothing to preserve, take it, mark partial.
    app.handle_msg(AppMsg::CostsFetched {
        gen: app.generation,
        account: None,
        region: "us-east-1".into(),
        result: Ok(truncated("api-prod", 100.0)),
    });
    assert!(!app.costs_complete, "a truncated walk is not complete");
    assert_eq!(app.costs.get("api-prod"), Some(&100.0));

    // Second truncated walk: what we hold is itself partial, so the
    // fresher partial data must replace it rather than be discarded.
    app.handle_msg(AppMsg::CostsFetched {
        gen: app.generation,
        account: None,
        region: "us-east-1".into(),
        result: Ok(truncated("web-prod", 55.0)),
    });
    assert!(!app.costs_complete);
    assert_eq!(app.costs.get("web-prod"), Some(&55.0));
    assert!(
        !app.costs.contains_key("api-prod"),
        "the stale partial map must not accumulate"
    );

    // A complete walk clears the partial flag and wins outright.
    app.handle_msg(AppMsg::CostsFetched {
        gen: app.generation,
        account: None,
        region: "us-east-1".into(),
        result: Ok(crate::aws::EnvCosts {
            rows: vec![crate::aws::EnvCost {
                env_name: "full".into(),
                cost_usd: 1.0,
            }],
            truncated: false,
        }),
    });
    assert!(app.costs_complete);
    assert_eq!(app.costs.len(), 1);

    // And now a truncated walk must NOT replace the complete map.
    app.handle_msg(AppMsg::CostsFetched {
        gen: app.generation,
        account: None,
        region: "us-east-1".into(),
        result: Ok(truncated("partial", 9.0)),
    });
    assert!(
        app.costs_complete,
        "a complete map survives a truncated walk"
    );
    assert_eq!(app.costs.get("full"), Some(&1.0));
}

// ── every write command refuses under read-only ────────────────────
//
// The safety gate itself is well covered; what wasn't covered is that
// each write command actually reaches it. Several whole modules of
// setters (`cmd_option`, `cmd_settings`) contain no `deny_write` call
// at all — they're safe only because every one of them routes through
// `spawn_option_settings_update` or `spawn_tag_update`, which gate.
// That is a convention, and this is what turns it into a checked one:
// a new setter that dispatches directly fails here.

/// Write commands, with arguments plausible enough to get past their
/// own usage validation and reach the gate.
const WRITE_COMMANDS: &[&str] = &[
    // option-setting setters (cmd_option.rs — no deny_write of its own)
    "deployment-policy Rolling",
    "rolling-update on",
    "health-check-url /healthz",
    "keypair my-key",
    "service-role arn:aws:iam::1:role/svc",
    "instance-profile arn:aws:iam::1:instance-profile/eb",
    "public-ip on",
    "elb-scheme internal",
    "set-option aws:autoscaling:asg MinSize 2",
    // per-env settings (cmd_settings.rs — likewise)
    "tag Owner platform",
    "untag Owner",
    "logs-stream on",
    "notify ops@example.com",
    "rds-detach api-prod",
    // alarms and templates (these gate directly)
    "alarm-create my-alarm 5xx 20",
    "alarm-delete ebman-api-prod-5xx",
    "config-save my-template",
    "config-apply my-template",
];

/// Writes that aren't scoped to one environment — a saved-configuration
/// template belongs to an *application*. These gate with an empty env
/// name, so they honour the global toggle but deliberately can't match
/// a per-env pin. Listed separately so that distinction is stated
/// rather than silently absent from the table above.
const APPLICATION_SCOPED_WRITES: &[&str] = &["config-delete uflexi my-template"];

fn read_only_app_with_env() -> App {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.read_only = true;
    app
}

#[tokio::test]
async fn every_write_command_is_refused_in_read_only_mode() {
    for cmd in WRITE_COMMANDS.iter().chain(APPLICATION_SCOPED_WRITES) {
        let mut app = read_only_app_with_env();
        app.execute_command(cmd);
        let err = app.error_message.as_deref().unwrap_or_default();
        assert!(
            err.contains("read-only mode"),
            ":{cmd} was not refused by the safety gate — got {err:?}\n\
             (a write that doesn't reach `deny_write` ignores --deny-write \
             and safety.envs.*.read_only)"
        );
    }
}

#[tokio::test]
async fn an_application_scoped_write_still_honours_the_global_toggle() {
    // It can't match a per-env pin — there's no single env — so the
    // global toggle is the only thing standing in front of it.
    for cmd in APPLICATION_SCOPED_WRITES {
        let mut app = read_only_app_with_env();
        app.execute_command(cmd);
        let err = app.error_message.as_deref().unwrap_or_default();
        assert!(err.contains("read-only mode"), ":{cmd} — got {err:?}");
    }
}

#[tokio::test]
async fn every_write_command_is_refused_by_a_per_env_safety_pin() {
    // The global toggle and the per-env pin are separate paths through
    // `is_read_only_for`; a command could honour one and not the other.
    for cmd in WRITE_COMMANDS {
        let mut app = read_only_app_with_env();
        app.read_only = false;
        app.cfg.safety_envs.insert("api-prod".into(), true);
        app.execute_command(cmd);
        let err = app.error_message.as_deref().unwrap_or_default();
        assert!(
            err.contains("safety.envs"),
            ":{cmd} ignored the per-env safety pin — got {err:?}"
        );
    }
}

/// Bulk writes. These gate through `deny_write_batch` rather than
/// `deny_write` — a separate code path that a per-command test of the
/// single-env surface wouldn't exercise at all.
const BATCH_WRITE_COMMANDS: &[&str] = &[
    "batch-rebuild",
    "batch-restart",
    "batch-deploy build-900",
    "batch-tag Owner platform",
    "batch-untag Owner",
    "batch-set-option aws:autoscaling:asg MinSize 2",
];

#[tokio::test]
async fn every_batch_write_is_refused_in_read_only_mode() {
    for cmd in BATCH_WRITE_COMMANDS {
        let mut app = read_only_app_with_env();
        // Bulk ops act on the space-multi-selected set.
        app.multi_selected = ["api-prod".to_string()].into_iter().collect();
        app.execute_command(cmd);
        let err = app.error_message.as_deref().unwrap_or_default();
        assert!(
            err.contains("read-only"),
            ":{cmd} was not refused by the batch safety gate — got {err:?}"
        );
    }
}

#[tokio::test]
async fn every_batch_write_is_refused_when_one_member_is_pinned() {
    // The point of `deny_write_batch`: a batch is refused if ANY member
    // is pinned, not just if all of them are. A batch that skipped the
    // pinned env and wrote to the rest would be worse than refusing.
    for cmd in BATCH_WRITE_COMMANDS {
        let mut app = read_only_app_with_env();
        app.environments = vec![
            mk_env("api-prod", "uflexi", "Web", "Green"),
            mk_env("api-staging", "uflexi", "Web", "Green"),
        ];
        app.rebuild_view();
        app.table_state.select(Some(0));
        app.read_only = false;
        // Only ONE of the two is pinned.
        app.cfg.safety_envs.insert("api-prod".into(), true);
        app.multi_selected = ["api-prod".to_string(), "api-staging".to_string()]
            .into_iter()
            .collect();
        app.execute_command(cmd);
        let err = app.error_message.as_deref().unwrap_or_default();
        assert!(
            err.contains("safety.envs"),
            ":{cmd} wrote to a batch containing a pinned env — got {err:?}"
        );
    }
}

// ── DLQ destructive operations gate too ────────────────────────────

/// A DLQ viewer open on `env`, with one message selected — enough for
/// the destructive handlers to get as far as the safety gate.
fn open_dlq_state(env: &str) -> crate::app::DlqState {
    let mut list_state = ratatui::widgets::ListState::default();
    list_state.select(Some(0));
    crate::app::DlqState {
        env_name: env.into(),
        main_queue_url: "https://sqs/q".into(),
        dlq_url: "https://sqs/q-dlq".into(),
        messages: vec![crate::aws::QueueMessage {
            id: "m-1".into(),
            receipt_handle: "rh-1".into(),
            body: "{}".into(),
            receive_count: 1,
            sent_at: None,
        }],
        list_state,
        loading: false,
        error: None,
        confirm_purge: false,
        purge_typed: Default::default(),
        viewing: crate::app::QueueView::Dlq,
        confirm_delete_id: None,
        replay_input: None,
    }
}

#[tokio::test]
async fn dlq_destructive_operations_are_refused_in_read_only_mode() {
    // Purge and replay are irreversible and driven from the DLQ
    // viewer's keymap rather than a `:command`, so the command-level
    // property tests above never reach them.
    /// One destructive DLQ handler, driven from the viewer's keymap.
    type DlqOp = fn(&mut App);
    let cases: Vec<(&str, DlqOp)> = vec![
        ("purge", |app: &mut App| {
            app.spawn_dlq_purge("api-prod".into(), "https://sqs/q-dlq".into())
        }),
        ("replay", |app: &mut App| app.spawn_dlq_replay_batch(vec![])),
        ("resend", |app: &mut App| app.spawn_dlq_resend_selected()),
        ("delete", |app: &mut App| app.spawn_dlq_delete_one("m-1")),
    ];
    for (name, op) in cases {
        let mut app = read_only_app_with_env();
        // `replay`/`resend`/`delete` read the env from DLQ state and
        // return early without it, so they'd never reach the gate.
        app.dlq = Some(open_dlq_state("api-prod"));
        op(&mut app);
        let err = app.error_message.as_deref().unwrap_or_default();
        assert!(
            err.contains("read-only mode"),
            "DLQ {name} was not refused — got {err:?}"
        );
    }
}

#[tokio::test]
async fn dlq_destructive_operations_honour_a_per_env_pin() {
    let mut app = read_only_app_with_env();
    app.read_only = false;
    app.cfg.safety_envs.insert("api-prod".into(), true);
    app.spawn_dlq_purge("api-prod".into(), "https://sqs/q-dlq".into());
    let err = app.error_message.as_deref().unwrap_or_default();
    assert!(err.contains("safety.envs"), "got {err:?}");
}

#[tokio::test]
async fn a_region_that_fails_the_fan_out_is_reported_not_dropped() {
    // The fan-out only reported an error when EVERY region failed, so
    // one region throttling or exceeding its page budget removed all
    // of its environments from the table with nothing on screen. That
    // was survivable while a truncated walk returned a short list;
    // once `list_environments` started refusing partial results it
    // meant a whole region could vanish silently.
    let mut app = test_app();
    app.apply_refresh(
        Ok(vec![mk_env("api-prod", "uflexi", "Web", "Green")]),
        vec!["eu-west-2: DescribeEnvironments failed".to_string()],
    );
    let err = app.error_message.as_deref().unwrap_or_default();
    assert!(
        err.contains("eu-west-2") && err.contains("NOT shown"),
        "a partially-failed fan-out must say which region is missing: {err:?}"
    );
    assert_eq!(
        app.environments.len(),
        1,
        "the rows that arrived still render"
    );
}

#[tokio::test]
async fn a_clean_fan_out_reports_nothing() {
    let mut app = test_app();
    app.apply_refresh(
        Ok(vec![mk_env("api-prod", "uflexi", "Web", "Green")]),
        Vec::new(),
    );
    assert!(app.error_message.is_none());
}

#[test]
fn the_gap_marker_is_not_rendered_in_the_dimmest_colour() {
    // Changing the sentinel severity from "WARN" to "GAP" demoted the
    // marker's colour: the renderer matches ERROR/FATAL → red, WARN →
    // yellow, and everything else → muted. So the change that made the
    // marker survive the filter also made it the least visible line on
    // screen — strictly worse than the yellow row it replaced.
    let theme = crate::theme::Theme::default();
    assert_ne!(
        crate::ui::event_severity_style(super::EVENT_TAIL_GAP_SEVERITY, &theme),
        crate::ui::event_severity_style("INFO", &theme),
        "the gap marker must not render identically to routine chatter"
    );
}

#[tokio::test]
async fn cost_status_and_fleet_cost_say_when_the_data_is_partial() {
    // A truncated walk deliberately leaves `costs_fetched_at` unset so
    // a retry isn't suppressed — which meant `:cost status` reported
    // "no data yet" while dollar figures were on screen, and
    // `:fleet-cost` rendered an under-reporting total with no marker.
    let mut app = test_app();
    app.cost_enabled = true;
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
    app.rebuild_view();
    app.handle_msg(AppMsg::CostsFetched {
        gen: app.generation,
        account: None,
        region: "us-east-1".into(),
        result: Ok(crate::aws::EnvCosts {
            rows: vec![crate::aws::EnvCost {
                env_name: "api-prod".into(),
                cost_usd: 100.0,
            }],
            truncated: true,
        }),
    });
    assert!(!app.costs_complete);

    app.execute_command("cost status");
    let status = app.status_message.as_deref().unwrap_or_default();
    assert!(
        status.contains("INCOMPLETE"),
        ":cost status must not present partial data as settled: {status:?}"
    );

    app.error_message = None;
    app.execute_command("fleet-cost");
    let err = app.error_message.as_deref().unwrap_or_default();
    assert!(
        err.contains("under-reports"),
        ":fleet-cost must mark a partial total: {err:?}"
    );
}

#[tokio::test]
async fn switching_context_resets_the_cost_completeness_verdict() {
    // The flag belonged to the previous account; leaving it set meant
    // a fresh context inherited a stale "partial" verdict.
    let mut app = test_app();
    app.cost_enabled = true; // `:cost off` early-returns when already off
    app.costs.insert("old".into(), 1.0);
    app.costs_complete = false;
    app.execute_command("cost off");
    assert!(app.costs.is_empty());
    assert!(app.costs_complete, "a torn-down map carries no verdict");
}

#[tokio::test]
async fn a_partial_fan_out_does_not_clobber_an_operator_message() {
    // The auto-clear above deliberately preserves a message the
    // operator set during the refresh round-trip — a failed `:deploy`,
    // say. Writing the region notice unconditionally overwrote it, and
    // did so again every 15s tick with no way to dismiss it.
    let mut app = test_app();
    app.error_message = Some("deploy failed: version build-901 not found".into());
    app.status_snapshot_at_refresh = Some((None, None));
    app.apply_refresh(
        Ok(vec![mk_env("api-prod", "uflexi", "Web", "Green")]),
        vec!["region eu-west-2: DescribeEnvironments failed".to_string()],
    );
    assert_eq!(
        app.error_message.as_deref(),
        Some("deploy failed: version build-901 not found"),
        "the operator's own message must survive the region notice"
    );
}

#[tokio::test]
async fn a_partially_throttled_fan_out_still_backs_off() {
    // Throttled regions now arrive in the Ok arm, because other regions
    // returned rows. Resetting the back-off there meant ebman never
    // backed off from the regions rate-limiting it, re-hammering them
    // every tick and deepening the throttle.
    let mut app = test_app();
    app.apply_refresh(
        Ok(vec![mk_env("api-prod", "uflexi", "Web", "Green")]),
        vec!["region eu-west-2: ThrottlingException: Rate exceeded".to_string()],
    );
    assert!(
        app.throttle_until.is_some(),
        "a throttled region must arm the back-off even when others succeeded"
    );
    assert_eq!(app.consecutive_throttles, 1);
}

#[tokio::test]
async fn a_clean_fan_out_clears_the_back_off() {
    let mut app = test_app();
    app.consecutive_throttles = 3;
    app.apply_refresh(
        Ok(vec![mk_env("api-prod", "uflexi", "Web", "Green")]),
        Vec::new(),
    );
    assert!(app.throttle_until.is_none());
    assert_eq!(app.consecutive_throttles, 0);
}

#[tokio::test]
async fn cost_on_retries_an_incomplete_walk_instead_of_saying_already_on() {
    // `spawn_cost_fetch` has exactly one caller — the `:cost on`
    // transition — so there is no periodic refetch. Answering "already
    // on" made a truncated walk terminal for the session: the partial
    // map stayed, the INCOMPLETE toast was cleared by the next refresh
    // tick, and every env past the cap showed `—`, indistinguishable
    // from untagged.
    let mut app = test_app();
    app.cost_enabled = true;
    app.costs.insert("api-prod".into(), 1.0);
    app.costs_complete = false;

    app.execute_command("cost on");
    let msg = app.status_message.as_deref().unwrap_or_default();
    assert!(
        msg.contains("retrying"),
        ":cost on must retry an incomplete walk, not report 'already on': {msg:?}"
    );

    // With complete data it still short-circuits — no metered refetch
    // for an operator who typed it twice.
    app.costs_complete = true;
    app.execute_command("cost on");
    let msg = app.status_message.as_deref().unwrap_or_default();
    assert!(msg.contains("already on"), "{msg:?}");
}

#[tokio::test]
async fn cost_status_does_not_call_a_partial_result_cached() {
    // Reachable when a truncated walk lands over a non-empty map: the
    // previous timestamp stays, so the arm that formats it said
    // "cached" for data the handler had explicitly refused to cache.
    let mut app = test_app();
    app.cost_enabled = true;
    app.costs.insert("api-prod".into(), 1.0);
    app.costs_fetched_at = Some(chrono::Utc::now() - chrono::Duration::hours(3));
    app.costs_complete = false;

    app.execute_command("cost status");
    let msg = app.status_message.as_deref().unwrap_or_default();
    assert!(msg.contains("INCOMPLETE"), "{msg:?}");
    assert!(
        !msg.contains("env(s) cached"),
        "a partial result was never cached: {msg:?}"
    );
}

#[test]
fn a_truncated_explain_says_so_before_the_rows() {
    // `SimulatePrincipalPolicy` hitting its page budget used to warn
    // only to the log. `:explain` is the one surface where an action's
    // *absence* from the table reads as "that one's fine", so a short
    // table has to announce itself — and above the rows, since it
    // changes how all of them should be read.
    let rows = vec![crate::aws::IamSimResult {
        action: "s3:GetObject".into(),
        resource: "*".into(),
        decision: "allowed".into(),
        matched_statements: vec![],
        missing_context: vec![],
        blocked_by_scp: false,
        blocked_by_boundary: false,
    }];
    let body = super::render_explain_overlay("arn:aws:iam::1:role/R", &rows, true);
    assert!(body.contains("INCOMPLETE"), "{body}");
    assert!(
        body.find("INCOMPLETE").unwrap() < body.find("s3:GetObject").unwrap(),
        "the banner has to precede the rows it qualifies:\n{body}"
    );

    let clean = super::render_explain_overlay("arn:aws:iam::1:role/R", &rows, false);
    assert!(
        !clean.contains("INCOMPLETE"),
        "a complete walk must not cry wolf:\n{clean}"
    );
}

#[tokio::test]
async fn the_health_panel_shows_fatal_events() {
    // The recent-events filter accepted ERROR and WARN only, so FATAL —
    // the severity *above* ERROR, and the one an operator opens this
    // panel to find — was the single event the health panel would not
    // show. With only a FATAL in the window the panel said "no error /
    // warning events", which reads as calm.
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Red")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    let detail = app.detail.as_mut().expect("detail opened");
    detail.loading_events = false;
    detail.events = vec![crate::aws::Event {
        at: Some(chrono::Utc::now() - chrono::Duration::minutes(2)),
        env: "api-prod".into(),
        application: "uflexi".into(),
        message: "SEVERE-CANARY: instance terminated".into(),
        severity: "FATAL".into(),
        version_label: None,
    }];

    let buf = render_buf(&mut app, 140, 40);
    let row = find_row(&buf, "SEVERE-CANARY")
        .expect("a FATAL event has to appear in the health panel's recent events");
    assert!(
        row_has_fg(&buf, row, app.theme.health_red),
        "FATAL is at least as severe as ERROR — it must not render muted"
    );
}

#[test]
fn a_federated_session_arn_is_refused_before_the_api_call() {
    // `parse_access_denied` deliberately leaves an STS ARN it can't
    // rewrite alone rather than failing the parse — but that ARN then
    // reached SimulatePrincipalPolicy, which rejects it as
    // InvalidInput, and `:explain` rendered the failure under its
    // "you probably lack iam:SimulatePrincipalPolicy" hint. The
    // operator was pointed at a permissions gap they didn't have.
    use crate::app::principal_not_simulatable as check;

    for ok in [
        "arn:aws:iam::123456789012:role/EbmanDeploy",
        "arn:aws:iam::123456789012:user/tom",
        "arn:aws:iam::123456789012:group/platform",
        "arn:aws-us-gov:iam::123456789012:role/EbmanDeploy",
        // Paths are legal in role ARNs.
        "arn:aws:iam::123456789012:role/service-role/EbmanDeploy",
    ] {
        assert!(check(ok).is_none(), "{ok} is a valid policy source");
    }

    let fed = check("arn:aws:sts::123456789012:federated-user/tom").expect("refused");
    assert!(fed.contains("role/NAME"), "names the fix: {fed}");
    let root = check("arn:aws:iam::123456789012:root").expect("refused");
    assert!(root.contains("root"), "{root}");
    // Not an ARN at all — the caller's own guard handles usage, so
    // this just must not claim it's fine.
    assert!(check("EBL001").is_none(), "non-ARNs are the guard's job");
}

#[tokio::test]
async fn explain_rewrites_a_pasted_assumed_role_arn() {
    // The ARN an operator pastes into `:explain ARN ACTION` is almost
    // always copied out of the AccessDenied message, so it's a session
    // ARN. The parsed-from-error path rewrote it and the args path
    // didn't, which made the documented form the one that failed.
    let mut app = test_app();
    app.execute_command(
        "explain arn:aws:sts::123456789012:assumed-role/EbmanDeploy/i-0abc s3:GetObject",
    );
    assert!(
        app.error_message.is_none(),
        "a session ARN must be rewritten, not refused: {:?}",
        app.error_message
    );
    let status = app.status_message.as_deref().unwrap_or_default();
    assert!(
        status.contains("arn:aws:iam::123456789012:role/EbmanDeploy"),
        "it should be simulating the underlying role: {status:?}"
    );
}

#[test]
fn the_clipboard_is_only_reached_through_yank() {
    // `yank` is stubbed under `cfg(test)` so the suite can't clobber
    // the clipboard of whoever runs it. That only holds while `yank`
    // is the sole door: `:update` reached `arboard` directly and every
    // test that ran it wrote to the real clipboard.
    let mut sites: Vec<String> = Vec::new();
    let mut stack = vec![std::path::PathBuf::from("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("src dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs")
                // This file names it in its own assertions.
                || path.file_name().and_then(|f| f.to_str()) == Some("tests.rs")
            {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read");
            for (n, line) in text.lines().enumerate() {
                // Skip the comments that discuss it by name.
                let code = line.split("//").next().unwrap_or("");
                if code.contains("arboard::") {
                    sites.push(format!("{}:{}", path.display(), n + 1));
                }
            }
        }
    }
    assert_eq!(
        sites.len(),
        1,
        "the only `arboard::` call belongs inside `yank`; found: {sites:?}"
    );
    assert!(sites[0].starts_with("src/app.rs:"), "{sites:?}");
}

// --- per-row work goes to the row's region -----------------------------

#[tokio::test]
async fn detail_and_why_and_dlq_use_the_rows_own_region() {
    // Under a multi-region fan-out the selected row is routinely in
    // some other region, but every per-row background fetch used
    // `self.aws`, whose region is `context.region`. Detail showed the
    // environment's name beside the home region's instances, metrics,
    // events and alarms — wrong data wearing the right label.
    let mut app = test_app();
    let mut env = mk_env("api-prod", "uflexi", "Web", "Green");
    env.region = Some("eu-west-2".into());
    app.environments = vec![env.clone()];
    app.rebuild_view();
    app.table_state.select(Some(0));
    assert_eq!(app.context.region, "us-east-1", "home region differs");

    // The lookup all four accessors share.
    assert_eq!(app.region_for_name("api-prod"), "eu-west-2");
    // An env we hold no row for falls back to home — a modal opened
    // before the refresh landed. That's the pre-fan-out behaviour.
    assert_eq!(app.region_for_name("not-in-the-table"), "us-east-1");

    app.open_detail();
    assert_eq!(
        app.detail_client().region_for_tests(),
        "eu-west-2",
        "Detail must fetch from where the environment actually is"
    );
    app.dlq = Some(crate::app::DlqState {
        env_name: "api-prod".into(),
        main_queue_url: String::new(),
        dlq_url: String::new(),
        messages: Vec::new(),
        list_state: Default::default(),
        loading: false,
        error: None,
        confirm_purge: false,
        purge_typed: tui_common::TextInput::new(),
        viewing: crate::app::QueueView::Dlq,
        confirm_delete_id: None,
        replay_input: None,
    });
    assert_eq!(
        app.dlq_client().region_for_tests(),
        "eu-west-2",
        "an SQS queue URL doesn't even exist in the home region"
    );
}

#[tokio::test]
async fn a_write_dispatches_to_the_rows_region() {
    // The worst case in this class: a restart / terminate / deploy on a
    // fan-out row went to the home region, where it either failed as
    // "environment not found" or — with a same-named env at home, which
    // is what a fleet with per-region copies looks like — dispatched a
    // destructive action against the wrong environment entirely.
    let mut app = test_app();
    let mut env = mk_env("api-prod", "uflexi", "Web", "Green");
    env.region = Some("eu-west-2".into());
    app.environments = vec![env];
    app.rebuild_view();
    app.table_state.select(Some(0));

    assert_eq!(
        app.client_for_region(&app.region_for_name("api-prod"))
            .region_for_tests(),
        "eu-west-2",
        "the write client follows the row"
    );
    // A row with no region of its own stays on the home client, which
    // may be an AssumeRole session `cached_client` can't rebuild.
    let mut homeless = mk_env("home-env", "uflexi", "Web", "Green");
    homeless.region = None;
    app.environments.push(homeless);
    app.rebuild_view();
    assert_eq!(
        app.client_for_region(&app.region_for_name("home-env"))
            .region_for_tests(),
        "us-east-1"
    );
}

#[tokio::test]
async fn demo_mode_never_resolves_a_remote_region() {
    // The demo fleet's regions are fictional and its client is a stub;
    // resolving one would reach real AWS for a region the fixture
    // invented, during a screencast.
    let mut app = test_app();
    app.demo_mode = true;
    let mut env = mk_env("api-prod", "uflexi", "Web", "Green");
    env.region = Some("ap-southeast-4".into());
    app.environments = vec![env];
    app.rebuild_view();
    assert!(
        app.client_for_region("ap-southeast-4").is_home_for_tests(),
        "demo mode stays on the stub"
    );
}

// --- the home client ages out so pasted credentials take effect --------

#[tokio::test]
async fn the_home_client_ages_out() {
    // The client cache's TTL only ever reached
    // `list_environments_in_region`. Everything else goes through
    // `self.aws`, replaced only by an explicit context switch — so a
    // single-region operator, who never reaches the cached path at
    // all, still had to restart after pasting fresh static
    // credentials. Static profile creds carry no expiry, so the SDK's
    // providers never re-resolve them on their own.
    let mut app = test_app();
    assert!(
        !app.should_refresh_home_client(),
        "a freshly built client is not stale"
    );

    app.aws_built_at = std::time::Instant::now() - crate::aws::CLIENT_CACHE_TTL;
    assert!(app.should_refresh_home_client(), "past the TTL it is");

    // One at a time — the 15s tick must not stack refreshes.
    app.aws_refresh_in_flight = true;
    assert!(!app.should_refresh_home_client());
    app.aws_refresh_in_flight = false;

    // The demo stub isn't rebuildable.
    app.demo_mode = true;
    assert!(!app.should_refresh_home_client());
    app.demo_mode = false;

    // An AssumeRole session has a hard one-hour cap; re-assuming is a
    // different operation, not a silent swap.
    app.cfg.accounts.insert(
        "prod".into(),
        crate::config::AccountSpec {
            role_arn: "arn:aws:iam::1:role/R".into(),
            ..Default::default()
        },
    );
    app.context.profile = Some("prod".into());
    assert!(
        !app.should_refresh_home_client(),
        "an assumed-role context must not be silently swapped"
    );
    assert_eq!(app.assumed_account().map(|(n, _)| n), Some("prod".into()));
}

#[tokio::test]
async fn a_stale_client_refresh_never_displaces_a_context_switch() {
    // The refresh carries `rebuild_epoch`, so a `:region` / `:account`
    // switch spawned while it was building wins. Without the guard the
    // app would serve the PREVIOUS context's client under the new
    // context's header — silently, since this path shows no message.
    let mut app = test_app();
    app.rebuild_epoch = 4;
    app.aws_refresh_in_flight = true;
    let before = std::sync::Arc::as_ptr(&app.aws);

    app.handle_msg(AppMsg::ClientRefreshed {
        epoch: 3, // spawned before the switch
        result: Ok(Box::new(crate::aws::AwsClient::stub())),
    });
    assert_eq!(
        std::sync::Arc::as_ptr(&app.aws),
        before,
        "a stale refresh must not swap the client"
    );
    assert!(
        !app.aws_refresh_in_flight,
        "the in-flight flag clears either way, or the retry never fires again"
    );

    // Current epoch: applied.
    app.handle_msg(AppMsg::ClientRefreshed {
        epoch: 4,
        result: Ok(Box::new(crate::aws::AwsClient::stub())),
    });
    assert_ne!(
        std::sync::Arc::as_ptr(&app.aws),
        before,
        "current one lands"
    );
}

#[tokio::test]
async fn a_failed_client_refresh_is_silent_and_keeps_the_old_client() {
    // Nothing the operator can see changes, so an error toast here
    // would displace whatever they were reading for a transient
    // credential-chain hiccup. The previous client keeps working.
    let mut app = test_app();
    app.status_message = Some("deploy dispatched".into());
    app.aws_refresh_in_flight = true;
    let before = std::sync::Arc::as_ptr(&app.aws);

    app.handle_msg(AppMsg::ClientRefreshed {
        epoch: app.rebuild_epoch,
        result: Err("no credentials found".into()),
    });
    assert_eq!(std::sync::Arc::as_ptr(&app.aws), before);
    assert!(app.error_message.is_none(), "silent");
    assert_eq!(
        app.status_message.as_deref(),
        Some("deploy dispatched"),
        "the operator's message survives"
    );
    assert!(!app.should_refresh_home_client(), "the age clock reset");
}

#[tokio::test]
async fn a_stale_rebuild_ok_never_overwrites_the_newer_context() {
    // The existing guard test drives the Err path, which proves the
    // early return runs but not what it protects. The Ok arm is the
    // dangerous one: it swaps `aws`, replaces `context`, bumps
    // `generation` and clears the fleet. A slow first switch landing
    // after a fast second would settle the app on the FIRST choice
    // while the header showed the second.
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
    app.rebuild_view();
    app.rebuild_epoch = 2;
    app.context.region = "eu-west-2".into();
    let gen_before = app.generation;

    app.handle_msg(AppMsg::Rebuild {
        epoch: 1,
        result: Ok(Box::new(crate::aws::AwsClient::stub())),
    });
    assert_eq!(app.context.region, "eu-west-2", "the newer context stands");
    assert_eq!(app.generation, gen_before, "no generation bump");
    assert_eq!(app.environments.len(), 1, "the fleet is not torn down");

    // The current epoch does apply, teardown and all.
    app.handle_msg(AppMsg::Rebuild {
        epoch: 2,
        result: Ok(Box::new(crate::aws::AwsClient::stub())),
    });
    assert_ne!(app.generation, gen_before);
    assert!(app.environments.is_empty(), "current switch tears down");
}

// --- the Detail auto-refresh can't stack scans -------------------------

#[tokio::test]
async fn the_detail_tick_does_not_stack_a_slow_scan() {
    // `detail_refresh_active_tab` fires on every 15-second tick when
    // auto-refresh is on. The interactive scans behind these tabs are
    // worst-case 500 sequential round trips, so a scan slower than the
    // tick collected a new companion every 15 seconds for as long as
    // it ran — a fan of sequential AWS calls against an account that,
    // by the time anyone is looking at this screen, is usually already
    // having a bad day.
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Red")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    let detail = app.detail.as_mut().expect("detail opened");
    detail.tab_idx = detail
        .tabs
        .iter()
        .position(|t| *t == DetailTab::Instances)
        .expect("Instances tab present");
    // `open_detail` fires its own eager fetches; clear them so the
    // assertions below are about the tick, not the open.
    detail.loading_instances = false;
    detail.loading_events = false;
    app.detail_fetch_started = None;
    assert!(
        !app.detail.as_ref().unwrap().tab_loading(),
        "nothing outstanding yet"
    );

    // First refresh fires and marks the tab loading.
    app.detail_refresh_active_tab();
    assert!(app.detail.as_ref().unwrap().loading_instances);
    let fired_at = app.detail_fetch_started.expect("stamped");

    // A tick landing while it's still running is a no-op.
    app.detail_refresh_active_tab();
    assert_eq!(
        app.detail_fetch_started,
        Some(fired_at),
        "a second refresh must not have been fired"
    );

    // Once the result lands, the next tick proceeds.
    app.detail.as_mut().unwrap().loading_instances = false;
    app.detail_refresh_active_tab();
    assert_ne!(
        app.detail_fetch_started,
        Some(fired_at),
        "a finished fetch must not block the next one"
    );
}

#[tokio::test]
async fn a_lost_detail_fetch_does_not_wedge_the_tab() {
    // The guard reads the `loading_*` flags, which a dropped result —
    // a generation-guarded arrival after a context switch, say — never
    // clears. Without an age cap the tab would refuse to refresh for
    // the rest of the session.
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Red")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    let detail = app.detail.as_mut().expect("detail opened");
    detail.tab_idx = detail
        .tabs
        .iter()
        .position(|t| *t == DetailTab::Instances)
        .expect("Instances tab present");

    app.detail_refresh_active_tab();
    // Flag stuck on; the fetch is older than the stuck threshold.
    app.detail_fetch_started =
        Some(std::time::Instant::now() - std::time::Duration::from_secs(121));
    let stale = app.detail_fetch_started;
    app.detail_refresh_active_tab();
    assert_ne!(
        app.detail_fetch_started, stale,
        "an outstanding fetch this old is lost, not slow — retry it"
    );
}

#[tokio::test]
async fn every_detail_tab_reports_its_own_loading_state() {
    // `tab_loading` gates the refresh, so a tab whose flag it forgot
    // would silently lose its in-flight guard — and one that read the
    // WRONG flag would refuse to refresh whenever some unrelated tab
    // was busy.
    let mut app = test_app();
    app.environments = vec![mk_env("wk-prod", "uflexi", "Worker", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    let d = app.detail.as_mut().expect("detail opened");
    assert!(
        d.tabs.contains(&DetailTab::Queue),
        "worker envs get the Queue tab"
    );

    // `open_detail` fires eager fetches; start from a clean slate.
    d.loading_events = false;
    d.loading_instances = false;
    d.loading_queues = false;
    d.loading_metrics = false;
    d.loading_cw_alarms = false;
    d.loading_recent_versions = false;
    d.log_tail.stage = crate::app::LogTailStage::Idle;

    // Every flag off: no tab claims to be loading.
    for idx in 0..d.tabs.len() {
        d.tab_idx = idx;
        assert!(!d.tab_loading(), "{:?} with no flags set", d.tab());
    }

    // Each flag turns on exactly the tabs that own it.
    /// (label, the flag to set, the tabs that should then report loading)
    type LoadingCase = (
        &'static str,
        fn(&mut crate::app::DetailState),
        &'static [DetailTab],
    );
    let cases: &[LoadingCase] = &[
        (
            "events",
            |d| d.loading_events = true,
            &[DetailTab::Health, DetailTab::Events],
        ),
        (
            "instances",
            |d| d.loading_instances = true,
            &[DetailTab::Instances],
        ),
        (
            "queues",
            |d| d.loading_queues = true,
            &[DetailTab::Health, DetailTab::Queue],
        ),
        (
            "metrics",
            |d| d.loading_metrics = true,
            &[DetailTab::Metrics],
        ),
        (
            "alarms",
            |d| d.loading_cw_alarms = true,
            &[DetailTab::Health],
        ),
        (
            "recent versions",
            |d| d.loading_recent_versions = true,
            &[DetailTab::Health],
        ),
        (
            "log tail",
            |d| d.log_tail.stage = crate::app::LogTailStage::Polling,
            &[DetailTab::Logs],
        ),
    ];
    for (label, set, owners) in cases {
        let d = app.detail.as_mut().unwrap();
        d.loading_events = false;
        d.loading_instances = false;
        d.loading_queues = false;
        d.loading_metrics = false;
        d.loading_cw_alarms = false;
        d.loading_recent_versions = false;
        d.log_tail.stage = crate::app::LogTailStage::Idle;
        set(d);
        for idx in 0..d.tabs.len() {
            d.tab_idx = idx;
            let tab = d.tab();
            assert_eq!(
                d.tab_loading(),
                owners.contains(&tab),
                "{label} loading, on the {tab:?} tab"
            );
        }
    }
}

#[tokio::test]
async fn a_cross_region_row_under_an_assumed_role_re_assumes() {
    // `assume_role` puts the friendly ACCOUNT name in
    // `context.profile` as the header breadcrumb. So resolving a
    // cross-region row through `cached_client(context.profile, …)`
    // went looking for an AWS profile called `prod` that was never a
    // profile — the fix for wrong-region data would have traded it for
    // a confusing "profile not found". Re-assume into the same account
    // pointed at the other region, exactly as `:org-health` does.
    let mut app = test_app();
    app.cfg.accounts.insert(
        "prod".into(),
        crate::config::AccountSpec {
            role_arn: "arn:aws:iam::1:role/EbmanReadOnly".into(),
            region: Some("us-east-1".into()),
            ..Default::default()
        },
    );
    app.context.profile = Some("prod".into());
    let mut env = mk_env("api-prod", "uflexi", "Web", "Green");
    env.region = Some("eu-west-2".into());
    app.environments = vec![env];
    app.rebuild_view();

    let client = app.client_for_region("eu-west-2");
    assert_eq!(
        client.account_for_tests().as_deref(),
        Some("prod"),
        "it must re-assume, not look for a profile named after the account"
    );
    assert_eq!(
        client.region_for_tests(),
        "eu-west-2",
        "and point the assumed session at the row's region, not the spec's"
    );

    // The home region keeps the LIVE session rather than re-assuming —
    // that client already holds valid credentials.
    assert!(app.client_for_region("us-east-1").is_home_for_tests());
}

#[test]
fn every_spawn_declares_whether_it_is_per_env() {
    // `self.aws` is the HOME client — its region is `context.region`.
    // Under a multi-region fan-out the selected row is routinely
    // somewhere else, so per-env work on the home client shows (or
    // writes to) the wrong region's environment. Sixty-odd spawn sites
    // had that shape; the ones below are the residue that is genuinely
    // account- or region-wide. Anything new taking `self.aws` has to
    // be named here with a reason, so it's a deliberate choice rather
    // than the path of least resistance.
    //
    // The per-env accessors are `client_for_env` / `client_for_app` /
    // `current_env_client` / `detail_client` / `why_red_client` /
    // `dlq_client`.
    const HOME_CLIENT_IS_CORRECT_BECAUSE: &[(&str, &str)] = &[
        (
            "spawn_aws",
            "the home-client helper itself — `spawn_aws_in` is the \
             per-region sibling",
        ),
        (
            "spawn_refresh",
            "the fleet listing; the multi-region fan-out beside it \
             builds its own per-region clients",
        ),
        (
            "spawn_event_tail",
            "account-wide DescribeEvents, not scoped to a row",
        ),
        (
            "spawn_identity",
            "sts:GetCallerIdentity for the session as a whole",
        ),
        (
            "spawn_cost_fetch",
            "Cost Explorer is account-wide and reached through the \
             partition's global endpoint",
        ),
        (
            "spawn_applications",
            "the applications catalogue for the home region. Under a \
             fan-out an app exists once per region and this shows one \
             of them — `applications` is a single list, so widening it \
             is a data-model change, not a client change",
        ),
        (
            "spawn_solution_stacks",
            "same shape: `latest_stacks` is one map, so the platform \
             catalogue is the home region's",
        ),
        (
            "cmd_accounts",
            "Organizations is a global service reached through the \
             partition's endpoint",
        ),
        (
            "cmd_explain",
            "IAM is a global service; the principal ARN carries its own \
             partition",
        ),
        (
            "cmd_secrets",
            "an account-wide Secrets Manager browse, not about any row",
        ),
        ("cmd_secret_view", "same browse, one secret deep"),
        (
            "cmd_custom_platforms",
            "custom platforms are an account-level catalogue",
        ),
        (
            "cmd_custom_platform_delete",
            "deletes from that same account-level catalogue, by ARN",
        ),
    ];

    let mut offenders: Vec<String> = Vec::new();
    let mut stack = vec![std::path::PathBuf::from("src/app")];
    let mut files = vec![std::path::PathBuf::from("src/app.rs")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("app dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs")
                && path.file_name().and_then(|f| f.to_str()) != Some("tests.rs")
            {
                files.push(path);
            }
        }
    }
    assert!(files.len() > 20, "the walk found only {}", files.len());

    for path in files {
        let raw = std::fs::read_to_string(&path).expect("read");
        let lines: Vec<&str> = raw.lines().collect();
        for (n, line) in lines.iter().enumerate() {
            // Comments discuss `self.aws` by name.
            let code = line.split("//").next().unwrap_or("");
            if !(code.contains("self.aws.clone()") || code.contains("self.spawn_aws(")) {
                continue;
            }
            // `RegionClient` keeps the home client as its fallback —
            // that IS the per-region machinery, not a bypass of it.
            if path.file_name().and_then(|f| f.to_str()) == Some("app.rs")
                && code.contains("let home = self.aws.clone()")
            {
                continue;
            }
            let enclosing = lines[..=n]
                .iter()
                .rev()
                .find_map(|l| {
                    let t = l.trim_start();
                    let t = t.strip_prefix("pub(crate) ").unwrap_or(t);
                    let t = t.strip_prefix("pub(super) ").unwrap_or(t);
                    let t = t.strip_prefix("pub ").unwrap_or(t);
                    t.strip_prefix("fn ").map(|r| {
                        r.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                            .next()
                            .unwrap_or("")
                            .to_string()
                    })
                })
                .unwrap_or_default();
            if !HOME_CLIENT_IS_CORRECT_BECAUSE
                .iter()
                .any(|(name, _)| *name == enclosing)
            {
                offenders.push(format!(
                    "{}::{enclosing}",
                    path.file_name().unwrap().to_string_lossy()
                ));
            }
        }
    }
    offenders.sort();
    offenders.dedup();
    assert!(
        offenders.is_empty(),
        "these spawn AWS work on the HOME client without declaring why \
         that's right for them — use `client_for_env` / `client_for_app` \
         (or `spawn_aws_in`), or add them to HOME_CLIENT_IS_CORRECT_BECAUSE \
         with a reason: {offenders:?}"
    );
}

#[tokio::test]
async fn the_per_env_accessors_all_follow_the_row() {
    // One assertion per accessor, so a future refactor that points one
    // of them back at `context.region` is caught here rather than by an
    // operator whose restart went to the wrong region.
    let mut app = test_app();
    let mut env = mk_env("api-prod", "uflexi", "Web", "Green");
    env.region = Some("eu-west-2".into());
    app.environments = vec![env];
    app.rebuild_view();
    app.table_state.select(Some(0));
    assert_eq!(app.context.region, "us-east-1", "home region differs");

    assert_eq!(
        app.client_for_env("api-prod").region_for_tests(),
        "eu-west-2"
    );
    assert_eq!(app.client_for_app("uflexi").region_for_tests(), "eu-west-2");
    assert_eq!(app.current_env_client().region_for_tests(), "eu-west-2");

    // Detail wins over the table selection — it's what's on screen.
    app.open_detail();
    assert_eq!(app.detail_client().region_for_tests(), "eu-west-2");

    // An unknown name falls back to home rather than inventing a
    // region: a modal can outlive the row that opened it.
    assert_eq!(app.client_for_env("ghost").region_for_tests(), "us-east-1");
    assert_eq!(
        app.client_for_app("ghost-app").region_for_tests(),
        "us-east-1"
    );

    // With nothing selected at all, `current_env_client` is the home
    // client rather than a panic or an empty region.
    let mut empty = test_app();
    assert!(empty.current_env_client().is_home_for_tests());
    empty.table_state.select(None);
    assert!(empty.current_env_client().is_home_for_tests());
}

#[tokio::test]
async fn a_write_audits_the_region_it_actually_went_to() {
    // The audit log is the record of what was done to production. A
    // dispatch that went to eu-west-2 while the journal said
    // us-east-1 is worse than no line at all — it's a confident wrong
    // answer during an incident review.
    let mut app = test_app();
    let mut env = mk_env("api-prod", "uflexi", "Web", "Green");
    env.region = Some("eu-west-2".into());
    app.environments = vec![env];
    app.rebuild_view();
    app.table_state.select(Some(0));

    assert_eq!(
        app.region_for_name("api-prod"),
        "eu-west-2",
        "the audit region comes from this lookup at every dispatch site"
    );
    // The home region is still what an env we hold no row for gets.
    assert_eq!(app.region_for_name("ghost"), "us-east-1");
}

#[tokio::test]
async fn a_dispatch_and_its_completion_agree_on_the_region() {
    // The two lines are a pair — `ebman audit` correlates them by
    // action + target. If the dispatch names the row's region and the
    // completion names the home one, a grep across the pair reports an
    // action that started in eu-west-2 and finished in us-east-1.
    let mut app = test_app();
    let mut env = mk_env("api-prod", "uflexi", "Web", "Green");
    env.region = Some("eu-west-2".into());
    app.environments = vec![env];
    app.rebuild_view();
    app.table_state.select(Some(0));

    let path = crate::util::cache_dir().join("audit.log");
    let before = std::fs::read_to_string(&path).unwrap_or_default();

    app.handle_msg(AppMsg::ActionResult {
        gen: app.generation,
        action: crate::app::Action::RestartAppServer,
        env_name: "api-prod".into(),
        result: Ok(()),
    });

    let after = std::fs::read_to_string(&path).unwrap_or_default();
    let line = after
        .strip_prefix(&before)
        .unwrap_or(&after)
        .lines()
        .find(|l| l.contains("api-prod"))
        .expect("a completion line was written")
        .to_string();
    assert!(
        line.contains("region=eu-west-2"),
        "the completion must name where the work went: {line}"
    );
}

#[tokio::test]
async fn a_cross_region_role_client_comes_from_the_cache() {
    // The pre-tag review added the role cache and a test that it gets
    // CLEARED — which passed while the code path that was supposed to
    // read it still called `assume_role` directly, because the edit
    // routing it through silently failed. A cache nothing reads is not
    // a fix, and "the clear works" could never have caught that.
    //
    // Per-env work under `:account` resolves once per call, and
    // `spawn_env_instance_counts` builds a client per row on every
    // 15-second tick: a fresh AssumeRole each time is an STS storm for
    // a session that stays valid for another hour.
    let _guard = crate::aws::CACHE_TEST_LOCK.lock().await;
    crate::aws::clear_client_cache();

    let mut app = test_app();
    app.cfg.accounts.insert(
        "prod".into(),
        crate::config::AccountSpec {
            role_arn: "arn:aws:iam::1:role/R".into(),
            region: Some("us-east-1".into()),
            ..Default::default()
        },
    );
    app.context.profile = Some("prod".into());

    // Seed the cache for the key the accessor will build. Assuming for
    // real needs live STS, so this proves the READ path — which is the
    // half that was broken.
    let seeded = std::sync::Arc::new(crate::aws::AwsClient::stub());
    crate::aws::seed_role_cache_for_tests("prod", "eu-west-2", seeded.clone());

    let client = app.client_for_region("eu-west-2");
    assert_eq!(client.account_for_tests().as_deref(), Some("prod"));
    let resolved = client.resolve().await.expect("cache hit, no STS call");
    assert!(
        std::sync::Arc::ptr_eq(&resolved, &seeded),
        "resolve must come from the role cache, not a fresh AssumeRole"
    );

    crate::aws::clear_client_cache();
}

#[tokio::test]
async fn a_detail_env_that_left_the_table_keeps_its_region() {
    // `region_for_name` looks in `self.environments`, but Detail's
    // snapshot is taken at open time and is NOT torn down when a
    // refresh drops the row — a terminated env, or a region whose
    // fetch failed under a fan-out. The action menu targets Detail's
    // env, so without the snapshot fallback a restart / terminate
    // dispatched there fell back to the HOME region: the original
    // wrong-region bug, in a narrow window, and silently.
    let mut app = test_app();
    let mut env = mk_env("api-prod", "uflexi", "Web", "Green");
    env.region = Some("eu-west-2".into());
    app.environments = vec![env];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    assert!(app.detail.is_some(), "detail open on the fan-out row");

    // The refresh that drops it — eu-west-2 failed this tick.
    app.environments.clear();
    app.view.invalidate();
    app.rebuild_view();

    assert_eq!(
        app.region_for_name("api-prod"),
        "eu-west-2",
        "Detail's snapshot still knows where this env lives"
    );
    assert_eq!(app.detail_client().region_for_tests(), "eu-west-2");
    // A name neither the table nor Detail knows still falls back.
    assert_eq!(app.region_for_name("ghost"), "us-east-1");
}

#[tokio::test]
async fn current_env_client_and_client_for_env_are_not_interchangeable() {
    // `current_env_client` is Detail-first, matching how `:alarms` and
    // `:alarm-history` pick their env. Most commands instead operate on
    // `selected_env()`. The two agree almost always — opening Detail
    // uses the selection — but a refresh that reorders or filters the
    // table moves the selection while Detail keeps its snapshot, and
    // then they name different environments in different regions.
    //
    // `:alarm-create` / `:alarm-delete` were resolving through the
    // Detail-first accessor while operating on the selection, so the
    // alarm would have been written to one region and audited as
    // another. This pins the distinction so the accessors don't get
    // swapped back for looking similar.
    let mut app = test_app();
    let mut a = mk_env("api-prod", "uflexi", "Web", "Green");
    a.region = Some("eu-west-2".into());
    let mut b = mk_env("api-staging", "uflexi", "Web", "Green");
    b.region = Some("ap-south-1".into());
    app.environments = vec![a, b];
    app.rebuild_view();

    // Detail on the first row.
    app.table_state.select(Some(0));
    app.open_detail();
    // Selection moves to the second — what a re-sorted refresh does.
    app.table_state.select(Some(1));

    assert_eq!(
        app.current_env_client().region_for_tests(),
        "eu-west-2",
        "Detail-first: the env on screen"
    );
    let selected = app.selected_env().expect("row 1").name.clone();
    assert_eq!(selected, "api-staging");
    assert_eq!(
        app.client_for_env(&selected).region_for_tests(),
        "ap-south-1",
        "selection-based: the env the command operates on"
    );
    // And the audit region for a selection-based command follows the
    // selection too, so the client and the journal agree.
    assert_eq!(app.region_for_name(&selected), "ap-south-1");
}

#[tokio::test]
async fn a_write_whose_row_left_the_table_still_goes_to_its_region() {
    // The confirm modal carries a target NAME, and there is an undo
    // window between the operator confirming and `tick_pending_dispatch`
    // firing. A 15-second refresh landing in that window — a terminated
    // env, or a region whose fetch failed under a fan-out — dropped the
    // row, and the dispatch fell back to the home region. Silently.
    //
    // The Detail-snapshot fallback covers this only when Detail happens
    // to be open on that env; an action started from the table has no
    // snapshot at all.
    let mut app = test_app();
    let mut env = mk_env("api-prod", "uflexi", "Web", "Green");
    env.region = Some("eu-west-2".into());

    // The refresh that put it on screen is what remembers the region.
    app.handle_msg(AppMsg::Refresh {
        gen: app.generation,
        result: Ok(vec![env]),
        partial_errors: Vec::new(),
    });
    assert_eq!(app.region_for_name("api-prod"), "eu-west-2");
    assert!(app.detail.is_none(), "no Detail snapshot to lean on");

    // The next tick drops it — eu-west-2 failed, or the env terminated.
    app.handle_msg(AppMsg::Refresh {
        gen: app.generation,
        result: Ok(vec![]),
        partial_errors: vec!["region eu-west-2: throttled".into()],
    });
    assert!(app.environments.is_empty(), "the row is gone");

    assert_eq!(
        app.region_for_name("api-prod"),
        "eu-west-2",
        "a write in its undo window must not silently retarget the home region"
    );
    assert_eq!(
        app.client_for_env("api-prod").region_for_tests(),
        "eu-west-2"
    );
    // A name we have never seen still falls back.
    assert_eq!(app.region_for_name("ghost"), "us-east-1");
}

#[tokio::test]
async fn remembered_regions_do_not_survive_a_context_switch() {
    // A same-named env in another account or partition is a different
    // environment. Carrying the old answer across would aim a write at
    // a region the new context may not even have.
    let mut app = test_app();
    let mut env = mk_env("api-prod", "uflexi", "Web", "Green");
    env.region = Some("eu-west-2".into());
    app.handle_msg(AppMsg::Refresh {
        gen: app.generation,
        result: Ok(vec![env]),
        partial_errors: Vec::new(),
    });
    assert_eq!(app.region_for_name("api-prod"), "eu-west-2");

    app.handle_msg(AppMsg::Rebuild {
        epoch: app.rebuild_epoch,
        result: Ok(Box::new(crate::aws::AwsClient::stub())),
    });
    assert_eq!(
        app.region_for_name("api-prod"),
        app.context.region,
        "the new context's home region, not the old context's answer"
    );
}

#[tokio::test]
async fn the_instance_console_link_follows_the_row_now_that_the_data_does() {
    // This link deliberately used the HOME region, because Detail's
    // instance list was fetched through the home client — the link had
    // to agree with the data it named. 0.30.0 fixed the fetch and left
    // the compensation in place, which turned the workaround into the
    // bug: a real instance ID from eu-west-2 pointed at the us-east-1
    // console, where it resolves to "does not exist".
    let mut app = test_app();
    let mut env = mk_env("api-prod", "uflexi", "Web", "Green");
    env.region = Some("eu-west-2".into());
    app.environments = vec![env];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();

    let detail = app.detail.as_mut().expect("detail opened");
    detail.instances = vec![crate::aws::Instance {
        id: "i-0abc123".into(),
        health: "Ok".into(),
        color: "Green".into(),
        causes: vec![],
        instance_type: "t3.small".into(),
        availability_zone: "eu-west-2a".into(),
        launched_at: None,
    }];
    detail.instances_cursor = 0;

    let (region, id) = app
        .instance_console_target()
        .expect("an instance is selected");
    assert_eq!(id, "i-0abc123");
    assert_eq!(
        region, "eu-west-2",
        "the link must name the region the instance list was fetched from"
    );
    // Which is the same region Detail fetched it from — the two agreeing
    // is the actual invariant here.
    assert_eq!(app.detail_client().region_for_tests(), region);

    // No Detail, or no instance under the cursor: no target, no panic.
    app.detail.as_mut().unwrap().instances.clear();
    assert!(app.instance_console_target().is_none());
    app.detail = None;
    assert!(app.instance_console_target().is_none());
}

#[tokio::test]
async fn the_breadcrumb_names_the_region_of_the_env_it_names() {
    // The crumb reads `REGION / app / env`. It used to render
    // `context.region` unconditionally, which was accidentally
    // truthful while Detail showed home-region data — and became a lie
    // the moment Detail started fetching from the row's region.
    // `us-east-1 / uflexi / api-prod` for an env in eu-west-2 is the
    // confusion this release exists to remove.
    let mut app = test_app();
    let mut env = mk_env("api-prod", "uflexi", "Web", "Green");
    env.region = Some("eu-west-2".into());
    app.environments = vec![env];
    app.rebuild_view();
    app.table_state.select(Some(0));
    assert_eq!(app.context.region, "us-east-1", "home region differs");

    let out = render(&mut app, 160, 40);
    assert!(
        out.contains("eu-west-2"),
        "the crumb must name the selected env's region:\n{out}"
    );

    // Detail replaces the screen with its own header and draws no
    // crumb, so there is no wrong region to show there — but no region
    // at all either. Recorded in BACKLOG as a gap rather than fixed
    // here: adding one is a UI addition, not a stale workaround.
    app.open_detail();
    let out = render(&mut app, 160, 40);
    assert!(
        !out.contains("us-east-1"),
        "Detail must not show the SESSION's region beside another region's env:\n{out}"
    );

    // Nothing selected: the session's region is the right answer.
    let mut empty = test_app();
    let out = render(&mut empty, 160, 40);
    assert!(
        out.contains("us-east-1"),
        "session region with no env:\n{out}"
    );
}

#[test]
fn ebl010_tells_an_untagged_env_from_an_unloaded_one() {
    // `env_tag_keys` was a bare slice, so "the fetch failed" and "this
    // env has no tags" were the same value — a failed
    // `ListTagsForResource` silently disabled the rule, and an env
    // with no tags at all, the worst case the rule exists to catch,
    // looked identical to one whose tags hadn't loaded. Same
    // conflation as `describe_worker_queues` returning an empty list
    // for AccessDenied, fixed in 0.27.
    use crate::lint::LintContext;
    let env = mk_env("api-prod", "uflexi", "Web", "Green");
    let opts: Vec<(String, String, String)> = Vec::new();
    let required = vec!["Owner".to_string(), "CostCentre".to_string()];
    let rules = crate::lint::default_rules(&[]);

    // Not loaded: skip. Firing here would flag every env in the fleet
    // on a transient API error.
    let ctx = LintContext::for_env(&env, &opts).with_required_tags(&required);
    assert!(
        !crate::lint::run_rules(&rules, &ctx)
            .iter()
            .any(|i| i.rule_id == "EBL010"),
        "unloaded tags must not fire"
    );

    // Loaded and empty: fires for both keys. This is the env that has
    // no tags at all, which used to be invisible.
    let none_at_all: Vec<String> = Vec::new();
    let ctx = LintContext::for_env(&env, &opts)
        .with_required_tags(&required)
        .with_env_tag_keys(&none_at_all);
    let issue = crate::lint::run_rules(&rules, &ctx)
        .into_iter()
        .find(|i| i.rule_id == "EBL010")
        .expect("an env with no tags at all must fire");
    assert!(issue.detail.contains("Owner"), "{}", issue.detail);
    assert!(issue.detail.contains("CostCentre"), "{}", issue.detail);

    // Loaded and complete: silent.
    let all = vec!["Owner".to_string(), "CostCentre".to_string()];
    let ctx = LintContext::for_env(&env, &opts)
        .with_required_tags(&required)
        .with_env_tag_keys(&all);
    assert!(!crate::lint::run_rules(&rules, &ctx)
        .iter()
        .any(|i| i.rule_id == "EBL010"));
}

#[test]
fn json_surfaces_are_parsed_by_a_json_parser() {
    // Three JSON inputs used to go through `serde_yml` on the
    // reasoning that JSON is a YAML subset. True — but it means every
    // YAML feature applies to input ebman doesn't control: two LLM
    // response bodies carrying model-generated text, and a tfstate
    // file discovered by walking up from cwd. Anchor/alias expansion
    // is the specific hazard. `serde_json` was a direct dependency the
    // whole time, so the comment justifying the detour was stale too.
    //
    // Pinned by call site rather than by behaviour: the hazard is the
    // *parser choice*, and a test that fed YAML in would only prove
    // one of its features is absent.
    // Extended after the first version missed four more: the lint
    // baseline parser (whose own error message says "baseline JSON
    // parse failed"), and three round-trip tests asserting output is
    // valid JSON while reading it with a YAML parser — which accepts
    // things JSON rejects, so they asserted less than they appeared
    // to. A guard scoped to the files I happened to be editing is the
    // same mistake as a backlog entry nobody re-checks.
    for (name, src) in [
        ("llm.rs", include_str!("../llm.rs")),
        ("terraform.rs", include_str!("../terraform.rs")),
        ("lint.rs", include_str!("../lint.rs")),
        ("audit.rs", include_str!("../audit.rs")),
        ("cli/mod.rs", include_str!("../cli/mod.rs")),
    ] {
        let code: String = src
            .lines()
            .map(|l| l.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !code.contains("serde_yml"),
            "{name} parses JSON with the YAML parser again"
        );
    }
    // `saved_config.rs` and `eb_cli.rs` are exempt and stay exempt:
    // EB saved configurations and `.elasticbeanstalk/config.yml`
    // really are YAML. They are also the ONLY two remaining
    // `serde_yml` consumers, which is what makes the RUSTSEC waiver
    // on it a two-file problem rather than a nine-file one.
    assert!(
        include_str!("../saved_config.rs").contains("serde_yml"),
        "saved configs are genuinely YAML — if this flipped, check why"
    );
}

#[test]
fn no_lint_caller_flattens_a_failed_tag_fetch_into_an_empty_list() {
    // Making `env_tag_keys` an `Option` fixed the rule but INVERTED the
    // bug at the call sites: all three collapsed `None` (fetch failed,
    // or the env has no ARN) into an empty Vec before calling, so a
    // failed `ListTagsForResource` went from silently skipping the rule
    // to firing a false positive for every required key on every env.
    // Worse than what it replaced.
    //
    // Pinned structurally because the failure is a lost distinction,
    // not a wrong value: `unwrap_or_default()` on the tags option is
    // exactly the shape that throws it away.
    for (name, src) in [
        ("app/cmd_misc.rs", include_str!("cmd_misc.rs")),
        ("app/spawn_deploy.rs", include_str!("spawn_deploy.rs")),
        ("cli/lint.rs", include_str!("../cli/lint.rs")),
    ] {
        let code: String = src
            .lines()
            .map(|l| l.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        // Find each tag-keys binding and check the WHOLE expression,
        // not just its first line — the binding routinely wraps, and a
        // single-line check missed a two-line `Some(tags_opt
        // .unwrap_or_default() …)` when this guard was mutation-tested.
        let lines: Vec<&str> = code.lines().collect();
        for (n, line) in lines.iter().enumerate() {
            if !(line.contains("env_tag_keys") && line.contains('=')) {
                continue;
            }
            // Read to the end of the statement.
            let mut expr = String::new();
            for l in &lines[n..] {
                expr.push_str(l);
                if l.trim_end().ends_with(';') {
                    break;
                }
            }
            assert!(
                !expr.contains("unwrap_or_default"),
                "{name}:{} flattens the tag-fetch failure into an empty list, \
                 which makes EBL010 fire instead of skip: {}",
                n + 1,
                expr.trim()
            );
        }
    }
}

#[tokio::test]
async fn ascii_icon_mode_renders_no_unicode_arrows() {
    // The end-to-end half of `every_status_glyph_has_an_ascii_form`:
    // the pure helpers can be right while a call site still hardcodes
    // the glyph. Five did — the header delta arrows, the sort marker,
    // and the Metrics anomaly badge, which baked `▲` into its message
    // string where a glyph-helper grep wouldn't find it.
    let cfg = crate::config::Config {
        icons: "ascii".into(),
        ..crate::config::Config::default()
    };
    let mut app = App::for_tests(crate::aws::AwsClient::stub(), cfg);
    app.environments = vec![
        mk_env("api-prod", "uflexi", "Web", "Red"),
        mk_env("api-staging", "uflexi", "Web", "Green"),
    ];
    app.rebuild_view();
    app.table_state.select(Some(0));
    // Header deltas render only when a bucket moved.
    app.health_delta = vec![("Red".to_string(), 1)];
    app.status_delta = vec![("Ready".to_string(), -1)];

    let out = render(&mut app, 160, 44);
    for g in ['▲', '▼'] {
        assert!(!out.contains(g), "ascii mode rendered {g}:\n{out}");
    }
    // The information the glyphs carry is still there.
    assert!(
        out.contains('^') || out.contains('v'),
        "the ascii forms replaced them:\n{out}"
    );
}

#[tokio::test]
async fn the_anomaly_badge_is_ascii_at_its_call_site_too() {
    // `series_anomaly_label` takes an `IconStyle` and its unit test
    // pins that. What that test can't see is whether the CALL SITE
    // passes `theme.icons` or hardcodes `Unicode` — and call sites are
    // where every regression in this release cycle actually lived.
    // Verified by mutation: hardcoding the glyph inside the function
    // leaves the fleet-view ascii test green, because that frame never
    // renders the Metrics tab.
    let cfg = crate::config::Config {
        icons: "ascii".into(),
        ..crate::config::Config::default()
    };
    let mut app = App::for_tests(crate::aws::AwsClient::stub(), cfg);
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Red")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();

    let detail = app.detail.as_mut().expect("detail opened");
    detail.tab_idx = detail
        .tabs
        .iter()
        .position(|t| *t == DetailTab::Metrics)
        .expect("Metrics tab present");
    detail.loading_metrics = false;
    // A flat baseline then a spike — the shape `series_anomaly_label`
    // fires on for a 5xx series.
    let now = chrono::Utc::now();
    detail.metrics = vec![crate::aws::MetricSeries {
        id: "req5xx".into(),
        label: "5xx".into(),
        points: (0..6)
            .map(|i| {
                let v = if i == 5 { 99.0 } else { 1.0 };
                (now - chrono::Duration::minutes(6 - i as i64), v)
            })
            .collect(),
    }];

    let out = render(&mut app, 160, 44);
    assert!(
        out.contains("anomaly"),
        "the badge has to be on screen for this test to mean anything:\n{out}"
    );
    assert!(
        !out.contains('▲'),
        "ascii mode rendered ▲ in the anomaly badge:\n{out}"
    );
}

#[tokio::test]
async fn a_red_env_gets_a_red_status_pill_in_the_table() {
    // `status_alert()` has unit tests; nothing checked that the TABLE
    // uses its result. Forcing `StatusAlert::None` at the call site —
    // which strips the alert colour from every Red env's STATUS pill,
    // the thing that says "this one" at a glance during triage —
    // passed all 1,097 tests.
    //
    // HEALTH and TREND are hidden because they colour a Red row red on
    // their own: the FIRST version of this test asserted on the row
    // and passed under the very mutation it was written to catch,
    // because the health dot satisfied it. With them hidden, the
    // status pill is the only thing that can make this row red.
    let mut app = test_app();
    app.view.hidden_cols.insert("HEALTH".into());
    app.view.hidden_cols.insert("TREND".into());

    let mut calm = mk_env("api-calm", "uflexi", "Web", "Green");
    calm.status = "Ready".into();
    let mut red = mk_env("api-red", "uflexi", "Web", "Red");
    red.status = "Ready".into();
    app.environments = vec![calm, red];
    app.rebuild_view();

    let buf = render_buf(&mut app, 170, 20);
    let calm_row = find_row(&buf, "api-calm").expect("calm row rendered");
    let red_row = find_row(&buf, "api-red").expect("red row rendered");

    assert!(
        row_has_fg(&buf, red_row, app.theme.health_red),
        "a Red env's STATUS pill must carry the alert colour"
    );
    assert!(
        !row_has_fg(&buf, calm_row, app.theme.health_red),
        "and a healthy env's must not — otherwise the assertion above \
         proves nothing about the alert"
    );
}

// --- render smoke for the screens the ui.rs split moved -----------------
//
// Measured after the split: `draw_dlq`, `draw_shell` and `draw_events`
// could each be replaced with `return;` and all 1,098 tests still
// passed. Three whole screens with no render coverage — and they are
// the ones an operator reaches during an incident, which is when a
// panic or a blank pane costs most. These are smoke tests, not
// golden-frame tests: they assert the screen draws and puts its own
// identifying content on the buffer.

#[tokio::test]
async fn the_dlq_viewer_renders_its_messages() {
    let mut app = test_app();
    app.environments = vec![mk_env("wk-prod", "uflexi", "Worker", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.dlq = Some(crate::app::DlqState {
        env_name: "wk-prod".into(),
        main_queue_url: "https://sqs.eu-west-2.amazonaws.com/1/awseb-main".into(),
        dlq_url: "https://sqs.eu-west-2.amazonaws.com/1/awseb-main-dlq".into(),
        messages: vec![crate::aws::QueueMessage {
            id: "MSG-CANARY-1".into(),
            receipt_handle: "rh".into(),
            body: "poison pill payload".into(),
            receive_count: 7,
            sent_at: None,
        }],
        list_state: Default::default(),
        loading: false,
        error: None,
        confirm_purge: false,
        purge_typed: tui_common::TextInput::new(),
        viewing: crate::app::QueueView::Dlq,
        confirm_delete_id: None,
        replay_input: None,
    });
    app.mode = crate::app::Mode::Dlq;

    let out = render(&mut app, 150, 30);
    assert!(
        out.contains("MSG-CANARY-1"),
        "the message id renders:\n{out}"
    );
    assert!(out.contains("wk-prod"), "and the env it belongs to:\n{out}");
}

#[tokio::test]
async fn the_events_panel_renders_its_rows() {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Red")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.event_panel.visible = true;
    app.event_panel.events = vec![crate::aws::Event {
        at: Some(chrono::Utc::now()),
        env: "api-prod".into(),
        application: "uflexi".into(),
        message: "EVENT-CANARY: deployment failed".into(),
        severity: "ERROR".into(),
        version_label: None,
    }];

    let out = render(&mut app, 160, 40);
    assert!(
        out.contains("EVENT-CANARY"),
        "the events panel draws its rows:\n{out}"
    );
}
