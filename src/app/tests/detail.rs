//! The Detail pane: tabs, instances, metrics, alarms.
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
fn render_env_resources_tree_marks_orphan_instances_when_no_asg() {
    let mut res = empty_resources();
    res.instances = vec!["i-stranded".into()];
    let body = crate::app::render_env_resources_tree(&res, "env", "Web");
    assert!(body.contains("orphan (no ASG attached)"));
    assert!(body.contains("i-stranded"));
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
    let expected = crate::app::completion_candidates("ba");
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(
        app.command_input.text(),
        expected[0],
        "first Tab should land on the first match, not skip it"
    );
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
    // Absolute, per kind. Comparing two aliases to each other passes
    // when BOTH resolve to None, which is exactly what deleting their
    // shared arm does — that is how the 4xx and latency arms came to be
    // deletable, and 4xx was never mentioned at all.
    for (kind, metric, op, stat) in [
        (
            "health",
            "EnvironmentHealth",
            "LessThanOrEqualToThreshold",
            "Maximum",
        ),
        (
            "4xx",
            "ApplicationRequests4xx",
            "GreaterThanThreshold",
            "Sum",
        ),
        (
            "req4xx",
            "ApplicationRequests4xx",
            "GreaterThanThreshold",
            "Sum",
        ),
        (
            "5xx",
            "ApplicationRequests5xx",
            "GreaterThanThreshold",
            "Sum",
        ),
        (
            "req5xx",
            "ApplicationRequests5xx",
            "GreaterThanThreshold",
            "Sum",
        ),
        (
            "latency",
            "ApplicationLatencyP90",
            "GreaterThanThreshold",
            "Average",
        ),
        (
            "p90",
            "ApplicationLatencyP90",
            "GreaterThanThreshold",
            "Average",
        ),
    ] {
        assert_eq!(
            alarm_kind_to_metric(kind),
            Some((metric, op, stat)),
            "alarm_kind_to_metric({kind:?})"
        );
    }
    // Unknown.
    assert!(alarm_kind_to_metric("cpu").is_none());
    assert!(alarm_kind_to_metric("").is_none());
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
    let stub = crate::app::format_alarm_history("high-cpu", &[]);
    assert!(stub.contains("No history items"));
    assert!(stub.contains("90 days"));
    // Real entries → each row carries timestamp, kind in brackets,
    // and the summary line. Order preserved (newest-first per the
    // SDK's default).
    let entries = vec![
        mk(ts(12, 5), "StateUpdate", "Alarm updated from OK to ALARM"),
        mk(ts(11, 0), "ConfigurationUpdate", "Threshold changed to 80"),
    ];
    let body = crate::app::format_alarm_history("high-cpu", &entries);
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
    let body = crate::app::format_alarm_history("high-cpu", &entries);
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

#[tokio::test]
async fn tab_cycles_scope_envs_to_apps_and_back() {
    let mut app = test_app();
    assert_eq!(app.scope, Scope::Envs);
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.scope, Scope::Apps);
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.scope, Scope::Envs);
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
async fn config_tab_renders_the_in_place_value_editor() {
    // `draw_detail_config` matched on `editing.map(|e| e.mode)` and then
    // unwrapped `editing` inside the arm. Nothing drew this row, so the
    // rewrite to a match-on-Option was unverified. Render it.
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "poly", "Web", "Green")];
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
    app.environments = vec![mk_env("api-prod", "poly", "Web", "Red")];
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
    detail.instances_request = Default::default();
    detail.events_request = Default::default();
    assert!(
        app.detail.as_ref().unwrap().tab_loading_since().is_none(),
        "nothing outstanding yet"
    );

    // First refresh fires and marks the tab loading.
    let instances = |app: &App| {
        app.detail
            .as_ref()
            .unwrap()
            .instances_request
            .ticket_in_flight()
    };
    app.detail_refresh_active_tab();
    let fired = instances(&app).expect("the refresh fired");

    // A tick landing while it's still running is a no-op.
    app.detail_refresh_active_tab();
    assert_eq!(
        instances(&app),
        Some(fired),
        "a second refresh must not have been fired"
    );

    // Once the result lands, the next tick proceeds.
    app.detail.as_mut().unwrap().instances_request = Default::default();
    app.detail_refresh_active_tab();
    let next = instances(&app).expect("the next refresh fired");
    assert_ne!(next, fired, "a finished fetch must not block the next one");
}

#[tokio::test]
async fn a_lost_detail_fetch_does_not_wedge_the_tab() {
    // The guard reads the `*_request` fetches, which a dropped result —
    // a generation-guarded arrival after a context switch, say — never
    // clears. Without an age cap the tab would refuse to refresh for
    // the rest of the session.
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "poly", "Web", "Red")];
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
    // Still outstanding, and older than the stuck threshold.
    let d = app.detail.as_mut().unwrap();
    let stale = d.instances_request.ticket_in_flight().expect("in flight");
    d.instances_request
        .backdate(std::time::Instant::now() - std::time::Duration::from_secs(121));
    app.detail_refresh_active_tab();
    assert_ne!(
        app.detail
            .as_ref()
            .unwrap()
            .instances_request
            .ticket_in_flight(),
        Some(stale),
        "an outstanding fetch this old is lost, not slow — retry it"
    );
}

#[tokio::test]
async fn every_detail_tab_reports_its_own_loading_state() {
    // `tab_loading_since` gates the refresh, so a tab whose flag it forgot
    // would silently lose its in-flight guard — and one that read the
    // WRONG flag would refuse to refresh whenever some unrelated tab
    // was busy.
    let mut app = test_app();
    app.environments = vec![mk_env("wk-prod", "poly", "Worker", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    let d = app.detail.as_mut().expect("detail opened");
    assert!(
        d.tabs.contains(&DetailTab::Queue),
        "worker envs get the Queue tab"
    );

    // `open_detail` fires eager fetches; start from a clean slate.
    d.events_request = Default::default();
    d.instances_request = Default::default();
    d.queues_request = Default::default();
    d.metrics_request = Default::default();
    d.cw_alarms = Default::default();
    d.recent_versions = Default::default();
    d.app_tab.metrics = Default::default();
    d.log_tail.request = Default::default();

    // Every flag off: no tab claims to be loading.
    for idx in 0..d.tabs.len() {
        d.tab_idx = idx;
        assert!(
            d.tab_loading_since().is_none(),
            "{:?} with no flags set",
            d.tab()
        );
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
            |d| {
                let _ = d.events_request.begin();
            },
            &[DetailTab::Health, DetailTab::Events],
        ),
        (
            "instances",
            |d| {
                let _ = d.instances_request.begin();
            },
            &[DetailTab::Instances],
        ),
        (
            "queues",
            |d| {
                let _ = d.queues_request.begin();
            },
            &[DetailTab::Health, DetailTab::Queue],
        ),
        (
            "metrics",
            |d| {
                let _ = d.metrics_request.begin();
            },
            &[DetailTab::Metrics],
        ),
        (
            "alarms",
            |d| {
                let _ = d.cw_alarms.begin();
            },
            &[DetailTab::Health],
        ),
        (
            "app metrics",
            |d| {
                let _ = d.app_tab.metrics.begin();
            },
            &[DetailTab::App],
        ),
        (
            "recent versions",
            |d| {
                let _ = d.recent_versions.begin();
            },
            &[DetailTab::Health],
        ),
        (
            "log tail",
            |d| {
                let _ = d.log_tail.request.begin();
            },
            &[DetailTab::Logs],
        ),
    ];
    // A tab no case owns is a tab whose flag nobody checked. Config
    // fires no refresh fetch, so it owns none.
    let d = app.detail.as_ref().unwrap();
    for tab in d.tabs.iter().filter(|t| **t != DetailTab::Config) {
        assert!(
            cases.iter().any(|(_, _, owners)| owners.contains(tab)),
            "no case covers the {tab:?} tab's loading state"
        );
    }
    for (label, set, owners) in cases {
        let d = app.detail.as_mut().unwrap();
        d.events_request = Default::default();
        d.instances_request = Default::default();
        d.queues_request = Default::default();
        d.metrics_request = Default::default();
        d.cw_alarms = Default::default();
        d.recent_versions = Default::default();
        d.app_tab.metrics = Default::default();
        d.log_tail.request = Default::default();
        set(d);
        for idx in 0..d.tabs.len() {
            d.tab_idx = idx;
            let tab = d.tab();
            assert_eq!(
                d.tab_loading_since().is_some(),
                owners.contains(&tab),
                "{label} loading, on the {tab:?} tab"
            );
        }
    }
}

// --- render smoke for the Detail tabs the coverage sweep found bare ---
//
// A sweep over all 41 `draw_*` entry points (stub each with an early
// return, run the suite, see whether anything notices) found 28 that no
// test would catch if they stopped drawing entirely. These four are the
// Detail tabs an operator reaches mid-incident, which is the same
// argument that got `draw_dlq` / `draw_shell` / `draw_events` covered.

fn detail_app_on_tab(tab: DetailTab) -> App {
    detail_app_on_tab_for_tier(tab, "Web")
}

/// The Queue tab only exists for Worker-tier environments — `open_detail`
/// builds the tab list from the tier — so the tier has to be a parameter.
fn detail_app_on_tab_for_tier(tab: DetailTab, tier: &str) -> App {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "poly", tier, "Red")];
    app.view.invalidate();
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    let detail = app.detail.as_mut().expect("detail opened");
    detail.tab_idx = detail
        .tabs
        .iter()
        .position(|t| *t == tab)
        .expect("tab present");
    app
}

#[tokio::test]
async fn detail_events_tab_renders_its_events() {
    let mut app = detail_app_on_tab(DetailTab::Events);
    let d = app.detail.as_mut().unwrap();
    d.events_request = Default::default();
    d.events = vec![make_event("EVENT-CANARY deployment failed")];

    let out = render(&mut app, 160, 44);
    assert!(out.contains("EVENT-CANARY"), "event text renders:\n{out}");
}

#[tokio::test]
async fn detail_instances_tab_renders_its_instances() {
    let mut app = detail_app_on_tab(DetailTab::Instances);
    let d = app.detail.as_mut().unwrap();
    d.instances_request = Default::default();
    d.instances = vec![crate::aws::Instance {
        id: "i-0canary99".into(),
        health: "Severe".into(),
        color: "Red".into(),
        causes: vec!["ELB health failing".into()],
        instance_type: "t3.medium".into(),
        availability_zone: "eu-west-2a".into(),
        launched_at: None,
        app_stats: None,
    }];

    let out = render(&mut app, 160, 44);
    assert!(out.contains("i-0canary99"), "instance id renders:\n{out}");
}

#[tokio::test]
async fn detail_queue_tab_renders_its_queues() {
    let mut app = detail_app_on_tab_for_tier(DetailTab::Queue, "Worker");
    let d = app.detail.as_mut().unwrap();
    d.queues_request = Default::default();
    d.queues = crate::aws::WorkerQueues {
        main_url: Some("https://sqs.eu-west-2.amazonaws.com/1/awseb-CANARY".into()),
        dlq_url: None,
        main_stats: None,
        dlq_stats: None,
        dlq_origin: None,
    };

    let out = render(&mut app, 160, 44);
    assert!(out.contains("awseb-CANARY"), "queue url renders:\n{out}");
}

#[tokio::test]
async fn detail_logs_tab_draws_even_with_nothing_tailing() {
    // The empty state is the one an operator hits first, and a panic
    // here would take the whole TUI down mid-incident.
    let mut app = detail_app_on_tab(DetailTab::Logs);
    let out = render(&mut app, 160, 44);
    // Assert on the pane's OWN title, not on "Logs" (the tab strip draws
    // that) and not on the env name (the Detail header draws that). The
    // first cut of this test asserted both and passed with
    // `draw_detail_logs` stubbed out entirely — it was measuring the
    // chrome around the tab, not the tab.
    assert!(
        out.contains("instance(s)"),
        "the Logs pane's own title counts instances and lines:\n{out}"
    );
}

/// Characterisation test, written BEFORE the `Fetch<T>` refactor and
/// kept afterwards, re-expressed through the new API. The assertions are
/// unchanged; only the spelling moved.
///
/// The BACKLOG called `Option<T>` + `loading_*: bool` "4 representable
/// states for 3 real ones". That is a misreading, and this test is the
/// evidence — which is why `Fetch<T>` is a struct holding both facts and
/// not the four-variant enum that entry implies. All four combinations
/// are reachable and each means something different: the settled value
/// says whether we hold data, the in-flight flag says whether a request
/// is running right now.
///
/// The state that makes them orthogonal is settled-and-in-flight: a
/// refresh running while the previous result is still on screen.
/// `spawn_detail_alarms` calls `begin()` without clearing the value,
/// deliberately, so a refresh does not blank the panel. A four-variant
/// enum would lose it and `tab_loading()` would stop reporting a refresh
/// as in flight whenever data was already present.
#[tokio::test]
async fn the_alarms_pair_encodes_two_orthogonal_facts_not_one_redundant_state() {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "poly", "WebServer", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    let d = app.detail.as_mut().expect("detail opened");
    d.tab_idx = d
        .tabs
        .iter()
        .position(|t| *t == DetailTab::Health)
        .expect("Health tab");
    // `open_detail` fires eager fetches; start from a clean slate.
    d.events_request = Default::default();
    d.queues_request = Default::default();
    d.recent_versions = Default::default();

    let alarm = || {
        vec![crate::aws::CwAlarm {
            name: "cpu-high".into(),
            state: "ALARM".into(),
            state_reason: String::new(),
            metric_name: "CPUUtilization".into(),
            namespace: "AWS/EC2".into(),
        }]
    };

    // Idle: nothing held, nothing running.
    d.cw_alarms = Default::default();
    assert!(
        d.tab_loading_since().is_none(),
        "idle must not claim to be loading"
    );
    assert!(!d.cw_alarms.is_first_load());
    assert!(d.cw_alarms.ready().is_none() && d.cw_alarms.error().is_none());

    // First load: running, nothing to show, so the spinner is on.
    let _ = d.cw_alarms.begin();
    assert!(d.tab_loading_since().is_some(), "first load is in flight");
    assert!(
        d.cw_alarms.is_first_load(),
        "nothing to show yet, so ui/detail.rs draws `fetching alarms…`"
    );

    // Settled with data.
    d.cw_alarms.settle_now(Ok(alarm()));
    assert!(
        d.tab_loading_since().is_none(),
        "settled must not claim to be loading"
    );
    assert!(!d.cw_alarms.is_first_load());
    assert_eq!(d.cw_alarms.ready().map(Vec::len), Some(1));

    // THE state a four-variant enum would lose: a refresh in flight with
    // the previous result still displayed.
    let _ = d.cw_alarms.begin();
    assert!(
        d.tab_loading_since().is_some(),
        "a refresh must still report in-flight even when data is present"
    );
    assert!(
        !d.cw_alarms.is_first_load(),
        "but it must NOT draw the spinner — the old data stays visible"
    );
    assert_eq!(
        d.cw_alarms.ready().map(Vec::len),
        Some(1),
        "and the previous result must survive `begin()`"
    );

    // Settled and failed. Distinct from idle, and `ready` must not lie.
    d.cw_alarms.settle_now(Err("DescribeAlarms denied".into()));
    assert!(d.tab_loading_since().is_none());
    assert!(d.cw_alarms.ready().is_none(), "a failure is not a value");
    assert!(d.cw_alarms.error().is_some_and(|e| e.contains("denied")));
}

/// Does a successful fetch erase an unrelated fetch's error?
///
/// `open_detail` fires events / instances / metrics / queues
/// concurrently, and all four handlers settle into the SAME
/// `DetailState::error` slot — `Some(msg)` on failure, and `None` on
/// success. So whichever AWS response lands last decides what the
/// operator sees, and a success can silently wipe a real failure.
///
/// Written to find out rather than to assert: if this passes, the
/// concern is unfounded and the test documents that.
#[tokio::test]
async fn a_successful_fetch_does_not_erase_another_fetchs_error() {
    let mut app = test_app();
    app.environments = vec![mk_env("wk-prod", "poly", "Worker", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();

    let gen = app.generation;
    // Events fetch fails — AccessDenied on DescribeEvents, say.
    let ticket = detail_ticket(&mut app, |d| &mut d.events_request);
    app.handle_msg(crate::app::AppMsg::DetailEvents {
        gen,
        ticket,
        result: Err("AccessDenied: DescribeEvents".to_string()),
    });
    let after_failure = app
        .detail
        .as_ref()
        .and_then(|d| d.error.as_ref().map(|e| e.message.clone()));
    assert_eq!(
        after_failure.as_deref(),
        Some("AccessDenied: DescribeEvents"),
        "the failure must be recorded"
    );

    // An UNRELATED fetch then succeeds.
    let ticket = detail_ticket(&mut app, |d| &mut d.instances_request);
    app.handle_msg(crate::app::AppMsg::DetailInstances {
        gen,
        ticket,
        result: Ok(Vec::new()),
    });

    let after_success = app
        .detail
        .as_ref()
        .and_then(|d| d.error.as_ref().map(|e| e.message.clone()));
    assert_eq!(
        after_success.as_deref(),
        Some("AccessDenied: DescribeEvents"),
        "an unrelated success must NOT erase it — the operator would see \
         a clean panel and never learn the events fetch was denied"
    );
}

// ── mutation-sweep triage, 2026-08-26 ────────────────────────────────
//
// `detail_scroll` holds five more cursor implementations (a sixth in
// `detail_cycle_tab`), all on `rem_euclid` — which is the correct idiom,
// and notably NOT what three other modules hand-roll. See the
// cursor-wrap backlog item.
//
// The `if n == 0` guards are the load-bearing part: `rem_euclid(0)`
// panics, so an empty list without its guard takes the TUI down on a
// keypress. Same class as the empty-Select panic.

fn detail_on(env: &str) -> App {
    let mut app = test_app();
    app.environments = vec![mk_env(env, "poly", "Web", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    app.mode = crate::app::Mode::Detail;
    app
}

fn focus_tab(app: &mut App, tab: DetailTab) {
    let detail = app.detail.as_mut().expect("detail open");
    detail.tab_idx = detail
        .tabs
        .iter()
        .position(|t| *t == tab)
        .unwrap_or_else(|| panic!("{tab:?} not present"));
}

/// An empty list must not take the TUI down.
///
/// `rem_euclid(0)` panics, and each of these tabs guards with
/// `if n == 0 { return }`. The sweep left all three guards survivable —
/// inverted, the empty case is exactly the one that reaches the
/// division.
#[tokio::test]
async fn scrolling_an_empty_detail_list_is_inert_not_fatal() {
    for tab in [DetailTab::Instances, DetailTab::Health, DetailTab::Config] {
        for delta in [1, -1] {
            let mut app = detail_on("api-prod");
            {
                let d = app.detail.as_mut().expect("detail open");
                d.instances.clear();
                d.tags.clear();
                d.env_vars = Vec::new();
                d.events = Vec::new();
            }
            focus_tab(&mut app, tab);
            // Must not panic, and must leave the cursor where it was.
            app.detail_scroll(delta);
            let d = app.detail.as_ref().expect("detail still open");
            let cursor = match tab {
                DetailTab::Instances => d.instances_cursor,
                DetailTab::Health => d.health_cursor,
                DetailTab::Config => d.config_cursor,
                _ => 0,
            };
            assert_eq!(
                cursor, 0,
                "{tab:?} with delta {delta}: cursor moved on an empty list"
            );
        }
    }
}

/// The Queue tab's cursor wraps between exactly two rows, and the Config
/// tab's *clamps* instead — a deliberate difference the comment calls
/// out, because wrapping a long editable list past the bottom is
/// disorienting.
#[tokio::test]
async fn the_queue_cursor_wraps_and_the_config_cursor_clamps() {
    // Queue: two rows, wraps in both directions.
    let mut app = detail_on("worker-prod");
    {
        let d = app.detail.as_mut().expect("detail open");
        if !d.tabs.contains(&DetailTab::Queue) {
            d.tabs.push(DetailTab::Queue);
        }
    }
    focus_tab(&mut app, DetailTab::Queue);
    app.detail_scroll(1);
    assert_eq!(app.detail.as_ref().unwrap().queue_cursor, 1);
    app.detail_scroll(1);
    assert_eq!(
        app.detail.as_ref().unwrap().queue_cursor,
        0,
        "the queue cursor wraps past the last row"
    );
    app.detail_scroll(-1);
    assert_eq!(
        app.detail.as_ref().unwrap().queue_cursor,
        1,
        "and wraps backwards off the first"
    );

    // Config: clamps at both ends rather than wrapping.
    let mut app = detail_on("api-prod");
    {
        let d = app.detail.as_mut().expect("detail open");
        d.tags = vec![
            ("Owner".into(), "platform".into()),
            ("Team".into(), "infra".into()),
        ];
        d.env_vars = Vec::new();
        d.config_cursor = 0;
    }
    focus_tab(&mut app, DetailTab::Config);
    let n = crate::app::config_editable_items(app.detail.as_ref().unwrap()).len();
    assert!(n >= 2, "the fixture needs at least two editable rows");

    app.detail_scroll(-1);
    assert_eq!(
        app.detail.as_ref().unwrap().config_cursor,
        0,
        "the config cursor clamps at the top rather than wrapping to the end"
    );
    for _ in 0..(n + 3) {
        app.detail_scroll(1);
    }
    assert_eq!(
        app.detail.as_ref().unwrap().config_cursor,
        n - 1,
        "and clamps at the bottom rather than wrapping to the top"
    );
}

/// Tab cycling wraps both ways — and the tab list is never empty, which
/// is what makes `detail_cycle_tab`'s un-guarded `rem_euclid` safe.
#[tokio::test]
async fn detail_tab_cycling_wraps_both_ways() {
    let mut app = detail_on("api-prod");
    let n = app.detail.as_ref().unwrap().tabs.len();
    assert!(
        n >= 4,
        "detail_cycle_tab calls rem_euclid(tabs.len()) with no zero guard; \
         it is safe only because the list is built non-empty and only \
         grows. Found {n} tabs."
    );

    app.detail.as_mut().unwrap().tab_idx = 0;
    app.detail_cycle_tab(-1);
    assert_eq!(
        app.detail.as_ref().unwrap().tab_idx,
        n - 1,
        "cycling back off the first tab wraps to the last"
    );
    app.detail_cycle_tab(1);
    assert_eq!(
        app.detail.as_ref().unwrap().tab_idx,
        0,
        "and forward off the last wraps to the first"
    );
    app.detail_cycle_tab(1);
    assert_eq!(app.detail.as_ref().unwrap().tab_idx, 1, "plain forward");
}

/// `n` / `N` in the detail event search step to the NEXT match, wrapping.
///
/// The order starts at `cur + 1`, not `cur` — which is exactly what
/// makes repeated `n` advance instead of sticking on the match already
/// under the cursor. Twelve survivors sat on that arithmetic and the
/// direction test beside it.
#[tokio::test]
async fn detail_search_steps_to_the_next_match_and_wraps() {
    let armed = |cursor: u16| {
        let mut app = detail_on("api-prod");
        {
            let d = app.detail.as_mut().expect("detail open");
            // Three matches, at rows 0, 2 and 4. Two is not enough:
            // from a match in the middle, forward-wrapping and
            // backward both land on the same row, so the direction
            // assertion below cannot distinguish them. (My first
            // version had two and failed on exactly that.)
            d.events = vec![
                make_event("alpha match"),
                make_event("beta"),
                make_event("gamma match"),
                make_event("delta"),
                make_event("epsilon match"),
            ];
            d.search_pattern = Some(regex::Regex::new("match").expect("valid regex"));
            d.events_scroll = cursor;
        }
        app
    };
    let scroll = |app: &App| app.detail.as_ref().unwrap().events_scroll;

    // From row 0 (itself a match) forward → the NEXT one, not itself.
    let mut app = armed(0);
    app.detail_search_jump(1);
    assert_eq!(
        scroll(&app),
        2,
        "forward search must step past the match under the cursor, or `n` \
         would never advance"
    );

    // Again → the third match.
    app.detail_search_jump(1);
    assert_eq!(scroll(&app), 4);
    // Again → wraps back round to row 0.
    app.detail_search_jump(1);
    assert_eq!(scroll(&app), 0, "forward search wraps past the end");

    // Backward from row 0 → the LAST match, wrapping the other way.
    let mut app = armed(0);
    app.detail_search_jump(-1);
    assert_eq!(
        scroll(&app),
        4,
        "backward search wraps past the start to the last match"
    );

    // Backward from row 3 → the match before it.
    let mut app = armed(3);
    app.detail_search_jump(-1);
    assert_eq!(scroll(&app), 2, "backward finds the preceding match");

    // Forward and backward from the same place must differ when there
    // is more than one match — otherwise `N` is just `n`.
    let mut fwd = armed(2);
    fwd.detail_search_jump(1);
    let mut back = armed(2);
    back.detail_search_jump(-1);
    assert_ne!(
        scroll(&fwd),
        scroll(&back),
        "`n` and `N` must move in opposite directions"
    );
}

/// No match, and no events, both leave the cursor alone rather than
/// moving it or panicking. `n - 1` underflows on an empty list, which
/// is what the `n == 0` guard is for.
#[tokio::test]
async fn detail_search_with_nothing_to_find_is_inert() {
    // Pattern that matches nothing.
    let mut app = detail_on("api-prod");
    {
        let d = app.detail.as_mut().expect("detail open");
        d.events = vec![make_event("alpha"), make_event("beta")];
        d.search_pattern = Some(regex::Regex::new("nothing-here").expect("valid regex"));
        d.events_scroll = 1;
    }
    app.detail_search_jump(1);
    assert_eq!(
        app.detail.as_ref().unwrap().events_scroll,
        1,
        "a search with no match leaves the cursor where it was"
    );

    // No events at all: the `n == 0` guard, without which `n - 1`
    // underflows before the search even starts.
    let any = regex::Regex::new(".").expect("valid regex");
    for delta in [1, -1] {
        let mut app = detail_on("api-prod");
        {
            let d = app.detail.as_mut().expect("detail open");
            d.events = Vec::new();
            d.search_pattern = Some(any.clone());
            d.events_scroll = 0;
        }
        app.detail_search_jump(delta);
        assert_eq!(app.detail.as_ref().unwrap().events_scroll, 0);
    }

    // No pattern set at all is a no-op too.
    let mut app = detail_on("api-prod");
    {
        let d = app.detail.as_mut().expect("detail open");
        d.events = vec![make_event("alpha")];
        d.search_pattern = None;
        d.events_scroll = 0;
    }
    app.detail_search_jump(1);
    assert_eq!(app.detail.as_ref().unwrap().events_scroll, 0);
}

// ── mutation-sweep triage, 2026-08-26 ────────────────────────────────
//
// Nine `handle_*` methods in `app/msg.rs` guard with
// `if <open thing>.env_name != env_name { return }`, and the sweep left
// every one of those comparisons flippable. This is the class that has
// already shipped twice here — the wrong-env spacious click and the
// `:rollback` wrong-env — so it gets one table rather than nine
// scattered assertions.
//
// Driven through `handle_msg`, which also exercises the generation
// check and the routing.

/// A result for a different environment must never land in the open
/// Detail view.
#[tokio::test]
async fn detail_results_for_another_env_are_dropped() {
    use crate::app::AppMsg;

    // Each case: an answer to another env's request (`worker-prod`, or a
    // same-named env in another region) arriving while Detail is open on
    // `api-prod`, and what it would have overwritten. Those carry a ticket
    // this Detail never issued; the log groups carry the other env's key.
    let armed = || {
        let mut app = detail_on("api-prod");
        {
            let d = app.detail.as_mut().expect("detail open");
            d.cw_log_groups = Some(vec!["existing-group".into()]);
        }
        app
    };

    // Log groups.
    let mut app = armed();
    app.handle_msg(AppMsg::DetailLogGroups {
        gen: app.generation,
        key: EnvKey::new("us-east-1", "worker-prod"),
        groups: vec!["wrong-env-group".into()],
    });
    assert_eq!(
        app.detail.as_ref().unwrap().cw_log_groups,
        Some(vec!["existing-group".to_string()]),
        "another env's log groups were shown against this one"
    );

    // Alarms.
    let mut app = armed();
    let _ = app.detail.as_mut().unwrap().cw_alarms.begin();
    app.handle_msg(AppMsg::DetailAlarms {
        gen: app.generation,
        ticket: crate::app::Request::stray_ticket(),
        result: Ok(Vec::new()),
    });
    assert!(
        app.detail.as_ref().unwrap().cw_alarms.ready().is_none(),
        "another env's alarm result settled this env's alarm panel"
    );

    // Recent versions.
    let mut app = armed();
    let _ = app.detail.as_mut().unwrap().recent_versions.begin();
    app.handle_msg(AppMsg::DetailRecentVersions {
        gen: app.generation,
        ticket: crate::app::Request::stray_ticket(),
        result: Ok(vec![]),
    });
    assert!(
        app.detail
            .as_ref()
            .unwrap()
            .recent_versions
            .ready()
            .is_none(),
        "another env's version list settled this env's panel"
    );

    // App tab metrics: dropped for another env, applied for this one.
    let app_metrics = || crate::app_tab::AppMetrics {
        series: Vec::new(),
        resources: Vec::new(),
        notes: vec!["marker".into()],
    };
    let mut app = armed();
    let ticket = app.detail.as_mut().unwrap().app_tab.metrics.begin();
    app.handle_msg(AppMsg::DetailAppMetrics {
        gen: app.generation,
        ticket: crate::app::Request::stray_ticket(),
        result: Ok(app_metrics()),
    });
    assert!(
        app.detail
            .as_ref()
            .unwrap()
            .app_tab
            .metrics
            .ready()
            .is_none(),
        "another env's App metrics settled this env's App tab"
    );
    app.handle_msg(AppMsg::DetailAppMetrics {
        gen: app.generation,
        ticket,
        result: Ok(app_metrics()),
    });
    assert_eq!(
        app.detail.as_ref().unwrap().app_tab.metrics.ready(),
        Some(&app_metrics()),
        "the matching env's App metrics must apply"
    );

    // Top errors: dropped for another env, applied for this one.
    let top = || crate::app_tab::TopErrors {
        log_group: "g".into(),
        rows: vec![],
        more: (0, 0),
        truncated: false,
        unmatched: 0,
        records_scanned: 0,
        fetched_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
    };
    let mut app = armed();
    // An earlier query, superseded after the stuck escape.
    let earlier = app.detail.as_mut().unwrap().app_tab.top_errors.begin();
    let ticket = app.detail.as_mut().unwrap().app_tab.top_errors.begin();
    app.handle_msg(AppMsg::DetailTopErrors {
        gen: app.generation,
        ticket: crate::app::Request::stray_ticket(),
        result: Ok(top()),
    });
    assert!(app
        .detail
        .as_ref()
        .unwrap()
        .app_tab
        .top_errors
        .ready()
        .is_none());
    app.handle_msg(AppMsg::DetailTopErrors {
        gen: app.generation,
        ticket: earlier,
        result: Ok(top()),
    });
    assert!(
        app.detail
            .as_ref()
            .unwrap()
            .app_tab
            .top_errors
            .ready()
            .is_none(),
        "only the latest query's answer settles the tab"
    );
    assert!(
        app.detail.as_ref().unwrap().app_tab.top_errors.in_flight(),
        "and the latest is still awaited"
    );
    app.handle_msg(AppMsg::DetailTopErrors {
        gen: app.generation,
        ticket,
        result: Ok(top()),
    });
    assert_eq!(
        app.detail.as_ref().unwrap().app_tab.top_errors.ready(),
        Some(&top())
    );

    // And the matching env DOES apply — without this half, a handler
    // that dropped everything would pass all of the above.
    let mut app = armed();
    app.handle_msg(AppMsg::DetailLogGroups {
        gen: app.generation,
        key: EnvKey::new("us-east-1", "api-prod"),
        groups: vec!["right-env-group".into()],
    });
    assert_eq!(
        app.detail.as_ref().unwrap().cw_log_groups,
        Some(vec!["right-env-group".to_string()]),
        "the matching env's result must be applied, or the drop tests \
         above prove nothing"
    );
}

/// The same guard on the form-prefill path: a prefill for another env
/// must not populate the open form.
#[tokio::test]
async fn a_form_prefill_for_another_env_is_dropped() {
    use crate::app::AppMsg;

    let armed = || {
        let mut app = test_app();
        app.environments = vec![mk_env("api-prod", "shop", "WebServer", "Green")];
        app.rebuild_view();
        app.table_state.select(Some(0));
        let mut form = crate::form::Form {
            title: "Capacity".into(),
            fields: vec![crate::form::FormField::text(
                "MinSize",
                "Min",
                None::<String>,
            )],
            cursor: 0,
            state: crate::form::FormState::Loading,
            // The prefill populates fields via these mappings — an
            // empty list means nothing lands, which would make the
            // "matching env applies" half pass vacuously.
            submit: crate::form::FormSubmit::OptionSettings {
                mappings: vec![(
                    "MinSize".to_string(),
                    "aws:autoscaling:asg".to_string(),
                    "MinSize".to_string(),
                )],
            },
            summary: "capacity".into(),
            env_name: "api-prod".into(),
            region: "us-east-1".into(),
            banner: String::new(),
            scroll: 0,
        };
        form.fields[0].value = "untouched".into();
        app.form = Some(form);
        app
    };

    let mut app = armed();
    app.handle_msg(AppMsg::FormPrefilled {
        gen: app.generation,
        key: home_key("worker-prod"),
        settings: Ok(vec![(
            "aws:autoscaling:asg".into(),
            "MinSize".into(),
            "99".into(),
        )]),
    });
    assert_eq!(
        app.form.as_ref().unwrap().fields[0].value,
        "untouched",
        "another env's settings were prefilled into this env's form — the \
         next submit would write them back to the wrong environment"
    );

    // The matching env applies.
    let mut app = armed();
    app.handle_msg(AppMsg::FormPrefilled {
        gen: app.generation,
        key: home_key("api-prod"),
        settings: Ok(vec![(
            "aws:autoscaling:asg".into(),
            "MinSize".into(),
            "4".into(),
        )]),
    });
    assert_ne!(
        app.form.as_ref().unwrap().fields[0].value,
        "untouched",
        "the matching env's prefill must be applied"
    );
}

#[tokio::test]
async fn a_stale_dlq_depth_says_so_rather_than_reading_as_current() {
    // The depth shown is the last-known value when the most recent
    // check failed. Presenting it as live would have an operator
    // conclude a queue had drained when nothing had been read at all —
    // the number is identical either way, so the suffix is the only
    // thing carrying the difference.
    let mut app = test_app();
    app.environments = vec![mk_env("worker-prod", "poly", "Worker", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    app.mode = crate::app::Mode::Detail;
    app.caches.worker_dlq.set(home_key("worker-prod"), 42);

    let live = render(&mut app, 200, 40);
    assert!(
        live.contains("DLQ:42"),
        "a worker env shows its DLQ depth; got:\n{live}"
    );
    assert!(
        !live.contains("DLQ:42 (stale)"),
        "a fresh reading is not marked stale; got:\n{live}"
    );

    app.caches
        .worker_dlq
        .reconcile(home_key("worker-prod"), Err(()));
    let stale = render(&mut app, 200, 40);
    assert!(
        stale.contains("DLQ:42 (stale)"),
        "a failed check marks the depth stale; got:\n{stale}"
    );
}

#[tokio::test]
async fn the_detail_header_drops_whole_fields_on_a_narrow_terminal() {
    // Detail replaces the whole screen and draws no breadcrumb, so this
    // header is the only thing on it saying what you are looking at.
    // ratatui clips the right edge, which rendered `CNAME: api-pr` — a
    // label promising a value and then cutting it in half.
    let mut app = detail_on("api-prod");

    let narrow = render(&mut app, 80, 26);
    let dangling = narrow
        .lines()
        .any(|l| l.contains("CNAME:") && !l.contains("example.com"));
    assert!(
        !dangling,
        "a CNAME label with a half-rendered value; got:\n{narrow}"
    );
    // The fields that identify the env survive — they are first for
    // exactly that reason.
    assert!(narrow.contains("Region:"), "got:\n{narrow}");
    assert!(narrow.contains("Platform:"), "got:\n{narrow}");

    // Given room, everything is shown in full.
    let wide = render(&mut app, 200, 26);
    assert!(
        wide.contains("CNAME:") && wide.contains("example.com"),
        "CNAME returns when there is room; got:\n{wide}"
    );
}

#[tokio::test]
async fn the_detail_header_keeps_the_env_identity_at_any_width() {
    // Whatever else goes, the row has to say which env and how it is.
    let mut app = detail_on("api-prod");
    for w in [70u16, 80, 100, 160] {
        let f = render(&mut app, w, 26);
        assert!(f.contains("api-prod"), "no env name at {w}; got:\n{f}");
        assert!(f.contains("Health:"), "no health at {w}; got:\n{f}");
    }
}

/// `x` arms a destructive action, and WHICH one depends entirely on the
/// tab guard.
///
/// On Config it arms deletion of a configuration row; on Instances it
/// arms termination of an EC2 instance. Both guards were free in the
/// 2026-08-28 whole-tree sweep — six mutants each, replacing the
/// `matches!(… DetailTab::Instances)` guard with `true` and with
/// `false`. A guard stuck `true` arms an instance termination from the
/// Config tab; stuck `false`, `x` falls through to whatever arm catches
/// it next. Either way the operator arms something they were not
/// looking at, and the confirm prompt is the only thing standing
/// between that and a dispatch.
///
/// Table-driven over the tabs so a new tab cannot be added without a
/// row here — the guard is per-tab and a test naming two of them proves
/// nothing about the third.
#[tokio::test]
async fn x_arms_the_delete_belonging_to_the_focused_tab_and_no_other() {
    let armed = |app: &App| -> (Option<usize>, Option<usize>) {
        let d = app.detail.as_ref().expect("detail open");
        (d.config_delete_confirm, d.instance_terminate_confirm)
    };
    let open_on = |tab: DetailTab| {
        let mut app = detail_on("api-prod");
        {
            let d = app.detail.as_mut().expect("detail open");
            // Both tabs populated every time: an empty tab arms nothing
            // regardless of the guard, which would let a broken guard
            // pass for the wrong reason.
            d.tags = vec![
                ("Owner".into(), "platform".into()),
                ("Team".into(), "infra".into()),
            ];
            d.config_cursor = 0;
            d.instances = vec![crate::aws::Instance {
                id: "i-0123456789abcdef0".into(),
                health: "Ok".into(),
                color: "Green".into(),
                causes: vec![],
                instance_type: "t3.small".into(),
                availability_zone: "us-east-1a".into(),
                launched_at: None,
                app_stats: None,
            }];
            d.instances_cursor = 0;
        }
        focus_tab(&mut app, tab);
        app
    };

    // Config tab: arms the config-row delete and nothing else.
    let mut app = open_on(DetailTab::Config);
    press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
    assert_eq!(
        armed(&app),
        (Some(0), None),
        "`x` on Config must arm the CONFIG delete only"
    );

    // Instances tab: arms the instance termination and nothing else.
    let mut app = open_on(DetailTab::Instances);
    press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
    assert_eq!(
        armed(&app),
        (None, Some(0)),
        "`x` on Instances must arm the INSTANCE termination only"
    );

    // Every other tab: `x` arms nothing destructive at all.
    for tab in [
        DetailTab::Health,
        DetailTab::Events,
        DetailTab::Metrics,
        DetailTab::Logs,
    ] {
        let mut app = open_on(tab);
        press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(
            armed(&app),
            (None, None),
            "`x` on {tab:?} armed a delete that belongs to another tab"
        );
    }
}

/// A failed tags / env-vars fetch must be visible, not just logged.
///
/// `DetailFetch` had no `Tags` or `EnvVars` variant, so both handlers
/// logged the failure with `tracing::warn!` and left the panel showing
/// an EMPTY list — indistinguishable from an environment that genuinely
/// has no tags and no environment variables. An AccessDenied on the
/// config fetch read as a clean result, in the panel an operator uses
/// during triage.
#[tokio::test]
async fn a_failed_config_fetch_is_shown_not_just_logged() {
    for (tags, needle) in [
        (true, "ListTagsForResource"),
        (false, "DescribeConfigurationSettings"),
    ] {
        let mut app = test_app();
        app.environments = vec![mk_env("api-prod", "poly", "Web", "Green")];
        app.rebuild_view();
        app.table_state.select(Some(0));
        app.open_detail();

        let msg = if tags {
            AppMsg::DetailTags {
                gen: 0,
                ticket: detail_ticket(&mut app, |d| &mut d.tags_request),
                result: Err("AccessDenied: elasticbeanstalk:ListTagsForResource".into()),
            }
        } else {
            AppMsg::DetailEnvVars {
                gen: 0,
                ticket: detail_ticket(&mut app, |d| &mut d.env_vars_request),
                result: Err("AccessDenied: DescribeConfigurationSettings".into()),
            }
        };
        app.handle_msg(msg);

        let d = app.detail.as_ref().expect("detail open");
        let err = d
            .error
            .as_ref()
            .unwrap_or_else(|| panic!("a failed fetch must surface: {needle}"));
        assert!(
            err.message.contains(needle),
            "the operator must see which call failed: {}",
            err.message
        );
    }
}

/// And a later success must clear it — without speaking for a sibling.
#[tokio::test]
async fn a_successful_config_refetch_clears_only_its_own_error() {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "poly", "Web", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();

    let ticket = detail_ticket(&mut app, |d| &mut d.tags_request);
    app.handle_msg(AppMsg::DetailTags {
        gen: 0,
        ticket,
        result: Err("tags boom".into()),
    });
    assert!(app.detail.as_ref().unwrap().error.is_some());

    // An ENV VARS success must not clear the TAGS error — that is the
    // whole contract of `clear_error`: a success must not speak for a
    // sibling fetch.
    let ticket = detail_ticket(&mut app, |d| &mut d.env_vars_request);
    app.handle_msg(AppMsg::DetailEnvVars {
        gen: 0,
        ticket,
        result: Ok(vec![("K".into(), "V".into())]),
    });
    assert!(
        app.detail
            .as_ref()
            .unwrap()
            .error
            .as_ref()
            .is_some_and(|e| e.message.contains("tags boom")),
        "an env-vars success must not clear the tags failure"
    );

    // The matching success does clear it.
    let ticket = detail_ticket(&mut app, |d| &mut d.tags_request);
    app.handle_msg(AppMsg::DetailTags {
        gen: 0,
        ticket,
        result: Ok(vec![("t".into(), "v".into())]),
    });
    assert!(
        app.detail.as_ref().unwrap().error.is_none(),
        "the tags refetch succeeded, so its error must go"
    );
}

/// Fetches with a `*_request` but no `DetailFetch` variant to
/// report a failure against.
///
/// Pure, over the source text, so the "one is missing" case can be
/// proven to fail without adding a field to the real `DetailState` —
/// which does not compile until every struct literal in the tree is
/// updated, making the experiment inconclusive rather than a result.
fn unpaired_detail_fetches(src: &str) -> Vec<String> {
    let pascal = |snake: &str| -> String {
        snake
            .split('_')
            .map(|w| {
                let mut c = w.chars();
                match c.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    None => String::new(),
                }
            })
            .collect()
    };
    let variants = src
        .split("pub(crate) enum DetailFetch {")
        .nth(1)
        .and_then(|s| s.split('}').next())
        .unwrap_or("");
    src.lines()
        .filter_map(|l| {
            let t = l.trim();
            t.strip_prefix("pub ")
                .and_then(|r| r.strip_suffix("_request: crate::app::Request,"))
        })
        .filter(|f| !variants.contains(&pascal(f)))
        .map(str::to_string)
        .collect()
}

/// Every `*_request` fetch must have a `DetailFetch` variant to report
/// its failure against.
///
/// Two of the six did not, so their handlers had nowhere to put an
/// error and logged it instead — leaving an empty panel that reads as
/// "nothing configured". The pairing is invisible at a glance: adding a
/// `x_request` field and its handler is a natural change that gives no
/// hint an error channel is also required.
#[test]
fn every_detail_fetch_can_report_a_failure() {
    let src = std::fs::read_to_string("src/mode_detail.rs").expect("read mode_detail.rs");
    let unpaired = unpaired_detail_fetches(&src);
    assert!(
        unpaired.is_empty(),
        "these fetches have no `DetailFetch` variant, so a failure has \
         nowhere to be reported and the panel shows an empty list that \
         reads as 'nothing configured': {unpaired:?}"
    );
}

/// The guard must FIRE on a missing pairing, not merely pass on a clean
/// tree — a scan that stopped matching would look identical.
#[test]
fn the_unpaired_fetch_guard_fires_on_a_missing_variant() {
    let synthetic = "\
pub(crate) enum DetailFetch {
    Events,
}
struct S {
    pub events_request: crate::app::Request,
    pub probe_request: crate::app::Request,
}
";
    assert_eq!(
        unpaired_detail_fetches(synthetic),
        vec!["probe".to_string()],
        "a flag with no variant must be reported"
    );

    let paired = "\
pub(crate) enum DetailFetch {
    Events,
    Probe,
}
struct S {
    pub events_request: crate::app::Request,
    pub probe_request: crate::app::Request,
}
";
    assert!(
        unpaired_detail_fetches(paired).is_empty(),
        "and a complete pairing must not be"
    );
}

/// The canary: the scan and the snake→Pascal conversion must both work,
/// or the loop above passes over an empty list.
#[test]
fn the_detail_fetch_scan_can_see_its_inputs() {
    let src = std::fs::read_to_string("src/mode_detail.rs").expect("read mode_detail.rs");
    assert!(
        src.contains("pub env_vars_request: crate::app::Request,"),
        "the field shape this guard scans for has changed"
    );
    assert!(
        src.split("pub(crate) enum DetailFetch {")
            .nth(1)
            .is_some_and(|s| s.split('}').next().is_some_and(|v| v.contains("EnvVars"))),
        "the variant shape this guard scans for has changed"
    );
}

/// `[` `]` on the App tab move the range shared with the Metrics tab,
/// and refetch both: each tab drops a result for the old range, so each
/// needs a fetch for the new one or it stays "loading" and refuses
/// refreshes until the stuck timeout.
#[tokio::test]
async fn the_range_keys_refetch_both_tabs_that_share_the_range() {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "shop", "WebServer", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    let d = app.detail.as_mut().unwrap();
    d.tab_idx = d.tabs.iter().position(|t| *t == DetailTab::App).unwrap();
    d.app_tab.metrics.settle_now(Ok(Default::default()));
    d.metrics_request = Default::default();
    assert_eq!(d.metrics_range_secs, 3600);

    press(&mut app, KeyCode::Char(']'), KeyModifiers::NONE);
    let d = app.detail.as_ref().unwrap();
    assert_eq!(d.metrics_range_secs, 21_600, "] widens the range");
    assert!(d.app_tab.metrics.in_flight(), "the App tab refetches");
    assert!(
        d.metrics_request.in_flight(),
        "and so does the Metrics tab: the range is shared, and its old-range \
         result will be dropped, so only a new fetch clears its loading state"
    );

    press(&mut app, KeyCode::Char('['), KeyModifiers::NONE);
    assert_eq!(app.detail.as_ref().unwrap().metrics_range_secs, 3600);
}

/// The App tab never calls AWS without config or in demo mode, and in
/// demo mode it still draws the configured rows, with a note saying
/// why they are empty.
#[tokio::test]
async fn the_app_tab_settles_at_once_without_config_or_in_demo_mode() {
    let fetch = |demo: bool, config: &str| {
        let mut app = App::for_tests(crate::aws::AwsClient::stub(), crate::config::parse(config));
        app.demo_mode = demo;
        app.environments = vec![mk_env("api-prod", "shop", "WebServer", "Green")];
        app.rebuild_view();
        app.table_state.select(Some(0));
        app.open_detail();
        while app.msg_rx.try_recv().is_ok() {}
        app.spawn_detail_app("api-prod".into());
        let msg = app.msg_rx.try_recv().expect("settled without a round trip");
        app.handle_msg(msg);
        app.detail
            .as_ref()
            .unwrap()
            .app_tab
            .metrics
            .ready()
            .cloned()
            .expect("settled ok")
    };
    let none = fetch(false, "");
    assert!(none.series.is_empty() && none.notes.is_empty());

    let demo = fetch(
        true,
        "app.api-prod.namespace = \"N\"\napp.api-prod.metric.users = \"U\"\n",
    );
    assert_eq!(demo.series.len(), 1, "the configured row is still drawn");
    assert!(demo.series[0].points.is_empty());
    assert_eq!(demo.notes, ["demo mode makes no CloudWatch calls"]);
}

/// The App tab's instance rows come from the instance list, so its
/// refresh refetches that list — unless a fetch is already running,
/// which must not stop the tab's own metrics fetch either.
#[tokio::test]
async fn the_app_tab_refresh_brings_the_instance_list_along() {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "shop", "WebServer", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    let d = app.detail.as_mut().unwrap();
    assert!(
        d.instances_request.in_flight(),
        "open_detail fetches instances eagerly"
    );
    d.tab_idx = d.tabs.iter().position(|t| *t == DetailTab::App).unwrap();
    app.detail_refresh_active_tab();
    assert!(
        app.detail.as_ref().unwrap().app_tab.metrics.in_flight(),
        "an instance fetch in flight must not block the App tab's own"
    );

    let d = app.detail.as_mut().unwrap();
    d.instances_request = Default::default();
    d.app_tab.metrics.settle_now(Ok(Default::default()));
    d.cw_alarms.settle_now(Ok(Vec::new()));
    app.detail_refresh_active_tab();
    let d = app.detail.as_ref().unwrap();
    assert!(
        d.instances_request.in_flight(),
        "the App refresh refetches instances"
    );
    assert!(d.cw_alarms.in_flight(), "and alarms, for its alarm line");
    assert!(d.app_tab.metrics.in_flight());
}

/// Every TUI alarm fetch applies the env's `app.ENV.alarm_prefix`: the
/// Health tab (and so the App tab), `:why`, and `:alarms`. One case per
/// caller — a prefix that applied on one surface and not another would
/// make them disagree about which alarms are the env's.
#[tokio::test]
async fn every_tui_alarm_fetch_takes_the_envs_alarm_prefix() {
    use aws_sdk_cloudwatch::operation::describe_alarms::DescribeAlarmsOutput;
    use aws_sdk_cloudwatch::types::{Dimension, MetricAlarm, StateValue};
    fn app_with_alarms() -> App {
        let rule = aws_smithy_mocks::mock!(aws_sdk_cloudwatch::Client::describe_alarms)
            .then_output(|| {
                DescribeAlarmsOutput::builder()
                    .metric_alarms(
                        MetricAlarm::builder()
                            .alarm_name("api-prod-db-cpu")
                            .state_value(StateValue::Alarm)
                            .dimensions(
                                Dimension::builder()
                                    .name("DBInstanceIdentifier")
                                    .value("db")
                                    .build(),
                            )
                            .build(),
                    )
                    // The worker's, which the prefix also covers.
                    .metric_alarms(
                        MetricAlarm::builder()
                            .alarm_name("api-prod-wk-health")
                            .state_value(StateValue::Alarm)
                            .build(),
                    )
                    .build()
            });
        let cw = aws_smithy_mocks::mock_client!(
            aws_sdk_cloudwatch,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&rule]
        );
        let sdk = aws_config::SdkConfig::builder()
            .region(aws_config::Region::new("us-east-1"))
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build();
        let client = crate::aws::AwsClient::for_tests(
            aws_sdk_elasticbeanstalk::Client::new(&sdk),
            aws_sdk_sqs::Client::new(&sdk),
            cw,
            aws_sdk_cloudwatchlogs::Client::new(&sdk),
            aws_sdk_s3::Client::new(&sdk),
            aws_sdk_ec2::Client::new(&sdk),
        );
        let cfg = crate::config::parse("app.api-prod.alarm_prefix = \"api-prod-\"\n");
        let mut app = App::for_tests(client, cfg);
        app.environments = vec![
            mk_env("api-prod", "shop", "WebServer", "Green"),
            mk_env("api-prod-wk", "shop", "Worker", "Green"),
        ];
        app.rebuild_view();
        app.table_state.select(Some(0));
        app
    }
    async fn alarm_names(app: &mut App) -> Vec<String> {
        // Bounded, as a rejected mock request would otherwise hang.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let msg = tokio::time::timeout_at(deadline, app.msg_rx.recv())
                .await
                .expect("an alarm result within 10s")
                .expect("a result");
            let result = match msg {
                AppMsg::DetailAlarms { result, .. }
                | AppMsg::WhyRedAlarms { result, .. }
                | AppMsg::Alarms { result, .. } => result,
                _ => continue,
            };
            return result
                .expect("fetch ok")
                .into_iter()
                .map(|a| a.name)
                .collect();
        }
    }

    let mut app = app_with_alarms();
    app.open_detail();
    while app.msg_rx.try_recv().is_ok() {}
    app.spawn_detail_alarms("api-prod".into());
    assert_eq!(
        alarm_names(&mut app).await,
        ["api-prod-db-cpu"],
        "Health / App tab"
    );

    let mut app = app_with_alarms();
    app.spawn_why_red_alarms("api-prod".into(), 1);
    assert_eq!(alarm_names(&mut app).await, ["api-prod-db-cpu"], ":why");

    let mut app = app_with_alarms();
    app.spawn_alarms_fetch(&mk_env("api-prod", "poly", "Web", "Green"));
    assert_eq!(alarm_names(&mut app).await, ["api-prod-db-cpu"], ":alarms");
}

/// Top errors runs on `^R` and when the tab opens, never on the
/// auto-refresh tick (Logs Insights bills per GB scanned), never
/// stacked on a query still running, never without `app.ENV.log_group`
/// and never in demo mode.
#[tokio::test]
async fn top_errors_runs_when_a_person_asks_and_only_then() {
    let armed = |config: &str, demo: bool| {
        let mut app = App::for_tests(crate::aws::AwsClient::stub(), crate::config::parse(config));
        app.demo_mode = demo;
        app.environments = vec![mk_env("api-prod", "shop", "WebServer", "Green")];
        app.rebuild_view();
        app.table_state.select(Some(0));
        app.open_detail();
        let d = app.detail.as_mut().unwrap();
        d.tab_idx = d.tabs.iter().position(|t| *t == DetailTab::App).unwrap();
        app
    };
    let configured = "app.api-prod.log_group = \"var/log/app.log\"\n";
    let running = |app: &App| app.detail.as_ref().unwrap().app_tab.top_errors.in_flight();

    let mut app = armed(configured, false);
    app.detail_refresh_on_tick();
    assert!(!running(&app), "the tick does not run it");
    assert!(
        app.detail.as_ref().unwrap().app_tab.metrics.in_flight(),
        "but does refresh metrics"
    );

    let mut app = armed(configured, false);
    app.detail_refresh_active_tab();
    assert!(running(&app), "^R runs it");
    // A second ask while it runs does not start another: one query, so
    // one answer (the stub client fails at once).
    app.spawn_detail_top_errors("api-prod".into());
    let mut answers = 0;
    while let Ok(Some(msg)) =
        tokio::time::timeout(std::time::Duration::from_millis(500), app.msg_rx.recv()).await
    {
        answers += usize::from(matches!(msg, AppMsg::DetailTopErrors { .. }));
    }
    assert_eq!(answers, 1, "a query in flight is not stacked on");

    // The run loop's tick, with auto-refresh on: metrics, not the query.
    let mut app = armed(configured, false);
    app.mode = Mode::Detail;
    app.detail.as_mut().unwrap().auto_refresh = true;
    app.refresh_open_detail_on_tick();
    assert!(
        app.detail.as_ref().unwrap().app_tab.metrics.in_flight(),
        "the tick refreshes"
    );
    assert!(!running(&app), "but leaves the query alone");
    let mut app = armed(configured, false);
    app.mode = Mode::Detail;
    app.refresh_open_detail_on_tick();
    assert!(
        !app.detail.as_ref().unwrap().app_tab.metrics.in_flight(),
        "auto-refresh off: the tick does nothing"
    );

    let mut app = armed("app.api-prod.namespace = \"N\"\n", false);
    app.detail_refresh_active_tab();
    assert!(!running(&app), "no log_group, no query");

    let mut app = armed(configured, true);
    app.detail_refresh_active_tab();
    assert!(!running(&app), "demo mode makes no Logs Insights call");

    // Switching to the tab is asking.
    let mut app = armed(configured, false);
    app.detail.as_mut().unwrap().tab_idx = 0;
    app.detail_cycle_tab(1);
    assert_eq!(app.detail.as_ref().unwrap().tab(), DetailTab::App);
    assert!(running(&app), "opening the tab with no answer runs it");

    // Opening it again with a fresh answer keeps that answer; a stale
    // one, or ^R, runs it again.
    let answer = |mins_ago: i64| crate::app_tab::TopErrors {
        log_group: "g".into(),
        rows: vec![],
        more: (0, 0),
        truncated: false,
        unmatched: 0,
        records_scanned: 0,
        fetched_at: chrono::Utc::now() - chrono::Duration::minutes(mins_ago),
    };
    let reopen = |mins_ago: i64| {
        let mut app = armed(configured, false);
        app.detail
            .as_mut()
            .unwrap()
            .app_tab
            .top_errors
            .settle_now(Ok(answer(mins_ago)));
        app.detail.as_mut().unwrap().tab_idx = 0;
        app.detail_cycle_tab(1);
        app
    };
    assert!(
        !running(&reopen(1)),
        "a 1-minute-old answer stands on reopening"
    );
    assert!(running(&reopen(10)), "a 10-minute-old one is rerun");
    let mut app = reopen(1);
    app.detail
        .as_mut()
        .unwrap()
        .app_tab
        .metrics
        .settle_now(Ok(Default::default()));
    app.detail_refresh_active_tab();
    assert!(running(&app), "^R reruns even a fresh answer");
}

/// The query top errors sends: the configured fingerprint, against the
/// configured log group under the env, over the last fifteen minutes;
/// and its rows land on the tab.
#[tokio::test]
async fn top_errors_queries_the_configured_group_with_the_fingerprint() {
    use aws_sdk_cloudwatchlogs::operation::get_query_results::GetQueryResultsOutput;
    use aws_sdk_cloudwatchlogs::operation::start_query::StartQueryOutput;
    use aws_sdk_cloudwatchlogs::types::{QueryStatistics, QueryStatus, ResultField};
    let start = aws_smithy_mocks::mock!(aws_sdk_cloudwatchlogs::Client::start_query)
        .match_requests(|req| {
            let window = req.end_time().unwrap_or(0) - req.start_time().unwrap_or(0);
            req.log_group_names() == ["/aws/elasticbeanstalk/api-prod/var/log/app.log"]
                && req
                    .query_string()
                    .is_some_and(|q| q.contains(r"parse @message /ERROR (?<fp>\w+)/"))
                && (899..=901).contains(&window)
        })
        .then_output(|| StartQueryOutput::builder().query_id("q1").build());
    let results = aws_smithy_mocks::mock!(aws_sdk_cloudwatchlogs::Client::get_query_results)
        .then_output(|| {
            let field = |k: &str, v: &str| ResultField::builder().field(k).value(v).build();
            GetQueryResultsOutput::builder()
                .status(QueryStatus::Complete)
                .results(vec![field("fp", "Boom"), field("n", "7")])
                .statistics(QueryStatistics::builder().records_scanned(50.0).build())
                .build()
        });
    // Opening Detail looks up the env's log groups for the Logs tab.
    let groups = aws_smithy_mocks::mock!(aws_sdk_cloudwatchlogs::Client::describe_log_groups)
        .then_output(|| {
            aws_sdk_cloudwatchlogs::operation::describe_log_groups::DescribeLogGroupsOutput::builder()
                .build()
        });
    let logs = aws_smithy_mocks::mock_client!(
        aws_sdk_cloudwatchlogs,
        aws_smithy_mocks::RuleMode::MatchAny,
        [&start, &results, &groups]
    );
    let sdk = aws_config::SdkConfig::builder()
        .region(aws_config::Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    let client = crate::aws::AwsClient::for_tests(
        aws_sdk_elasticbeanstalk::Client::new(&sdk),
        aws_sdk_sqs::Client::new(&sdk),
        aws_sdk_cloudwatch::Client::new(&sdk),
        logs,
        aws_sdk_s3::Client::new(&sdk),
        aws_sdk_ec2::Client::new(&sdk),
    );
    let cfg = crate::config::parse(
        "app.api-prod.log_group = \"var/log/app.log\"\n\
         app.api-prod.error_fingerprint = \"ERROR (?<fp>\\w+)\"\n",
    );
    let mut app = App::for_tests(client, cfg);
    app.environments = vec![mk_env("api-prod", "shop", "WebServer", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    while app.msg_rx.try_recv().is_ok() {}
    app.spawn_detail_top_errors("api-prod".into());
    // Bounded: a mock that rejects the request panics inside the
    // spawned task, and an unbounded wait would hang the suite.
    let msg = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            match app.msg_rx.recv().await.expect("a result") {
                m @ AppMsg::DetailTopErrors { .. } => break m,
                _ => continue,
            }
        }
    })
    .await
    .expect("top errors answered within 10s");
    app.handle_msg(msg);
    let t = app
        .detail
        .as_ref()
        .unwrap()
        .app_tab
        .top_errors
        .ready()
        .cloned()
        .expect("settled ok");
    assert_eq!(t.rows, [("Boom".to_string(), 7)]);
    assert_eq!(t.records_scanned, 50);
    assert_eq!(start.num_calls(), 1);
}

/// A top-errors query whose task never answered stops blocking the next
/// one once it is older than any answer could be.
#[tokio::test]
async fn a_top_errors_query_that_never_answered_does_not_block_forever() {
    let mut app = App::for_tests(
        crate::aws::AwsClient::stub(),
        crate::config::parse("app.api-prod.log_group = \"g\"\n"),
    );
    app.environments = vec![mk_env("api-prod", "shop", "WebServer", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    let long_ago = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(600))
        .expect("the clock is past ten minutes");
    let d = app.detail.as_mut().unwrap();
    let _ = d.app_tab.top_errors.begin();
    d.app_tab.top_errors.backdate(long_ago);
    app.spawn_detail_top_errors("api-prod".into());
    let started = app
        .detail
        .as_ref()
        .unwrap()
        .app_tab
        .top_errors
        .in_flight_since()
        .unwrap();
    assert!(started > long_ago, "a fresh query replaced the stuck one");
}

/// The TUI's top-errors query is bounded too: past
/// `TOP_ERRORS_BOUND` it is stopped on the server and the tab gets an
/// error, not a spinner that never ends.
#[tokio::test(start_paused = true)]
async fn the_tabs_top_errors_query_is_stopped_past_its_bound() {
    use aws_sdk_cloudwatchlogs::types::QueryStatus;
    let start =
        aws_smithy_mocks::mock!(aws_sdk_cloudwatchlogs::Client::start_query).then_output(|| {
            aws_sdk_cloudwatchlogs::operation::start_query::StartQueryOutput::builder()
                .query_id("q")
                .build()
        });
    let running = aws_smithy_mocks::mock!(aws_sdk_cloudwatchlogs::Client::get_query_results)
        .then_output(|| {
            aws_sdk_cloudwatchlogs::operation::get_query_results::GetQueryResultsOutput::builder()
                .status(QueryStatus::Running)
                .build()
        });
    // Opening Detail looks up the env's log groups for the Logs tab.
    let groups = aws_smithy_mocks::mock!(aws_sdk_cloudwatchlogs::Client::describe_log_groups)
        .then_output(|| {
            aws_sdk_cloudwatchlogs::operation::describe_log_groups::DescribeLogGroupsOutput::builder()
                .build()
        });
    let stop =
        aws_smithy_mocks::mock!(aws_sdk_cloudwatchlogs::Client::stop_query).then_output(|| {
            aws_sdk_cloudwatchlogs::operation::stop_query::StopQueryOutput::builder().build()
        });
    let sdk = aws_config::SdkConfig::builder()
        .region(aws_config::Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    let client = crate::aws::AwsClient::for_tests(
        aws_sdk_elasticbeanstalk::Client::new(&sdk),
        aws_sdk_sqs::Client::new(&sdk),
        aws_sdk_cloudwatch::Client::new(&sdk),
        aws_smithy_mocks::mock_client!(
            aws_sdk_cloudwatchlogs,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&start, &running, &stop, &groups]
        ),
        aws_sdk_s3::Client::new(&sdk),
        aws_sdk_ec2::Client::new(&sdk),
    );
    let mut app = App::for_tests(
        client,
        crate::config::parse("app.api-prod.log_group = \"g\"\n"),
    );
    app.environments = vec![mk_env("api-prod", "shop", "WebServer", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    while app.msg_rx.try_recv().is_ok() {}
    app.spawn_detail_top_errors("api-prod".into());
    // Virtual time: generous, and a hang fails instead of stalling.
    let result = tokio::time::timeout(std::time::Duration::from_secs(600), async {
        loop {
            match app.msg_rx.recv().await.expect("a result") {
                AppMsg::DetailTopErrors { result, .. } => break result,
                _ => continue,
            }
        }
    })
    .await
    .expect("an answer within the bound");
    let err = result.expect_err("the query never completed");
    assert!(err.contains("ran past 120s and was stopped"), "{err}");
    assert_eq!(stop.num_calls(), 1);
}

/// `[` then `]` fires two fetches; whichever lands last, only the one
/// for the range on screen is shown. One case per tab.
#[tokio::test]
async fn a_metrics_result_for_a_range_left_behind_is_dropped() {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "shop", "WebServer", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    let d = app.detail.as_mut().unwrap();
    // The fetch for the old range, then the range change's.
    let old = d.app_tab.metrics.begin();
    let old_metrics = d.metrics_request.begin();
    d.metrics_range_secs = 86_400;
    let new = d.app_tab.metrics.begin();
    let new_metrics = d.metrics_request.begin();
    let series = |label: &str| crate::aws::MetricSeries {
        id: label.into(),
        label: label.into(),
        points: vec![],
    };
    let app_metrics = |note: &str| crate::app_tab::AppMetrics {
        notes: vec![note.into()],
        ..Default::default()
    };

    // The old range's answers land: dropped, still loading.
    app.handle_msg(AppMsg::DetailMetrics {
        gen: app.generation,
        ticket: old_metrics,
        result: Ok((vec![series("old")], None)),
    });
    app.handle_msg(AppMsg::DetailAppMetrics {
        gen: app.generation,
        ticket: old,
        result: Ok(app_metrics("old")),
    });
    let d = app.detail.as_ref().unwrap();
    assert!(
        d.metrics.is_empty() && d.metrics_request.in_flight(),
        "Metrics tab kept waiting"
    );
    assert!(
        d.app_tab.metrics.ready().is_none() && d.app_tab.metrics.in_flight(),
        "App tab kept waiting"
    );

    // The current range's answers apply.
    app.handle_msg(AppMsg::DetailMetrics {
        gen: app.generation,
        ticket: new_metrics,
        result: Ok((vec![series("new")], None)),
    });
    app.handle_msg(AppMsg::DetailAppMetrics {
        gen: app.generation,
        ticket: new,
        result: Ok(app_metrics("new")),
    });
    let d = app.detail.as_ref().unwrap();
    assert_eq!(d.metrics[0].label, "new");
    assert!(!d.metrics_request.in_flight());
    assert_eq!(
        d.app_tab.metrics.ready().map(|m| m.notes[0].as_str()),
        Some("new")
    );
}

/// The case a range check could not catch: the range changed and
/// changed back (or `^R` twice), so the older request is for the SAME
/// range as the newer one, and its late answer used to land over it
/// and end the loading state while the newer request was still out.
#[tokio::test]
async fn an_older_metrics_answer_for_the_same_range_is_dropped() {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "shop", "WebServer", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    let d = app.detail.as_mut().unwrap();
    let first = d.metrics_request.begin();
    let second = d.metrics_request.begin();
    let series = |label: &str| crate::aws::MetricSeries {
        id: label.into(),
        label: label.into(),
        points: vec![],
    };
    app.handle_msg(AppMsg::DetailMetrics {
        gen: app.generation,
        ticket: first,
        result: Ok((vec![series("first")], None)),
    });
    let d = app.detail.as_ref().unwrap();
    assert!(d.metrics.is_empty(), "the older answer was dropped");
    assert!(
        d.metrics_request.in_flight(),
        "the newer request is still out"
    );
    app.handle_msg(AppMsg::DetailMetrics {
        gen: app.generation,
        ticket: second,
        result: Ok((vec![series("second")], None)),
    });
    assert_eq!(app.detail.as_ref().unwrap().metrics[0].label, "second");
}

/// Starting a fetch clears only that fetch's own error. Each of these
/// used to clear every error, so pressing `]` (which refetches metrics
/// from the App tab too) erased a failed instance fetch and the tab
/// read as "no instances" rather than "could not look". One case per
/// fetch.
#[tokio::test]
async fn starting_a_fetch_does_not_erase_a_siblings_error() {
    use crate::mode_detail::DetailFetch;
    let mut app = test_app();
    app.environments = vec![mk_env("wk-prod", "poly", "Worker", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    type Spawn = fn(&mut App);
    let spawns: [(DetailFetch, Spawn); 4] = [
        (DetailFetch::Metrics, |a| {
            a.spawn_detail_metrics("wk-prod".into())
        }),
        (DetailFetch::Queues, |a| {
            a.spawn_detail_queues("poly".into(), "wk-prod".into())
        }),
        (DetailFetch::Events, |a| {
            a.spawn_detail_events("wk-prod".into())
        }),
        (DetailFetch::Instances, |a| {
            a.spawn_detail_instances("wk-prod".into())
        }),
    ];
    for (own, spawn) in spawns {
        let sibling = DetailFetch::Tags;
        app.detail
            .as_mut()
            .unwrap()
            .set_error(sibling, "denied".into());
        spawn(&mut app);
        assert_eq!(
            app.detail
                .as_ref()
                .unwrap()
                .error
                .as_ref()
                .map(|e| e.source),
            Some(sibling),
            "starting {own:?} erased {sibling:?}'s error"
        );
        app.detail
            .as_mut()
            .unwrap()
            .set_error(own, "its own".into());
        spawn(&mut app);
        assert_eq!(
            app.detail.as_ref().unwrap().error,
            None,
            "{own:?} clears its own"
        );
    }
}

/// A failed custom-metric fetch is shown, beside the built-in series it
/// no longer takes down with it.
#[test]
fn a_failed_custom_metric_fetch_is_shown_not_dropped() {
    let series = |label: &str| crate::aws::MetricSeries {
        id: label.into(),
        label: label.into(),
        points: vec![],
    };
    let merge = super::super::spawn_detail::merge_metrics;
    assert_eq!(
        merge(Ok(vec![series("health")]), Err("AccessDenied".into())).map(|(s, p)| (s.len(), p)),
        Ok((1, Some("custom metrics: AccessDenied".to_string())))
    );
    assert_eq!(
        merge(Ok(vec![series("health")]), Ok(vec![series("mine")])).map(|(s, p)| (s.len(), p)),
        Ok((2, None))
    );
    assert_eq!(
        merge(Err("down".into()), Ok(vec![series("mine")])).map(|(s, p)| (s.len(), p)),
        Err("down".to_string())
    );
}

#[tokio::test]
async fn a_partial_metrics_result_draws_and_reports_its_failure() {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "shop", "WebServer", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    let series = crate::aws::MetricSeries {
        id: "health".into(),
        label: "health".into(),
        points: vec![],
    };
    let ticket = app.detail.as_mut().unwrap().metrics_request.begin();
    app.handle_msg(AppMsg::DetailMetrics {
        gen: app.generation,
        ticket,
        result: Ok((vec![series], Some("custom metrics: AccessDenied".into()))),
    });
    let d = app.detail.as_ref().unwrap();
    assert_eq!(d.metrics.len(), 1, "the built-ins still draw");
    let e = d.error.as_ref().expect("and the failure is on screen");
    assert_eq!(e.source, crate::mode_detail::DetailFetch::Metrics);
    assert_eq!(e.message, "custom metrics: AccessDenied");
}

/// A hand edit to the config file reaches the App tab on its next
/// refresh, without a restart; an unreadable file keeps what was read
/// and is tried again; a deleted one is no config; and an App with no
/// config file (a test App) never reads one.
#[tokio::test]
async fn app_lines_are_re_read_when_the_file_changed() {
    let mut app = test_app();
    assert!(!app.reload_config_if_changed(), "no config file");
    assert!(app.cfg.app_tabs.envs.is_empty());

    let path = own_config_file(&mut app, "re-read", "app.e.namespace = \"First\"\n");
    let namespace = |app: &App| {
        app.cfg
            .app_tabs
            .envs
            .get("e")
            .and_then(|s| s.namespace.clone())
    };
    assert!(app.reload_config_if_changed(), "first read");
    assert_eq!(namespace(&app).as_deref(), Some("First"));
    assert!(!app.reload_config_if_changed(), "unchanged");

    // A different size: an edit even within the same mtime tick.
    std::fs::write(&path, "app.e.namespace = \"Edited\"\n").unwrap();
    assert!(app.reload_config_if_changed(), "changed");
    assert_eq!(namespace(&app).as_deref(), Some("Edited"));
    assert_eq!(app.config_seen, crate::config::stamp_of(&path));

    // Unreadable (a directory where the file was): kept, and the stamp
    // not recorded, so it is tried again.
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let seen = app.config_seen;
    assert!(!app.reload_config_if_changed());
    assert_eq!(namespace(&app).as_deref(), Some("Edited"));
    assert_eq!(app.config_seen, seen, "a failed read records nothing");

    // Deleted: nothing configured.
    std::fs::remove_dir(&path).unwrap();
    assert!(app.reload_config_if_changed());
    assert!(app.cfg.app_tabs.envs.is_empty());

    // An edit to the env on screen drops its rows fetched under the old
    // config (`set_app_tabs`), rather than leaving them beside the new.
    let mut app = detail_on("e");
    let path = own_config_file(&mut app, "re-read-detail", "app.e.namespace = \"First\"\n");
    app.reload_config_if_changed();
    let d = app.detail.as_mut().unwrap();
    let t = d.app_tab.metrics.begin();
    d.app_tab.metrics.settle_ticket(t, Ok(Default::default()));
    std::fs::write(&path, "app.e.namespace = \"Edited\"\n").unwrap();
    assert!(app.reload_config_if_changed());
    assert!(app
        .detail
        .as_ref()
        .unwrap()
        .app_tab
        .metrics
        .ready()
        .is_none());
}

/// This test's own `config.toml`, under the per-process test temp dir
/// and named for the test, so the real reload and save paths run
/// without the shared test config dir. Returns its path; the App has
/// seen no file yet, so the first use reads it.
fn own_config_file(app: &mut App, test: &str, text: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ebman-test-{}-{test}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("config.toml");
    std::fs::write(&path, text).expect("write");
    app.config_file = Some(path.clone());
    app.config_seen = None;
    path
}

/// Hand edits reach the App tab on its refresh, but not on one the
/// in-flight guard skips (the reload would reset that fetch and nothing
/// would replace it); and `:alarms` and `:why`, which use
/// `alarm_prefix`, read them too.
#[tokio::test]
async fn the_app_tab_refresh_re_reads_its_config() {
    let mut app = detail_on("api-prod");
    let path = own_config_file(
        &mut app,
        "refresh-re-reads",
        "app.api-prod.namespace = \"Hand\"\n",
    );
    let namespace = |app: &App| {
        app.cfg
            .app_tabs
            .envs
            .get("api-prod")
            .and_then(|s| s.namespace.clone())
    };
    app.detail.as_mut().unwrap().tab_idx = 1;
    assert_eq!(app.detail.as_ref().unwrap().tab(), DetailTab::App);
    let _ = app.detail.as_mut().unwrap().app_tab.metrics.begin();
    app.detail_refresh_active_tab();
    assert_eq!(namespace(&app), None, "not under a fetch in flight");

    app.detail.as_mut().unwrap().app_tab.metrics.reset();
    app.detail_refresh_active_tab();
    assert_eq!(namespace(&app).as_deref(), Some("Hand"));

    // Each of the other readers re-reads, and the App tab on screen,
    // emptied by the edit, is fetched again rather than left blank.
    let settle = |app: &mut App| {
        let m = &mut app.detail.as_mut().unwrap().app_tab.metrics;
        let t = m.begin();
        m.settle_ticket(t, Ok(Default::default()));
    };
    let refetched = |app: &App| app.detail.as_ref().unwrap().app_tab.metrics.in_flight();

    settle(&mut app);
    std::fs::write(&path, "app.api-prod.namespace = \"Alarms\"\n").unwrap();
    app.spawn_alarms_fetch(&mk_env("api-prod", "poly", "Web", "Green"));
    assert_eq!(namespace(&app).as_deref(), Some("Alarms"), ":alarms");
    assert!(refetched(&app), ":alarms refetched the tab");

    settle(&mut app);
    std::fs::write(&path, "app.api-prod.namespace = \"Why\"\n").unwrap();
    let env = app.environments[0].clone();
    app.open_why_red(&env);
    assert_eq!(namespace(&app).as_deref(), Some("Why"), ":why");
    assert!(refetched(&app), ":why refetched the tab");

    app.current_overlay = None;
    app.mode = Mode::Detail;
    settle(&mut app);
    app.demo_mode = true;
    std::fs::write(&path, "app.api-prod.namespace = \"Editor\"\n").unwrap();
    press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
    assert!(app.form.is_some(), "the editor");
    assert_eq!(namespace(&app).as_deref(), Some("Editor"), "e");
    assert!(refetched(&app), "e refetched the tab");
}

/// `e` opens the App tab's editor on the App tab and nowhere else; Esc
/// returns to Detail; discovery for another env is ignored, and failed
/// lookups are named in the banner; a bad entry is not saved.
#[tokio::test]
async fn the_app_tab_editor_opens_fills_and_refuses_bad_entries() {
    use crate::app_discover::Discovered;
    let mut app = test_app();
    app.demo_mode = true;
    app.environments = vec![mk_env("api-prod", "shop", "WebServer", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
    assert!(app.form.is_none(), "not on the Health tab");
    app.detail.as_mut().unwrap().tab_idx = 1;
    assert_eq!(app.detail.as_ref().unwrap().tab(), DetailTab::App);
    press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
    assert!(app.form.is_none(), "^E is the events panel, not the editor");
    press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
    assert!(matches!(app.mode, Mode::Form));
    assert!(matches!(
        app.form.as_ref().map(|f| &f.submit),
        Some(crate::form::FormSubmit::AppTab)
    ));

    // Another env's discovery is not this form's.
    let mut found = Discovered::default();
    found.discovery.alarm_prefix = Some(("api-prod-".into(), 2));
    found.failed = vec![("custom metrics".into(), "AccessDenied".into())];
    app.handle_msg(AppMsg::AppTabDiscovered {
        gen: app.generation,
        key: test_key(&app, "other"),
        ticket: discovery_ticket(&app, "other"),
        found: Box::new(found.clone()),
    });
    assert_eq!(
        app.form.as_ref().unwrap().state,
        crate::form::FormState::Loading
    );
    app.handle_msg(AppMsg::AppTabDiscovered {
        gen: app.generation,
        key: test_key(&app, "api-prod"),
        ticket: discovery_ticket(&app, "api-prod"),
        found: Box::new(found),
    });
    let form = app.form.as_ref().unwrap();
    assert_eq!(form.state, crate::form::FormState::Ready);
    assert!(
        form.banner.contains("custom metrics: AccessDenied"),
        "{}",
        form.banner
    );
    let prefix = form
        .fields
        .iter()
        .find(|f| f.key == "alarm_prefix")
        .unwrap();
    assert_eq!(prefix.value, "api-prod-");

    // A bad entry: refused, said why, nothing saved, the form stays.
    app.form
        .as_mut()
        .unwrap()
        .fields
        .iter_mut()
        .find(|f| f.key == "metrics")
        .unwrap()
        .value = "u=users|Avg".into();
    press(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
    let form = app.form.as_ref().expect("still open");
    assert!(form.banner.starts_with("not saved:"), "{}", form.banner);
    assert!(
        form.banner.contains("not a CloudWatch statistic"),
        "{}",
        form.banner
    );

    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.form.is_none());
    assert!(
        matches!(app.mode, Mode::Detail),
        "back to Detail, not the table"
    );
}

/// The editor opens on the file as it is now (a hand edit since the
/// last refresh is in the form, or the save would delete it); a save
/// writes this env's lines, keeps every other line and the env's lines
/// that did not apply, applies them, and records the file as seen; its
/// refresh is not skipped for a fetch in flight under the old config;
/// and a failed write applies nothing.
#[tokio::test]
async fn saving_the_app_tab_writes_then_applies() {
    let mut app = detail_on("api-prod");
    app.demo_mode = true;
    let path = own_config_file(
        &mut app,
        "saving-writes",
        "# mine\napp.other.namespace = \"Other\"\napp.api-prod.namespace = \"Old\"\n",
    );
    app.detail.as_mut().unwrap().tab_idx = 1;
    app.detail_refresh_active_tab();
    assert_eq!(
        app.cfg.app_tabs.envs["api-prod"].namespace.as_deref(),
        Some("Old")
    );
    // A hand edit since, with a line the parser rejects.
    std::fs::write(
        &path,
        "# mine\napp.other.namespace = \"Other\"\napp.api-prod.namespace = \"Hand\"\n\
         app.api-prod.bogus = \"x\"\n",
    )
    .unwrap();
    press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
    while let Ok(msg) = app.msg_rx.try_recv() {
        app.handle_msg(msg);
    }
    let form = app.form.as_mut().expect("the editor");
    assert_eq!(form.state, crate::form::FormState::Ready);
    let ns = form
        .fields
        .iter_mut()
        .find(|f| f.key == "namespace")
        .expect("namespace");
    assert_eq!(ns.value, "Hand", "opened on the file as it is now");
    // Longer than what it replaces: the stamp is (mtime, size), and
    // an edit within the same tick is told apart by its size.
    ns.value = "Saved-and-longer".into();

    // A fetch in flight under the old config, which the guard honours.
    let _ = app.detail.as_mut().unwrap().app_tab.metrics.begin();
    app.submit_app_tab_form();

    assert!(app.form.is_none(), "saved and closed");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.contains("app.api-prod.namespace = \"Saved-and-longer\""),
        "{text}"
    );
    assert!(!text.contains("\"Hand\""), "{text}");
    assert!(
        text.contains("# mine") && text.contains("app.other.namespace = \"Other\""),
        "{text}"
    );
    assert!(
        text.contains("app.api-prod.bogus = \"x\""),
        "rejected line kept: {text}"
    );
    assert_eq!(
        app.cfg.app_tabs.envs["api-prod"].namespace.as_deref(),
        Some("Saved-and-longer"),
        "applied"
    );
    assert_eq!(
        app.config_seen,
        crate::config::stamp_of(&path),
        "the file as written is the one seen"
    );
    let mut refetched = false;
    while let Ok(msg) = app.msg_rx.try_recv() {
        refetched |= matches!(msg, AppMsg::DetailAppMetrics { .. });
    }
    assert!(refetched, "the save's refresh ran, past the old fetch");

    // A write that fails applies nothing: under a regular file, no
    // directory can be made and nothing written.
    app.config_file = Some(path.join("config.toml"));
    press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
    while let Ok(msg) = app.msg_rx.try_recv() {
        app.handle_msg(msg);
    }
    let form = app.form.as_mut().expect("the editor");
    form.fields
        .iter_mut()
        .find(|f| f.key == "namespace")
        .unwrap()
        .value = "Unwritten".into();
    app.submit_app_tab_form();
    let form = app.form.as_ref().expect("still open");
    assert!(form.banner.starts_with("not saved:"), "{}", form.banner);
    assert_eq!(
        app.cfg.app_tabs.envs["api-prod"].namespace.as_deref(),
        Some("Saved-and-longer"),
        "nothing applied"
    );
}

/// An unconfigured env's discovery is kept for the session: opening the
/// tab again reuses it, `^R` looks again (resources come and go), and a
/// discovery whose answer was lost does not block the env for good.
#[tokio::test]
async fn discovery_is_reused_on_open_rerun_on_ctrl_r_and_never_wedged() {
    use crate::app_discover::Discovered;
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "shop", "WebServer", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    let d = app.detail.as_mut().unwrap();
    d.tab_idx = d.tabs.iter().position(|t| *t == DetailTab::App).unwrap();
    app.detail_refresh_on_open();
    assert!(app.discovery_running(&test_key(&app, "api-prod")));
    let settle = |app: &mut App| {
        app.detail
            .as_mut()
            .unwrap()
            .app_tab
            .metrics
            .settle_now(Ok(Default::default()));
    };

    app.caches.app_tab_discovery.discovering.clear();
    let key = test_key(&app, "api-prod");
    app.caches
        .app_tab_discovery
        .discovered
        .insert(key, Discovered::default());
    settle(&mut app);
    app.detail_refresh_on_open();
    assert!(
        !app.discovery_running(&test_key(&app, "api-prod")),
        "opening reuses what was found"
    );
    settle(&mut app);
    app.detail_refresh_active_tab();
    assert!(
        app.discovery_running(&test_key(&app, "api-prod")),
        "^R looks again"
    );

    // A run whose answer never came (dropped by a context switch, a task
    // that died) stops counting as running, so it is started again.
    let long_ago = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(120))
        .expect("clock past two minutes");
    let key = test_key(&app, "api-prod");
    let run = app
        .caches
        .app_tab_discovery
        .discovering
        .entry(key)
        .or_default();
    let _ = run.begin();
    run.backdate(long_ago);
    assert!(
        !app.discovery_running(&test_key(&app, "api-prod")),
        "lost, not running"
    );
    settle(&mut app);
    app.detail_refresh_active_tab();
    assert!(
        app.discovery_running(&test_key(&app, "api-prod")),
        "and started again"
    );
}

/// A preview result that lands after the env got config is not drawn as
/// that config's: it is dropped, and the config's own fetch is drawn.
#[tokio::test]
async fn a_preview_result_landing_after_a_save_is_dropped() {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "shop", "WebServer", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    let marked = |note: &str| crate::app_tab::AppMetrics {
        notes: vec![note.into()],
        ..Default::default()
    };
    // A preview's fetch, then the config is saved under it.
    let preview = app.detail.as_mut().unwrap().app_tab.metrics.begin();
    app.set_app_tabs(crate::config::parse("app.api-prod.rds = \"db\"\n").app_tabs);
    app.handle_msg(AppMsg::DetailAppMetrics {
        gen: app.generation,
        ticket: preview,
        result: Ok(marked("preview")),
    });
    let m = &app.detail.as_ref().unwrap().app_tab.metrics;
    assert!(m.ready().is_none(), "dropped");
    assert!(
        !m.in_flight(),
        "and no longer loading, or refreshes are refused"
    );
    let config = app.detail.as_mut().unwrap().app_tab.metrics.begin();
    app.handle_msg(AppMsg::DetailAppMetrics {
        gen: app.generation,
        ticket: config,
        result: Ok(marked("config")),
    });
    assert_eq!(
        app.detail
            .as_ref()
            .unwrap()
            .app_tab
            .metrics
            .ready()
            .map(|m| m.notes[0].as_str()),
        Some("config")
    );
}

/// Demo mode looked at nothing, so it caches nothing: the tab keeps
/// saying what to configure rather than "nothing found".
#[tokio::test]
async fn demo_mode_does_not_pass_off_an_empty_discovery() {
    let mut app = test_app();
    app.demo_mode = true;
    app.spawn_app_discovery(test_key(&app, "api-prod"));
    let msg = app.msg_rx.try_recv().expect("answered at once");
    app.handle_msg(msg);
    assert!(app.caches.app_tab_discovery.discovered.is_empty());
    assert!(!app.discovery_running(&test_key(&app, "api-prod")));
}

/// An older discovery that answers after a newer one started does not
/// mark the newer one finished.
#[tokio::test]
async fn only_the_latest_discoverys_answer_clears_its_marker() {
    let mut app = test_app();
    let key = test_key(&app, "api-prod");
    let run = app
        .caches
        .app_tab_discovery
        .discovering
        .entry(key)
        .or_default();
    let older = run.begin();
    let newer = run.begin();
    let answer = |ticket| AppMsg::AppTabDiscovered {
        gen: 0,
        key: EnvKey::new("us-east-1", "api-prod"),
        ticket,
        found: Box::default(),
    };
    app.handle_msg(answer(older));
    assert!(
        app.discovery_running(&test_key(&app, "api-prod")),
        "still running"
    );
    app.handle_msg(answer(newer));
    assert!(!app.discovery_running(&test_key(&app, "api-prod")));
}

/// A ticket settles only the latest request's answer; `reset` drops
/// what is shown and any answer still coming; and tickets are unique
/// across `Fetch`es, so a Detail view reopened on the same env does not
/// take the old view's late answer as its own.
#[test]
fn only_the_latest_tickets_answer_settles_a_fetch() {
    let mut f: crate::app::Fetch<u32> = Default::default();
    let first = f.begin();
    let second = f.begin();
    assert!(!f.settle_ticket(first, Ok(1)), "superseded");
    assert!(
        f.ready().is_none() && f.in_flight(),
        "the latest still awaited"
    );
    assert!(f.settle_ticket(second, Ok(2)));
    assert_eq!(f.ready(), Some(&2));

    let third = f.begin();
    f.reset();
    assert!(
        f.ready().is_none() && !f.in_flight(),
        "nothing shown, nothing awaited"
    );
    assert!(
        !f.settle_ticket(third, Ok(3)),
        "an answer from before the reset"
    );
    assert!(f.ready().is_none());

    let mut reopened: crate::app::Fetch<u32> = Default::default();
    assert!(
        !reopened.settle_ticket(second, Ok(8)),
        "a fresh Fetch takes no ticket"
    );
    assert!(reopened.ready().is_none());
    let fresh = reopened.begin();
    assert!(
        !reopened.settle_ticket(second, Ok(9)),
        "another Fetch's ticket"
    );
    assert!(reopened.settle_ticket(fresh, Ok(4)));
}

/// A config change forgets what the Detail view fetched under the old
/// config, and only that: the env's rows when its spec changed, top
/// errors only when what it queries changed (it is billed per scan), and
/// nothing for another env's edit.
#[tokio::test]
async fn a_config_change_forgets_only_what_it_invalidates() {
    let mut app = detail_on("api-prod");
    let tabs = |text: &str| crate::config::parse(text).app_tabs;
    let base = "app.api-prod.namespace = \"Shop\"\napp.api-prod.log_group = \"var/log/app.log\"\n";
    app.set_app_tabs(tabs(base));
    let settle = |app: &mut App| {
        let d = app.detail.as_mut().unwrap();
        let t = d.app_tab.metrics.begin();
        d.app_tab.metrics.settle_ticket(t, Ok(Default::default()));
        let t = d.cw_alarms.begin();
        d.cw_alarms.settle_ticket(t, Ok(Vec::new()));
        let t = d.app_tab.top_errors.begin();
        d.app_tab.top_errors.settle_ticket(
            t,
            Ok(crate::app_tab::TopErrors {
                log_group: "g".into(),
                rows: vec![],
                more: (0, 0),
                truncated: false,
                unmatched: 0,
                records_scanned: 0,
                fetched_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            }),
        );
    };
    // (series, alarms, top errors) still shown after the edit.
    let kept = |app: &App| {
        let d = app.detail.as_ref().unwrap();
        (
            d.app_tab.metrics.ready().is_some(),
            d.cw_alarms.ready().is_some(),
            d.app_tab.top_errors.ready().is_some(),
        )
    };
    // One case per field, each from the same base.
    for (edit, want, why) in [
        (
            "app.other.namespace = \"X\"\n",
            (true, true, true),
            "another env's edit",
        ),
        (
            "app.api-prod.threshold.x = \"1,2\"\n",
            (false, true, true),
            "a threshold: series only",
        ),
        (
            "app.api-prod.alarm_prefix = \"Shop-\"\n",
            (true, false, true),
            "the prefix: alarms only",
        ),
        (
            "app.api-prod.error_fingerprint = \"x(?<fp>y)\"\n",
            (true, true, false),
            "the fingerprint: the query only",
        ),
    ] {
        app.set_app_tabs(tabs(base));
        settle(&mut app);
        app.set_app_tabs(tabs(&format!("{base}{edit}")));
        assert_eq!(kept(&app), want, "{why}");
    }
    app.set_app_tabs(tabs(base));
    settle(&mut app);
    app.set_app_tabs(tabs("app.api-prod.namespace = \"Shop\"\n"));
    assert_eq!(
        kept(&app),
        (true, true, false),
        "log_group removed: its rows go"
    );
    settle(&mut app);
    app.set_app_tabs(Default::default());
    assert_eq!(
        kept(&app),
        (false, true, false),
        "config removed: no series to keep"
    );
}

/// After a reload outside a refresh empties the App tab on screen, its
/// series and alarms are fetched again, each only if it was emptied, and
/// not at all off the tab; an alarms answer matched under the old prefix
/// does not settle the panel.
#[tokio::test]
async fn an_emptied_app_tab_on_screen_is_fetched_again() {
    let fetched = |app: &mut App| {
        let (mut series, mut alarms) = (false, false);
        while let Ok(msg) = app.msg_rx.try_recv() {
            series |= matches!(msg, AppMsg::DetailAppMetrics { .. });
            alarms |= matches!(msg, AppMsg::DetailAlarms { .. });
        }
        (series, alarms)
    };
    let mut app = detail_on("api-prod");
    app.demo_mode = true;
    app.detail.as_mut().unwrap().tab_idx = 1;
    assert_eq!(app.detail.as_ref().unwrap().tab(), DetailTab::App);
    // As a config edit to both leaves them.
    fetched(&mut app);
    let d = app.detail.as_mut().unwrap();
    d.app_tab.metrics.reset();
    d.cw_alarms.reset();
    app.refetch_what_an_edit_emptied();
    assert_eq!(fetched(&mut app), (true, true), "both blank: both fetched");
    assert!(
        app.detail.as_ref().unwrap().tab_loading_since().is_some(),
        "and guarded as a refresh is"
    );

    // Series shown, alarms reset by a prefix edit: alarms alone.
    let base = "app.api-prod.namespace = \"Shop\"\n";
    app.set_app_tabs(crate::config::parse(base).app_tabs);
    fetched(&mut app);
    let d = app.detail.as_mut().unwrap();
    let t = d.app_tab.metrics.begin();
    d.app_tab.metrics.settle_ticket(t, Ok(Default::default()));
    let old = d.cw_alarms.begin();
    app.set_app_tabs(
        crate::config::parse(&format!("{base}app.api-prod.alarm_prefix = \"x-\"\n")).app_tabs,
    );
    app.handle_msg(AppMsg::DetailAlarms {
        gen: app.generation,
        ticket: old,
        result: Ok(Vec::new()),
    });
    assert!(
        app.detail.as_ref().unwrap().cw_alarms.ready().is_none(),
        "matched under the old prefix: dropped"
    );
    app.refetch_what_an_edit_emptied();
    assert_eq!(fetched(&mut app), (false, true), "rows kept: not refetched");

    // The Health tab shows the alarms too: a blank alarm list there is
    // refetched (it would read as no alarms), the series are not.
    let mut app = detail_on("api-prod");
    app.demo_mode = true;
    assert_eq!(app.detail.as_ref().unwrap().tab(), DetailTab::Health);
    fetched(&mut app);
    let d = app.detail.as_mut().unwrap();
    d.app_tab.metrics.reset();
    d.cw_alarms.reset();
    app.refetch_what_an_edit_emptied();
    assert_eq!(fetched(&mut app), (false, true), "Health: alarms only");

    // A tab that shows neither: nothing. Demo mode, so a fetch would be
    // answered at once and seen below.
    let mut app = detail_on("api-prod");
    app.demo_mode = true;
    app.detail.as_mut().unwrap().tab_idx = 2;
    assert!(!matches!(
        app.detail.as_ref().unwrap().tab(),
        DetailTab::App | DetailTab::Health
    ));
    fetched(&mut app);
    let d = app.detail.as_mut().unwrap();
    d.app_tab.metrics.reset();
    d.cw_alarms.reset();
    app.refetch_what_an_edit_emptied();
    assert_eq!(fetched(&mut app), (false, false), "not on screen");
}

/// A save that changes only the alarm prefix, made while a series fetch
/// is in flight (so the save's refresh is skipped by the guard), still
/// refetches the alarms it reset: otherwise firing alarms vanish from the
/// tab until a refresh that passes the guard.
#[tokio::test]
async fn a_prefix_only_save_under_a_fetch_in_flight_refetches_the_alarms() {
    let mut app = detail_on("api-prod");
    app.demo_mode = true;
    let base = "app.api-prod.namespace = \"Shop\"\n";
    let path = own_config_file(&mut app, "prefix-only-save", base);
    app.detail.as_mut().unwrap().tab_idx = 1;
    app.reload_config_if_changed();
    press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
    while let Ok(msg) = app.msg_rx.try_recv() {
        app.handle_msg(msg);
    }
    let form = app.form.as_mut().expect("the editor");
    form.fields
        .iter_mut()
        .find(|f| f.key == "alarm_prefix")
        .expect("alarm_prefix")
        .value = "Shop-".into();
    // Settled alarms, and a series fetch in flight under the guard.
    let d = app.detail.as_mut().unwrap();
    let t = d.cw_alarms.begin();
    d.cw_alarms.settle_ticket(t, Ok(Vec::new()));
    let _ = d.app_tab.metrics.begin();
    app.submit_app_tab_form();
    assert!(app.form.is_none(), "saved");
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("alarm_prefix"));
    assert!(
        app.detail.as_ref().unwrap().cw_alarms.in_flight(),
        "the reset alarms are being fetched again"
    );
}

/// The App's `app.` config is changed only through `set_app_tabs`,
/// which drops what was fetched under the old config: any other write
/// (an assignment, an insert or remove on its envs, a mutable borrow, a
/// whole new `cfg`) would leave the old rows beside the new config, and
/// an answer in flight for the old landing as the new one's.
#[test]
fn app_config_is_replaced_only_through_set_app_tabs() {
    const WRITES: &[&str] = &[
        "app_tabs =",
        "&mut self.cfg.app_tabs",
        "&mut app.cfg.app_tabs",
        "app_tabs.envs.insert(",
        "app_tabs.envs.remove(",
        "app_tabs.envs.clear(",
        "app_tabs.envs.get_mut(",
        "app_tabs.envs.entry(",
        "self.cfg = ",
        "app.cfg = ",
    ];
    let writes = super::scan::production_statements_where(|code| {
        code.contains("cfg") && WRITES.iter().any(|w| code.contains(w))
    });
    // config.rs builds a `Config` (its parser, and the save that keeps
    // the file's app. lines); no App passes through it.
    let by_app: Vec<&String> = writes
        .iter()
        .filter(|w| !w.starts_with("src/config.rs:"))
        .collect();
    assert_eq!(by_app.len(), 1, "{by_app:#?}: use set_app_tabs");
    assert!(
        by_app[0].starts_with("src/app/detail_nav.rs:")
            && by_app[0].contains("std::mem::replace(&mut self.cfg.app_tabs"),
        "{by_app:#?}: only set_app_tabs's own swap"
    );
}

/// Config wins; without it, what discovery has evidence for is a
/// preview; without that, not set up. And nothing outside
/// `app_tab_source` asks the question its own way.
#[tokio::test]
async fn an_envs_app_tab_comes_from_config_then_a_preview_then_nothing() {
    use crate::app::AppTabSource;
    let mut app = detail_on("api-prod");
    assert_eq!(
        app.app_tab_source(&test_key(&app, "api-prod")),
        AppTabSource::NotSetUp
    );

    let key = test_key(&app, "api-prod");
    let mut found = crate::app_discover::Discovered::default();
    found.discovery.app_log = Some("var/log/app.log".into());
    app.caches
        .app_tab_discovery
        .discovered
        .insert(key.clone(), found.clone());
    assert_eq!(
        app.app_tab_source(&test_key(&app, "api-prod")),
        AppTabSource::NotSetUp,
        "found, but nothing an env variable names"
    );
    found.discovery.rds_found = vec![("db-a".into(), "DB_HOST".into())];
    app.caches.app_tab_discovery.discovered.insert(key, found);
    let preview = app.app_tab_source(&test_key(&app, "api-prod"));
    assert!(!preview.is_configured());
    assert_eq!(
        preview.spec().map(|s| s.rds),
        Some(vec!["db-a".to_string()])
    );

    app.set_app_tabs(crate::config::parse("app.api-prod.alarm_prefix = \"x-\"\n").app_tabs);
    let configured = app.app_tab_source(&test_key(&app, "api-prod"));
    assert!(configured.is_configured(), "config wins, even with no rows");
    assert_eq!(configured.spec().map(|s| s.rds), Some(vec![]));

    // However the config is reached (`tabs.envs`, `app.cfg.app_tabs.envs`).
    // Also the branching shapes: `match … .envs.get(..)`, `if let Some(_)
    // = … .envs.get(..)`, `matches!(… .envs.get(..), …)`. The one such
    // statement allowed is app_tab_source's own match.
    let asked = super::scan::production_statements_where(|code| {
        code.contains(".envs.contains_key(")
            || (code.contains(".envs.get(")
                && (code.contains(".is_some()")
                    || code.contains(".is_none()")
                    || code.contains("match ")
                    || code.contains("if let Some(")
                    || code.contains("matches!(")))
    });
    assert_eq!(asked.len(), 1, "{asked:#?}: ask app_tab_source");
    assert!(
        asked[0]
            .starts_with("src/app/app_tab_form.rs: match self.cfg.app_tabs.envs.get(&env.name)"),
        "{asked:#?}"
    );
    // A preview is made from what discovery found only by AppTabSource,
    // however it is imported: every use but the definition is there.
    let previewed = super::scan::production_statements_where(|code| code.contains("preview_spec"));
    let uses: Vec<&String> = previewed
        .iter()
        .filter(|l| !l.contains("fn preview_spec("))
        .collect();
    assert_eq!(
        previewed.len(),
        uses.len() + 1,
        "{previewed:#?}: its definition"
    );
    assert!(
        !uses.is_empty()
            && uses
                .iter()
                .all(|l| l.starts_with("src/app/app_tab_form.rs:")),
        "{uses:#?}: ask app_tab_source"
    );
}

/// With no Detail view there is no Fetch to take the answer, so no
/// fetch is started and nothing is sent.
#[tokio::test]
async fn the_app_tab_fetch_needs_a_detail_view() {
    let mut app = test_app();
    app.demo_mode = true;
    app.spawn_detail_app("api-prod".into());
    assert!(app.msg_rx.try_recv().is_err());
}

/// `:settings` saves to this App's config file, not to a path of its
/// own; and an App with none refuses rather than writing somewhere.
#[tokio::test]
async fn settings_save_to_the_apps_config_file() {
    let mut app = test_app();
    let path = own_config_file(&mut app, "settings-save", "app.e.namespace = \"Kept\"\n");
    app.open_settings_form();
    press(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
    let text = std::fs::read_to_string(&path).unwrap();
    // Rewritten by the save (the settings beside the line), not left as
    // the one line written above, and the file's app. line kept.
    assert_ne!(text, "app.e.namespace = \"Kept\"\n", "written there");
    assert!(text.contains("app.e.namespace = \"Kept\""), "{text}");
    assert!(app.error_message.is_none(), "{:?}", app.error_message);

    let mut app = test_app();
    app.open_settings_form();
    press(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert!(
        app.error_message
            .as_deref()
            .is_some_and(|e| e.contains("settings not saved")),
        "{:?}",
        app.error_message
    );
}

/// A hand edit to the alarm config reaches the fleet view on the next
/// refresh, without opening an env: the refresh re-reads it before its
/// polls are matched. Both halves: the prefix and the dimensions.
#[tokio::test]
async fn a_fleet_refresh_re_reads_the_alarm_config() {
    let mut app = test_app();
    let path = own_config_file(&mut app, "fleet-reload", "");
    app.reload_config_if_changed();
    std::fs::write(
        &path,
        "app.shop-prod.alarm_prefix = \"shop-prod-\"\n\
         alarm_dimensions = \"Environment,-EnvironmentName\"\n",
    )
    .unwrap();
    app.apply_refresh(
        app.fanout_epoch,
        Ok(vec![mk_env("shop-prod", "poly", "Web", "Green")]),
        Vec::new(),
    );
    let alarm = |name: &str, dim: &str| crate::aws::FiringAlarm {
        name: name.into(),
        dimensions: vec![(dim.into(), "shop-prod".into())],
    };
    app.handle_msg(AppMsg::FiringAlarms {
        gen: app.generation,
        round: app.caches.poll_round,
        results: vec![(
            "us-east-1".into(),
            Ok(vec![
                alarm("shop-prod-db", "DBInstanceIdentifier"),
                alarm("by-own-spelling", "Environment"),
                alarm("by-default-spelling", "EnvironmentName"),
            ]),
        )],
    });
    assert_eq!(
        app.caches.firing_alarms.get(&home_key("shop-prod")),
        Some(&vec![
            "shop-prod-db".to_string(),
            "by-own-spelling".to_string()
        ]),
        "matched by the edited prefix and the edited dimensions"
    );
}

/// An edit to `alarm_dimensions` changes which alarms are an env's, so
/// the alarms the Detail view fetched under the old ones are dropped.
#[tokio::test]
async fn an_alarm_dimensions_edit_drops_the_alarms_fetched_under_the_old_ones() {
    let mut app = detail_on("api-prod");
    let path = own_config_file(&mut app, "dims-reload", "");
    app.reload_config_if_changed();
    let d = app.detail.as_mut().unwrap();
    let t = d.cw_alarms.begin();
    d.cw_alarms.settle_ticket(t, Ok(Vec::new()));
    std::fs::write(&path, "alarm_dimensions = \"Environment\"\n").unwrap();
    assert!(app.reload_config_if_changed());
    assert_eq!(
        app.cfg.alarm_dimensions,
        vec!["EnvironmentName".to_string(), "Environment".to_string()]
    );
    assert!(app.detail.as_ref().unwrap().cw_alarms.ready().is_none());
}

/// A model test of the Detail view's request lifecycle: random tab
/// switches, range changes, `^R`, auto-refresh ticks and config edits,
/// with every answer held and delivered in a random order, the way slow
/// and fast calls interleave. Eleven fixes this cycle were in this
/// class, each pinned by one hand-written ordering.
///
/// Checked at every delivery: an answer that is not the latest
/// request's changes nothing it reaches (the latest one settles, and
/// one that leaves a fetch in flight leaves it unchanged). And at the
/// end, once every answer has landed, no fetch is still loading.
mod lifecycle_model {
    use super::*;
    use proptest::prelude::*;

    #[derive(Debug, Clone)]
    enum Op {
        Tab(DetailTab),
        Range(bool),
        Refresh,
        Tick,
        Edit(u8),
        Deliver(usize),
        Collect,
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            prop_oneof![
                Just(DetailTab::Metrics),
                Just(DetailTab::App),
                Just(DetailTab::Health),
            ]
            .prop_map(Op::Tab),
            any::<bool>().prop_map(Op::Range),
            Just(Op::Refresh),
            Just(Op::Tick),
            (0u8..4).prop_map(Op::Edit),
            (0usize..8).prop_map(Op::Deliver),
            Just(Op::Collect),
        ]
    }

    /// The App tab's config, in four versions that differ in what each
    /// fetch reads: the series, the alarm prefix, the queried log group.
    fn config(n: u8) -> crate::app_tab::AppTabs {
        let extra = match n {
            0 => "",
            1 => "app.api-prod.alarm_prefix = \"api-\"\n",
            2 => "app.api-prod.metric.heap = \"heap|Maximum\"\n",
            _ => "app.api-prod.log_group = \"var/log/app.log\"\n",
        };
        crate::config::parse(&format!(
            "app.api-prod.namespace = \"N\"\napp.api-prod.metric.users = \"users|Sum\"\n{extra}"
        ))
        .app_tabs
    }

    /// Which fetch an answer is for, its ticket, and a snapshot of
    /// everything that answer may touch.
    fn reach(app: &App, msg: &AppMsg) -> Option<(&'static str, String, bool)> {
        let d = app.detail.as_ref()?;
        Some(match msg {
            AppMsg::DetailMetrics { ticket, .. } => (
                "metrics",
                format!("{:?}{:?}{:?}", d.metrics_request, d.metrics, d.error),
                d.metrics_request.ticket_in_flight() == Some(*ticket),
            ),
            AppMsg::DetailAppMetrics { ticket, .. } => (
                "app",
                format!("{:?}", d.app_tab.metrics),
                d.app_tab.metrics.ticket_in_flight() == Some(*ticket),
            ),
            AppMsg::DetailAlarms { ticket, .. } => (
                "alarms",
                format!("{:?}", d.cw_alarms),
                d.cw_alarms.ticket_in_flight() == Some(*ticket),
            ),
            AppMsg::DetailTopErrors { ticket, .. } => (
                "top errors",
                format!("{:?}", d.app_tab.top_errors),
                d.app_tab.top_errors.ticket_in_flight() == Some(*ticket),
            ),
            AppMsg::DetailRecentVersions { ticket, .. } => (
                "versions",
                format!("{:?}", d.recent_versions),
                d.recent_versions.ticket_in_flight() == Some(*ticket),
            ),
            _ => return None,
        })
    }

    fn snapshot(app: &App, what: &str) -> String {
        let d = app.detail.as_ref().expect("detail open");
        match what {
            "metrics" => format!("{:?}{:?}{:?}", d.metrics_request, d.metrics, d.error),
            "app" => format!("{:?}", d.app_tab.metrics),
            "alarms" => format!("{:?}", d.cw_alarms),
            "top errors" => format!("{:?}", d.app_tab.top_errors),
            _ => format!("{:?}", d.recent_versions),
        }
    }

    fn in_flight(app: &App) -> Vec<&'static str> {
        let d = app.detail.as_ref().expect("detail open");
        [
            ("metrics", d.metrics_request.in_flight()),
            ("app", d.app_tab.metrics.in_flight()),
            ("alarms", d.cw_alarms.in_flight()),
            ("top errors", d.app_tab.top_errors.in_flight()),
            ("versions", d.recent_versions.in_flight()),
        ]
        .into_iter()
        .filter_map(|(n, f)| f.then_some(n))
        .collect()
    }

    /// Let the spawned fetches run (the stub client answers each at once,
    /// with an error) and hold what they send.
    async fn collect(app: &mut App, held: &mut Vec<AppMsg>) {
        for _ in 0..40 {
            tokio::task::yield_now().await;
            while let Ok(m) = app.msg_rx.try_recv() {
                held.push(m);
            }
        }
    }

    /// Deliver one held answer, checking it against the request it
    /// claims to answer.
    fn deliver(app: &mut App, msg: AppMsg) -> Result<(), TestCaseError> {
        let Some((what, before, latest)) = reach(app, &msg) else {
            app.handle_msg(msg);
            return Ok(());
        };
        app.handle_msg(msg);
        // The latest request's answer settles it, which the end-of-run
        // check pins. One for a superseded request must change nothing.
        if !latest {
            prop_assert_eq!(
                snapshot(app, what),
                before,
                "a superseded {} answer changed what it reached",
                what
            );
        }
        Ok(())
    }

    fn run(ops: Vec<Op>, order: Vec<usize>) -> Result<(), TestCaseError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async move {
            let mut app = detail_on("api-prod");
            // Every fetch goes through the home stub client, which fails
            // at identity resolution with no IO; a row in another region
            // would build a real client config (reading ~/.aws).
            assert!(
                app.detail.as_ref().unwrap().env_snapshot.region.is_none(),
                "the model's env must be in the home region"
            );
            app.set_app_tabs(config(0));
            let mut held: Vec<AppMsg> = Vec::new();
            collect(&mut app, &mut held).await;
            for op in ops {
                match op {
                    Op::Tab(tab) => {
                        focus_tab(&mut app, tab);
                        app.detail_refresh_on_open();
                    }
                    Op::Range(wider) => app.cycle_metrics_range(if wider { 1 } else { -1 }),
                    Op::Refresh => app.detail_refresh_active_tab(),
                    Op::Tick => app.detail_refresh_on_tick(),
                    Op::Edit(n) => {
                        app.set_app_tabs(config(n));
                        app.refetch_what_an_edit_emptied();
                    }
                    Op::Deliver(i) => {
                        if !held.is_empty() {
                            let msg = held.remove(i % held.len());
                            deliver(&mut app, msg)?;
                        }
                    }
                    Op::Collect => collect(&mut app, &mut held).await,
                }
            }
            // Everything still out lands, in a random order, and so does
            // whatever those answers start, until nothing more comes.
            let mut order = order.into_iter().cycle();
            for _ in 0..20 {
                collect(&mut app, &mut held).await;
                if held.is_empty() {
                    break;
                }
                while !held.is_empty() {
                    let i = order.next().unwrap_or(0) % held.len();
                    let msg = held.remove(i);
                    deliver(&mut app, msg)?;
                }
            }
            prop_assert!(held.is_empty(), "answers kept coming");
            prop_assert_eq!(
                in_flight(&app),
                Vec::<&str>::new(),
                "still loading once all answered"
            );
            Ok(())
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn every_request_settles_once_and_only_the_latest_lands(
            ops in proptest::collection::vec(op(), 1..30),
            order in proptest::collection::vec(0usize..16, 1..8),
        ) {
            run(ops, order)?;
        }
    }
}

/// An older discovery run's late answer neither replaces what the newer
/// run found nor fills the open form.
#[tokio::test]
async fn a_superseded_discovery_answer_changes_nothing() {
    let mut app = test_app();
    let key = test_key(&app, "api-prod");
    let run = app
        .caches
        .app_tab_discovery
        .discovering
        .entry(key.clone())
        .or_default();
    let older = run.begin();
    let newer = run.begin();
    let found = |prefix: &str| {
        let mut d = crate::app_discover::Discovered::default();
        d.discovery.alarm_prefix = Some((prefix.into(), 1));
        Box::new(d)
    };
    let answer = |ticket, prefix: &str| AppMsg::AppTabDiscovered {
        gen: 0,
        key: EnvKey::new("us-east-1", "api-prod"),
        ticket,
        found: found(prefix),
    };
    app.handle_msg(answer(newer, "new-"));
    app.handle_msg(answer(older, "old-"));
    let kept = app.caches.app_tab_discovery.discovered.get(&key);
    assert_eq!(
        kept.and_then(|d| d.discovery.alarm_prefix.as_ref())
            .map(|(p, _)| p.as_str()),
        Some("new-")
    );
}

/// A `:settings` save that changes `alarm_dimensions` drops the alarms
/// the Detail view fetched under the old ones, as a reload does: both go
/// through `set_alarm_dimensions`.
#[tokio::test]
async fn a_settings_change_to_alarm_dimensions_drops_the_old_alarms() {
    let mut app = detail_on("api-prod");
    app.detail
        .as_mut()
        .unwrap()
        .cw_alarms
        .settle_now(Ok(Vec::new()));
    let cfg = crate::config::Config {
        alarm_dimensions: vec!["EnvironmentName".into(), "Environment".into()],
        ..crate::config::Config::default()
    };
    app.apply_config_live(&cfg);
    assert_eq!(app.cfg.alarm_dimensions.len(), 2);
    assert!(app.detail.as_ref().unwrap().cw_alarms.ready().is_none());
}

/// Which of Detail's ticketed fetches a case drives.
#[derive(Clone, Copy, Debug)]
enum TicketedFetch {
    Events,
    Instances,
    Queues,
    Tags,
    EnvVars,
}

impl TicketedFetch {
    fn spawn(self, app: &mut App) {
        match self {
            Self::Events => app.spawn_detail_events("wk-prod".into()),
            Self::Instances => app.spawn_detail_instances("wk-prod".into()),
            Self::Queues => app.spawn_detail_queues("poly".into(), "wk-prod".into()),
            Self::Tags => app.spawn_detail_tags(),
            Self::EnvVars => app.spawn_detail_env_vars(),
        }
    }

    fn request(self, d: &mut crate::mode_detail::DetailState) -> &mut crate::app::Request {
        match self {
            Self::Events => &mut d.events_request,
            Self::Instances => &mut d.instances_request,
            Self::Queues => &mut d.queues_request,
            Self::Tags => &mut d.tags_request,
            Self::EnvVars => &mut d.env_vars_request,
        }
    }

    /// The ticket an answer from this fetch carries; `None` for another
    /// message (a sibling fetch the open spawned).
    fn ticket_of(self, msg: &AppMsg) -> Option<crate::app::Ticket> {
        match (self, msg) {
            (Self::Events, AppMsg::DetailEvents { ticket, .. })
            | (Self::Instances, AppMsg::DetailInstances { ticket, .. })
            | (Self::Queues, AppMsg::DetailQueues { ticket, .. })
            | (Self::Tags, AppMsg::DetailTags { ticket, .. })
            | (Self::EnvVars, AppMsg::DetailEnvVars { ticket, .. }) => Some(*ticket),
            _ => None,
        }
    }
}

/// Two `^R`s in a row, and the first answer lands last. A bare loading
/// flag let it overwrite the second and clear the flag while the second
/// was still out; a ticket drops it.
#[tokio::test]
async fn an_older_detail_answer_cannot_land_over_a_newer_request() {
    for kind in [
        TicketedFetch::Events,
        TicketedFetch::Instances,
        TicketedFetch::Queues,
        TicketedFetch::Tags,
        TicketedFetch::EnvVars,
    ] {
        let mut app = test_app();
        app.environments = vec![mk_env("wk-prod", "poly", "Worker", "Green")];
        app.rebuild_view();
        app.table_state.select(Some(0));
        app.open_detail();
        // An answer to a request the open made carries an older ticket
        // too, so it serves as well as the first spawn's below.
        kind.spawn(&mut app);
        kind.spawn(&mut app);
        let newer = kind
            .request(app.detail.as_mut().unwrap())
            .ticket_in_flight()
            .unwrap_or_else(|| panic!("{kind:?}: the second request is in flight"));
        let (mut older_msg, mut newer_msg) = (None, None);
        while older_msg.is_none() || newer_msg.is_none() {
            let msg = next_msg(&mut app).await;
            match kind.ticket_of(&msg) {
                Some(t) if t == newer => newer_msg = Some(msg),
                Some(_) => older_msg = Some(msg),
                None => {}
            }
        }

        app.handle_msg(older_msg.unwrap());
        let d = app.detail.as_mut().unwrap();
        assert!(
            kind.request(d).in_flight(),
            "{kind:?}: the older answer must not clear the newer request"
        );
        assert!(d.error.is_none(), "{kind:?}: nor show its result");

        app.handle_msg(newer_msg.unwrap());
        let d = app.detail.as_mut().unwrap();
        assert!(
            !kind.request(d).in_flight(),
            "{kind:?}: the newer one lands"
        );
        assert!(
            d.error.is_some(),
            "{kind:?}: with its (offline) failure on screen"
        );
    }
}

/// `^R` on the Logs tab mid-tail starts a second tail; the first one's
/// answer, landing last, must not replace the second's or end it.
#[tokio::test]
async fn an_older_log_tail_cannot_land_over_a_newer_one() {
    use crate::mode_detail::LogTailStage;
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "poly", "Web", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    app.spawn_detail_logs("api-prod".into());
    app.spawn_detail_logs("api-prod".into());
    let newer = app
        .detail
        .as_ref()
        .unwrap()
        .log_tail
        .request
        .ticket_in_flight()
        .expect("the second tail is in flight");
    let (mut older_msg, mut newer_msg) = (None, None);
    while older_msg.is_none() || newer_msg.is_none() {
        match next_msg(&mut app).await {
            msg @ AppMsg::DetailLogs { ticket, .. } if ticket == newer => newer_msg = Some(msg),
            msg @ AppMsg::DetailLogs { .. } => older_msg = Some(msg),
            _ => {}
        }
    }
    // A stale progress report must not move the stage either.
    app.handle_msg(AppMsg::DetailLogsProgress {
        gen: app.generation,
        ticket: crate::app::Request::stray_ticket(),
        stage: LogTailStage::Fetching,
        attempt: 3,
    });
    app.handle_msg(older_msg.unwrap());
    let tail = &app.detail.as_ref().unwrap().log_tail;
    assert_eq!(
        tail.stage,
        LogTailStage::Requesting,
        "still the newer tail's"
    );
    assert!(tail.error.is_none() && tail.request.in_flight());

    app.handle_msg(newer_msg.unwrap());
    let tail = &app.detail.as_ref().unwrap().log_tail;
    assert_eq!(tail.stage, LogTailStage::Ready);
    assert!(tail.error.is_some(), "the newer (offline) failure shows");
}
