//! The refresh cycle and everything spawned off it: generation
//! guards, staleness, tails, watchdogs, rollouts, deploys, the undo
//! window and the pending queue.
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
    let body = crate::app::render_env_resources_tree(&res, "worker-prod", "Worker");
    assert!(body.contains("├─ WorkerQueue"));
    assert!(body.contains("https://sqs.eu-west-2.amazonaws.com/123/main"));
    assert!(body.contains("└─ WorkerDeadLetterQueue"));
    assert!(body.contains("https://sqs.eu-west-2.amazonaws.com/123/dlq"));
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
    let out = crate::app::format_app_versions(&versions, Some("build-5"), 20, false);
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
    let out = crate::app::format_app_versions(&versions, Some("build-2"), 20, false);
    assert!(out.contains("◀ deployed"));
    // No truncation banner when total <= limit.
    assert!(!out.contains("showing "));
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
    let body =
        crate::app::format_deploy_preview("uflexi-prod", "build-141", "build-142", &versions);
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
    let body =
        crate::app::format_deploy_preview("uflexi-prod", "build-new", "build-old", &versions);
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
    let body = crate::app::format_deploy_preview(
        "uflexi-prod",
        "build-141",
        "build-DOES-NOT-EXIST",
        &versions,
    );
    assert!(body.contains("not found"));
    assert!(body.contains("build-DOES-NOT-EXIST"));
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
        crate::app::previous_version_label(&events, "build-3"),
        Some("build-2".into())
    );
    // Only the current version (+ untagged) appears → None.
    let only_current = vec![ev(Some("build-3")), ev(None), ev(Some("build-3"))];
    assert_eq!(
        crate::app::previous_version_label(&only_current, "build-3"),
        None
    );
    // No version labels at all → None.
    assert_eq!(
        crate::app::previous_version_label(&[ev(None), ev(None)], "build-3"),
        None
    );
    // Empty event list → None.
    assert_eq!(crate::app::previous_version_label(&[], "build-3"), None);
    // Empty-string labels are skipped.
    assert_eq!(
        crate::app::previous_version_label(&[ev(Some("")), ev(Some("build-1"))], "build-3"),
        Some("build-1".into())
    );
}

#[test]
fn is_config_event_keeps_deploys_and_config_changes() {
    assert!(crate::app::is_config_event(
        "Updating environment uflexi-prod to use version label 'build-9'."
    ));
    assert!(crate::app::is_config_event(
        "Deploying new version to instance(s)."
    ));
    assert!(crate::app::is_config_event(
        "Updating environment uflexi-prod's configuration settings."
    ));
    // Routine health / lifecycle noise is filtered out.
    assert!(!crate::app::is_config_event(
        "Environment health transitioned from Ok to Severe."
    ));
    assert!(!crate::app::is_config_event(
        "Added instance 'i-abc' to environment."
    ));
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
    assert!(crate::app::format_lineage("prod", &[]).contains("No deploys"));
    // Two deploys: build-9 12:00→12:05 (took 5m), build-8 at 10:00
    // (gap of 2h since previous from build-9's POV).
    let evs = vec![
        mk(ts(12, 5), "build-9"),
        mk(ts(12, 0), "build-9"),
        mk(ts(10, 0), "build-8"),
    ];
    let body = crate::app::format_lineage("prod", &evs);
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
    match crate::app::classify_update_kind(&evs) {
        crate::app::UpdateKind::Deploy { version_label } => {
            assert_eq!(version_label.as_deref(), Some("build-142"));
        }
        other => panic!("expected Deploy, got {other:?}"),
    }
}

#[test]
fn classify_update_kind_deploy_without_label_still_classifies() {
    let evs = vec![make_event("Deploying new version to instance i-abc123.")];
    match crate::app::classify_update_kind(&evs) {
        crate::app::UpdateKind::Deploy { version_label } => {
            // Label can't be extracted from this message shape — that's
            // fine, it's still a Deploy.
            assert!(version_label.is_none());
        }
        other => panic!("expected Deploy, got {other:?}"),
    }
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
    app.apply_refresh(app.fanout_epoch, Ok(vec![env]), Vec::new());
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
    app.apply_refresh(app.fanout_epoch, Ok(vec![env]), Vec::new());
    assert!(
        app.armed_watchdogs.contains_key("prod"),
        "Updating+Green is mid-deploy — watchdog must remain armed"
    );
}

#[test]
fn deploy_settled_green_requires_both_status_ready_and_health_green_or_ok() {
    assert!(crate::app::deploy_settled_green("Ready", "Green"));
    assert!(crate::app::deploy_settled_green("Ready", "Ok"));
    assert!(crate::app::deploy_settled_green("ready", "green")); // case-insensitive
    assert!(crate::app::deploy_settled_green("READY", "OK"));
    // Status mismatch — false even if health is Green.
    assert!(!crate::app::deploy_settled_green("Updating", "Green"));
    assert!(!crate::app::deploy_settled_green("Launching", "Ok"));
    assert!(!crate::app::deploy_settled_green("Terminating", "Green"));
    // Health mismatch — false even if status is Ready.
    assert!(!crate::app::deploy_settled_green("Ready", "Red"));
    assert!(!crate::app::deploy_settled_green("Ready", "Yellow"));
    assert!(!crate::app::deploy_settled_green("Ready", "Severe"));
    // Both wrong.
    assert!(!crate::app::deploy_settled_green("", ""));
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
    let entry = crate::app::build_undo_entry("prod", "keypair foo", &to_set, &[], &pre);
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
    let entry =
        crate::app::build_undo_entry("prod", "health-check-url /healthz", &to_set, &[], &pre);
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
    let entry = crate::app::build_undo_entry("prod", "keypair foo", &to_set, &[], &pre);
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
    let entry = crate::app::build_undo_entry("prod", "clear keypair", &[], &to_remove, &pre);
    assert_eq!(entry.to_set.len(), 1);
    assert_eq!(entry.to_set[0].2, "bar");
    assert!(entry.to_remove.is_empty());
}

#[test]
fn build_undo_entry_remove_with_no_prior_value_is_a_noop_reverse() {
    // Original: remove a key that was already absent. Reverse:
    // nothing (both sides empty).
    let entry = crate::app::build_undo_entry(
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
async fn handle_undo_captured_pushes_into_history_with_cap() {
    // Pushing UNDO_HISTORY_CAP + 2 entries leaves CAP-many in
    // the deque, with the OLDEST entries evicted from the
    // front. Confirms the ring-buffer eviction logic.
    let mut app = test_app();
    for i in 0..(crate::app::UNDO_HISTORY_CAP + 2) {
        let entry = crate::app::UndoEntry {
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
    assert_eq!(app.undo_history.len(), crate::app::UNDO_HISTORY_CAP);
    // The two oldest (#0, #1) should have been evicted; #2
    // becomes the front-most surviving entry.
    assert_eq!(
        app.undo_history.front().unwrap().original_summary,
        "write #2"
    );
    // Back of the deque is the most-recent push.
    assert_eq!(
        app.undo_history.back().unwrap().original_summary,
        format!("write #{}", crate::app::UNDO_HISTORY_CAP + 1)
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
    app.apply_refresh(
        app.fanout_epoch,
        Ok(vec![mk_env("prod", "shop", "Web", "Green")]),
        Vec::new(),
    );
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
    app.apply_refresh(
        app.fanout_epoch,
        Ok(vec![mk_env("prod", "shop", "Web", "Red")]),
        Vec::new(),
    );
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
    let body = crate::app::format_armed_rollbacks(&armed, chrono::Utc::now());
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
    let body = crate::app::format_armed_rollbacks(&armed, now);
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
    let body = crate::app::format_armed_rollbacks(&armed, now);
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
    let (env, remaining) = crate::app::soonest_armed_rollback(&armed, now).expect("one armed");
    assert_eq!(env, "sooner");
    // Remaining is in humanize-short-age form — 60s renders as "1m".
    assert!(remaining.contains('m') || remaining.contains('s'));
}

#[test]
fn soonest_armed_rollback_returns_none_when_empty() {
    let armed = std::collections::HashMap::new();
    assert!(crate::app::soonest_armed_rollback(&armed, chrono::Utc::now()).is_none());
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
    app.apply_refresh(
        app.fanout_epoch,
        Ok(vec![mk_env("prod", "shop", "Web", "Green")]),
        Vec::new(),
    );
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
    app.apply_refresh(
        app.fanout_epoch,
        Ok(vec![mk_env("prod", "shop", "Web", "Red")]),
        Vec::new(),
    );
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
    app.apply_refresh(
        app.fanout_epoch,
        Ok(vec![mk_env("prod", "shop", "Web", "Green")]),
        Vec::new(),
    );
    assert!(
        app.armed_watchdogs.is_empty(),
        "Green refresh should disarm"
    );
    let status = app.status_message.as_deref().unwrap_or("");
    assert!(status.contains("watchdog disarmed"));
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
        app.fanout_epoch,
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
    app.apply_refresh(
        app.fanout_epoch,
        Ok(vec![mk_env("prod", "shop", "Web", "Red")]),
        Vec::new(),
    );
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
        severity: crate::app::EVENT_TAIL_GAP_SEVERITY.into(),
        version_label: None,
    };
    assert!(
        crate::app::event_tail_matches(&pattern, &marker),
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
    assert!(!crate::app::event_tail_matches(&pattern, &other));
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
        severity: crate::app::EVENT_TAIL_GAP_SEVERITY.into(),
        version_label: None,
    };
    // One truncated poll: the marker plus enough events to evict it.
    let mut batch = vec![marker];
    for i in 0..crate::app::EVENT_TAIL_MAX_EVENTS {
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

    let Some(crate::app::Overlay::EventTail {
        events,
        truncated_polls,
        ..
    }) = app.current_overlay.as_ref()
    else {
        panic!("event tail should be open");
    };
    assert!(
        !events.iter().any(crate::app::is_event_tail_gap),
        "the marker was evicted by its own batch — which is the point"
    );
    assert_eq!(
        *truncated_polls, 1,
        "the gap must still be reported once the marker is gone"
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
        app.fanout_epoch,
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
                && !is_test_source(&path)
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
