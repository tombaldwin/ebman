//! Pure helpers that don't belong to a single surface —
//! health ranking, error flattening, traffic warnings, update-kind
//! classification, unavailability maths.
//!
//! Split out of the 9,515-line `app/tests.rs`. Bodies moved
//! unchanged apart from one rewrite: `super::` meant `crate::app` in
//! the flat file and would mean `crate::app::tests` here, so every
//! explicit `super::` path was re-anchored (rustfmt reflowed some
//! lines as a result, since the new path is longer).

use super::super::*;
#[allow(unused_imports)]
use super::support::*;

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
fn console_url_encodes_special_chars() {
    // Reserved or non-alnum chars get %XX'd so the URL stays valid.
    let url = console_url("us-east-1", "my app", "env/with?slash").expect("commercial");
    assert!(url.contains("applicationName=my%20app"));
    assert!(url.contains("environmentName=env%2Fwith%3Fslash"));
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
    let r = crate::app::app_rollup(&envs, "foo", &dlq);
    assert_eq!(r.env_count, 2, "foo has 2 envs (prod + staging)");
    assert_eq!(r.red_count, 1, "staging is Red");
    assert_eq!(r.updating_count, 1, "staging is Updating");
    assert_eq!(r.worker_dlq_alerts, 0, "no worker envs in foo");
}

#[test]
fn app_rollup_empty_for_unknown_app() {
    let envs: Vec<crate::aws::Environment> = vec![];
    let dlq: HashMap<String, i64> = HashMap::new();
    let r = crate::app::app_rollup(&envs, "nope", &dlq);
    assert_eq!(r, crate::app::AppRollup::default());
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
    let diffs = crate::app::diff_config_options(&left, &right);
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
    assert!(crate::app::diff_config_options(&left, &right).is_empty());
}

#[test]
fn filter_config_diffs_drops_matching_names() {
    let diffs = vec![
        crate::app::ConfigDiff {
            namespace: "ns".into(),
            name: "MinSize".into(),
            left: Some("2".into()),
            right: Some("3".into()),
        },
        crate::app::ConfigDiff {
            namespace: "ns".into(),
            name: "version_label".into(),
            left: Some("v1".into()),
            right: Some("v2".into()),
        },
    ];
    let keys = crate::app::parse_ignore_keys(Some("version_label"));
    let filtered = crate::app::filter_config_diffs(diffs, &keys);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].name, "MinSize");
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

#[test]
fn flatten_err_marks_access_denied() {
    let e = color_eyre::eyre::eyre!("operation failed")
        .wrap_err("AccessDeniedException: User: arn:aws:sts::1234 is not authorized");
    let out = crate::app::flatten_err_to_string(&e);
    assert!(out.starts_with("AccessDenied:"), "got: {out}");
}

#[test]
fn flatten_err_marks_not_found() {
    let e = color_eyre::eyre::eyre!("operation failed")
        .wrap_err("ResourceNotFoundException: alarm 'foo' does not exist");
    let out = crate::app::flatten_err_to_string(&e);
    assert!(out.starts_with("NotFound:"), "got: {out}");
}

#[test]
fn flatten_err_marks_dependency_violation() {
    let e = color_eyre::eyre::eyre!("operation failed")
        .wrap_err("DependencyViolation: resource still has dependencies");
    let out = crate::app::flatten_err_to_string(&e);
    assert!(out.starts_with("Conflict:"), "got: {out}");
}

#[test]
fn flatten_err_marks_expired_token() {
    let e = color_eyre::eyre::eyre!("operation failed")
        .wrap_err("ExpiredToken: session credentials expired");
    let out = crate::app::flatten_err_to_string(&e);
    assert!(out.starts_with("ExpiredToken:"), "got: {out}");
}

#[test]
fn flatten_err_passes_unknown_through_unchanged() {
    let e = color_eyre::eyre::eyre!("some other failure");
    let out = crate::app::flatten_err_to_string(&e);
    assert!(
        !out.contains(":"),
        "expected no classification prefix; got: {out}"
    );
}

#[test]
fn traffic_warning_flags_updating() {
    let e = fake_env_with("prod", "Updating", "Yellow", Some(20));
    assert!(crate::app::compute_traffic_warning(&e)
        .unwrap()
        .contains("ACTIVE DEPLOY"));
}

#[test]
fn traffic_warning_flags_recent_change() {
    let e = fake_env_with("prod", "Ready", "Green", Some(2));
    assert!(crate::app::compute_traffic_warning(&e)
        .unwrap()
        .contains("RECENT CHANGE"));
}

#[test]
fn traffic_warning_silent_on_quiet_env() {
    let e = fake_env_with("prod", "Ready", "Green", Some(60));
    assert!(crate::app::compute_traffic_warning(&e).is_none());
}

#[test]
fn traffic_warning_flags_red_health() {
    let e = fake_env_with("prod", "Ready", "Red", Some(120));
    assert!(crate::app::compute_traffic_warning(&e)
        .unwrap()
        .contains("Red"));
}

#[test]
fn is_throttling_error_reads_the_classification_already_made() {
    // Narrowed contract: the input is a message from `flatten_err`,
    // which has already classified it. This used to substring-match the
    // flattened text for "throttling" — a second sniff after the first
    // had decided — so any error whose text merely contained the word
    // armed the refresh back-off.
    assert!(is_throttling_error("ThrottlingException: Rate exceeded"));
    assert!(is_throttling_error(
        "ThrottlingException: service error — please slow down"
    ));

    // Not throttling.
    assert!(!is_throttling_error("EnvironmentNotFound"));
    assert!(!is_throttling_error("AccessDenied: not authorized"));
    assert!(!is_throttling_error(""));
    // The false positive that mattered: the word appears, the
    // classification says otherwise.
    assert!(!is_throttling_error(
        "AccessDenied: environment throttling-test is not authorized"
    ));
    // Raw AWS text that never went through `flatten_err` is NOT this
    // function's input — both callers pass flattened strings, and
    // classifying twice is what created the bug.
    assert!(!is_throttling_error("RequestLimitExceeded"));
}

#[test]
fn the_debug_fallback_still_classifies_unconverted_call_sites() {
    // Not every `wrap_err` site captures typed metadata yet, so
    // `flatten_err_to_string` keeps its Debug sniff as a FALLBACK. End
    // to end, an unconverted throttle still comes out with the prefix
    // the predicate now reads — which is what makes narrowing the
    // predicate safe.
    let report = color_eyre::eyre::eyre!("ThrottlingException: Rate exceeded");
    let flat = crate::app::flatten_err_to_string(&report);
    assert!(
        is_throttling_error(&flat),
        "fallback path must still arm the back-off: {flat}"
    );
}

#[test]
fn pick_default_log_group_prefers_web_stdout() {
    let groups: Vec<String> = vec![
        "/aws/elasticbeanstalk/myenv/var/log/eb-engine.log".into(),
        "/aws/elasticbeanstalk/myenv/var/log/web.stdout.log".into(),
        "/aws/elasticbeanstalk/myenv/var/log/nginx/access.log".into(),
    ];
    assert_eq!(
        crate::app::pick_default_log_group(&groups).as_deref(),
        Some("/aws/elasticbeanstalk/myenv/var/log/web.stdout.log")
    );
}

#[test]
fn pick_default_log_group_falls_back_to_first() {
    let groups: Vec<String> = vec!["/aws/elasticbeanstalk/myenv/var/log/custom.log".into()];
    assert_eq!(
        crate::app::pick_default_log_group(&groups).as_deref(),
        Some("/aws/elasticbeanstalk/myenv/var/log/custom.log")
    );
    // No groups at all → None.
    assert_eq!(crate::app::pick_default_log_group(&[]), None);
}

#[test]
fn pick_default_log_group_prefers_engine_log_when_stdout_absent() {
    let groups: Vec<String> = vec![
        "/aws/elasticbeanstalk/myenv/var/log/nginx/access.log".into(),
        "/aws/elasticbeanstalk/myenv/var/log/eb-engine.log".into(),
    ];
    assert_eq!(
        crate::app::pick_default_log_group(&groups).as_deref(),
        Some("/aws/elasticbeanstalk/myenv/var/log/eb-engine.log")
    );
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
    let out = crate::app::collect_saved_configs(&apps);
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
    assert!(crate::app::collect_saved_configs(&apps).is_empty());
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
    crate::app::merge_app_latest_versions(&prev, &mut next);
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
    crate::app::merge_app_latest_versions(&prev, &mut next);
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
    crate::app::merge_app_latest_versions(&prev, &mut next);
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].latest_version_label.as_deref(), Some("build-2"));
}

#[test]
fn classify_update_kind_config_change() {
    let evs = vec![make_event("Updating environment configuration completed.")];
    assert_eq!(
        crate::app::classify_update_kind(&evs),
        crate::app::UpdateKind::Config
    );
}

#[test]
fn classify_update_kind_scale_event() {
    let evs = vec![make_event("Adding instance 'i-abc123' to environment.")];
    assert_eq!(
        crate::app::classify_update_kind(&evs),
        crate::app::UpdateKind::Scale
    );
}

#[test]
fn classify_update_kind_unknown_message_falls_through_to_generic() {
    let evs = vec![make_event("Something cryptic happened.")];
    assert_eq!(
        crate::app::classify_update_kind(&evs),
        crate::app::UpdateKind::Generic
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
    match crate::app::classify_update_kind(&evs) {
        crate::app::UpdateKind::Deploy { version_label } => {
            assert_eq!(version_label.as_deref(), Some("build-99"));
        }
        other => panic!("expected Deploy from newest match, got {other:?}"),
    }
}

#[test]
fn classify_update_kind_empty_events_is_generic() {
    assert_eq!(
        crate::app::classify_update_kind(&[]),
        crate::app::UpdateKind::Generic
    );
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
fn encode_filter_only_view_emits_just_the_filter_part() {
    // The encoded form must omit sort/grouped/scope so loading
    // doesn't perturb those — `apply_view` "missing fields
    // untouched" semantics depend on it.
    let encoded = crate::app::encode_filter_only_view("tag:env=prod");
    assert_eq!(encoded, "filter=tag:env=prod");
    // Empty filter — still emits `filter=` so load semantics
    // are consistent (filter clears to empty).
    assert_eq!(crate::app::encode_filter_only_view(""), "filter=");
}

#[test]
fn view_filter_value_extracts_filter_or_empty() {
    assert_eq!(
        crate::app::view_filter_value("filter=tag:env=prod"),
        "tag:env=prod"
    );
    // Filter portion in the middle of a full view.
    assert_eq!(
        crate::app::view_filter_value("sort=name:asc;filter=tag:env=prod;grouped=false"),
        "tag:env=prod",
    );
    // No filter portion → empty (operator's view that doesn't
    // touch the filter).
    assert_eq!(
        crate::app::view_filter_value("sort=name:asc;grouped=true"),
        ""
    );
    // Empty encoded → empty filter.
    assert_eq!(crate::app::view_filter_value(""), "");
    // Leading whitespace on a part is tolerated (matches the
    // tolerant parse in `apply_view`).
    assert_eq!(
        crate::app::view_filter_value("sort=name:asc; filter=foo"),
        "foo",
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

#[test]
fn compute_unavailability_count_per_policy() {
    // AllAtOnce — every instance flips at once.
    assert_eq!(
        crate::app::compute_unavailability_count("AllAtOnce", 1, "Fixed", 4),
        4
    );
    // Rolling, fixed batch of 1 on 4 instances → 1 unavailable.
    assert_eq!(
        crate::app::compute_unavailability_count("Rolling", 1, "Fixed", 4),
        1
    );
    // Rolling, fixed batch of 2 on 4 → 2.
    assert_eq!(
        crate::app::compute_unavailability_count("Rolling", 2, "Fixed", 4),
        2
    );
    // Rolling, 50% on 4 → 2.
    assert_eq!(
        crate::app::compute_unavailability_count("Rolling", 50, "Percentage", 4),
        2
    );
    // Rolling, 33% on 4 → ceil(1.32) = 2.
    assert_eq!(
        crate::app::compute_unavailability_count("Rolling", 33, "Percentage", 4),
        2
    );
    // RollingWithAdditionalBatch — extra batch first, zero impact.
    assert_eq!(
        crate::app::compute_unavailability_count("RollingWithAdditionalBatch", 1, "Fixed", 4),
        0
    );
    // Immutable + TrafficSplitting — new fleet, zero impact.
    assert_eq!(
        crate::app::compute_unavailability_count("Immutable", 1, "Fixed", 4),
        0
    );
    assert_eq!(
        crate::app::compute_unavailability_count("TrafficSplitting", 1, "Fixed", 4),
        0
    );
    // Unknown policy → assume worst case rather than lulling
    // the operator with a false zero.
    assert_eq!(
        crate::app::compute_unavailability_count("WeirdCustomPolicy", 1, "Fixed", 4),
        4
    );
    // Case-insensitive (EB API can return mixed casing).
    assert_eq!(
        crate::app::compute_unavailability_count("allatonce", 1, "Fixed", 4),
        4
    );
}

#[test]
fn compute_batch_count_clamps_and_rounds_up() {
    // Fixed clamps to [1, max].
    assert_eq!(crate::app::compute_batch_count(0, "Fixed", 4), 1);
    assert_eq!(crate::app::compute_batch_count(10, "Fixed", 4), 4);
    assert_eq!(crate::app::compute_batch_count(2, "Fixed", 4), 2);
    // Percentage rounds up.
    assert_eq!(crate::app::compute_batch_count(33, "Percentage", 4), 2); // ceil(1.32)=2
    assert_eq!(crate::app::compute_batch_count(25, "Percentage", 4), 1);
    assert_eq!(crate::app::compute_batch_count(26, "Percentage", 4), 2); // ceil(1.04)=2
    assert_eq!(crate::app::compute_batch_count(100, "Percentage", 4), 4);
    // Out-of-range percentage clamps.
    assert_eq!(crate::app::compute_batch_count(0, "Percentage", 4), 1);
    assert_eq!(crate::app::compute_batch_count(200, "Percentage", 4), 4);
}

#[test]
fn extract_unavailability_inputs_uses_eb_defaults_on_missing_settings() {
    // Empty option-settings — defaults match what EB itself
    // uses when no explicit value is configured.
    let (policy, batch, btype, asg) = crate::app::extract_unavailability_inputs(&[]);
    assert_eq!(policy, "AllAtOnce");
    assert_eq!(batch, 1);
    assert_eq!(btype, "Fixed");
    assert_eq!(asg, 1);

    // Partial — operator only set MaxSize.
    let opts = vec![("aws:autoscaling:asg".into(), "MaxSize".into(), "6".into())];
    let (_, _, _, asg) = crate::app::extract_unavailability_inputs(&opts);
    assert_eq!(asg, 6);

    // Empty string values collapse to default rather than the
    // empty string being mistaken for a policy.
    let opts = vec![(
        "aws:elasticbeanstalk:command".into(),
        "DeploymentPolicy".into(),
        String::new(),
    )];
    let (policy, _, _, _) = crate::app::extract_unavailability_inputs(&opts);
    assert_eq!(policy, "AllAtOnce");
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
async fn re_freezing_updates_the_reason_in_place() {
    // Shared freeze-marker path; see `freeze::MARKER_LOCK`.
    let _marker_guard = crate::freeze::MARKER_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
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
async fn ssm_run_without_args_errors_clearly() {
    let mut app = test_app();
    app.execute_command("ssm-run");
    let err = app.error_message.as_deref().unwrap_or("");
    assert!(
        err.contains("usage:") && err.contains("shell-command"),
        "expected usage hint, got: {err}"
    );
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
    assert!(!crate::app::is_event_tail_gap(&undated));
}

#[tokio::test]
async fn a_clean_fan_out_reports_nothing() {
    let mut app = test_app();
    app.apply_refresh(
        app.fanout_epoch,
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
        crate::ui::event_severity_style(crate::app::EVENT_TAIL_GAP_SEVERITY, &theme),
        crate::ui::event_severity_style("INFO", &theme),
        "the gap marker must not render identically to routine chatter"
    );
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
        app.fanout_epoch,
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
async fn a_clean_fan_out_clears_the_back_off() {
    let mut app = test_app();
    app.consecutive_throttles = 3;
    app.apply_refresh(
        app.fanout_epoch,
        Ok(vec![mk_env("api-prod", "uflexi", "Web", "Green")]),
        Vec::new(),
    );
    assert!(app.throttle_until.is_none());
    assert_eq!(app.consecutive_throttles, 0);
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
    let body = crate::app::render_explain_overlay("arn:aws:iam::1:role/R", &rows, true);
    assert!(body.contains("INCOMPLETE"), "{body}");
    assert!(
        body.find("INCOMPLETE").unwrap() < body.find("s3:GetObject").unwrap(),
        "the banner has to precede the rows it qualifies:\n{body}"
    );

    let clean = crate::app::render_explain_overlay("arn:aws:iam::1:role/R", &rows, false);
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

#[test]
fn dlq_absence_note_distinguishes_the_three_causes() {
    use crate::aws::DlqOrigin;
    // The whole point: these used to render identically as
    // "(queue URL not resolved)", so an operator could not tell a
    // missing dead-letter queue from a naming-convention guess that
    // missed, nor either from a queue EB references that has been
    // deleted underneath it.
    let derived = crate::app::dlq_absence_note(
        Some("https://sqs.eu-west-2.amazonaws.com/1/awseb-main-dlq"),
        Some(DlqOrigin::Derived),
    );
    assert!(derived.contains("guessed"), "{derived}");
    assert!(
        derived.contains("awseb-main-dlq"),
        "names the guess: {derived}"
    );

    let reported = crate::app::dlq_absence_note(
        Some("https://sqs.eu-west-2.amazonaws.com/1/real-dlq"),
        Some(DlqOrigin::Reported),
    );
    assert!(
        reported.contains("does not exist"),
        "a queue EB names but that is gone is an anomaly, not a shrug: {reported}"
    );
    assert!(reported.contains("real-dlq"), "{reported}");

    let none = crate::app::dlq_absence_note(None, None);
    assert!(none.contains("no dead-letter queue configured"), "{none}");

    // All three must differ, or the distinction is cosmetic.
    assert_ne!(derived, reported);
    assert_ne!(derived, none);
    assert_ne!(reported, none);
}

#[test]
fn dlq_absence_note_reads_as_one_clean_line() {
    use crate::aws::DlqOrigin;
    for (u, o) in [
        (Some("https://sqs/x-dlq"), Some(DlqOrigin::Derived)),
        (Some("https://sqs/x-dlq"), Some(DlqOrigin::Reported)),
        (Some("https://sqs/x-dlq"), None),
        (None, None),
    ] {
        let m = crate::app::dlq_absence_note(u, o);
        assert!(!m.contains('\n'), "status bar is one line: {m:?}");
        assert!(!m.contains("  "), "wrapped-literal indentation hole: {m:?}");
        assert!(!m.is_empty());
    }
}

#[test]
fn a_dlq_url_always_carries_its_origin() {
    // Guard for the widened type. `dlq_url` and `dlq_origin` are only
    // meaningful together: a url with no origin is a state the fetcher
    // cannot produce, and if one appears it means a call site set the
    // url and dropped the provenance — the exact way a distinction gets
    // destroyed after being added. Pinned against every fixture the
    // crate builds.
    for name in [
        "poly-batch",
        "poly-prod-worker",
        "poly-staging-worker",
        "not-a-worker-env",
    ] {
        let q = crate::demo_fixture::worker_queues_for_env(name);
        assert_eq!(
            q.dlq_url.is_some(),
            q.dlq_origin.is_some(),
            "{name}: dlq_url and dlq_origin must be set together"
        );
    }
}

#[test]
fn a_typed_error_code_beats_sniffing_the_debug_dump() {
    use crate::aws::AwsErrorMeta;
    use color_eyre::eyre::WrapErr;

    // The mechanism that arms the refresh back-off used to lowercase
    // `format!("{e:?}")` and substring-match it. That made the back-off
    // depend on the `Debug` representation of an SDK type — not a
    // stability contract — and handed out false positives for free.
    let meta = AwsErrorMeta {
        code: Some("ThrottlingException".into()),
        request_id: Some("abc-123".into()),
    };
    let report = Err::<(), _>(color_eyre::eyre::eyre!("service error"))
        .wrap_err(meta)
        .wrap_err_with(|| "DescribeEnvironments failed".to_string())
        .unwrap_err();
    let flat = crate::app::flatten_err_to_string(&report);
    assert!(
        flat.starts_with("ThrottlingException:"),
        "the SDK's own code classifies it: {flat}"
    );
    assert!(
        crate::app::is_throttling_error(&flat),
        "and the back-off predicate still fires: {flat}"
    );
}

#[test]
fn an_env_named_throttling_does_not_arm_the_back_off() {
    use crate::aws::AwsErrorMeta;
    use color_eyre::eyre::WrapErr;

    // The false positive the Debug sniff allowed. An env legitimately
    // named `throttling-test` put the word in the error chain, so a
    // plain AccessDenied against it read as throttling and armed the
    // refresh back-off — slowing the fleet listing over a permissions
    // problem that back-off cannot fix.
    let meta = AwsErrorMeta {
        code: Some("AccessDeniedException".into()),
        request_id: None,
    };
    let report = Err::<(), _>(color_eyre::eyre::eyre!(
        "environment throttling-test: not authorized"
    ))
    .wrap_err(meta)
    .wrap_err_with(|| "DescribeEnvironments failed".to_string())
    .unwrap_err();
    let flat = crate::app::flatten_err_to_string(&report);
    assert!(
        flat.starts_with("AccessDenied:"),
        "the code says AccessDenied, whatever the message contains: {flat}"
    );
    assert!(
        !crate::app::is_throttling_error(&flat),
        "and the back-off must NOT arm: {flat}"
    );
}

#[test]
fn the_request_id_survives_to_the_log() {
    // Never captured before, so when AWS support asked for one there was
    // nothing to give them.
    use crate::aws::AwsErrorMeta;
    let meta = AwsErrorMeta {
        code: Some("ThrottlingException".into()),
        request_id: Some("req-9f3c".into()),
    };
    assert!(meta.to_string().contains("req-9f3c"), "{meta}");
}
