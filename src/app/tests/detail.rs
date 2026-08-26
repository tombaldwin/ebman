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
    d.cw_alarms = Default::default();
    d.recent_versions = Default::default();
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
        ("alarms", |d| d.cw_alarms.begin(), &[DetailTab::Health]),
        (
            "recent versions",
            |d| d.recent_versions.begin(),
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
        d.cw_alarms = Default::default();
        d.recent_versions = Default::default();
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
    app.environments = vec![mk_env("api-prod", "uflexi", tier, "Red")];
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
    d.loading_events = false;
    d.events = vec![make_event("EVENT-CANARY deployment failed")];

    let out = render(&mut app, 160, 44);
    assert!(out.contains("EVENT-CANARY"), "event text renders:\n{out}");
}

#[tokio::test]
async fn detail_instances_tab_renders_its_instances() {
    let mut app = detail_app_on_tab(DetailTab::Instances);
    let d = app.detail.as_mut().unwrap();
    d.loading_instances = false;
    d.instances = vec![crate::aws::Instance {
        id: "i-0canary99".into(),
        health: "Severe".into(),
        color: "Red".into(),
        causes: vec!["ELB health failing".into()],
        instance_type: "t3.medium".into(),
        availability_zone: "eu-west-2a".into(),
        launched_at: None,
    }];

    let out = render(&mut app, 160, 44);
    assert!(out.contains("i-0canary99"), "instance id renders:\n{out}");
}

#[tokio::test]
async fn detail_queue_tab_renders_its_queues() {
    let mut app = detail_app_on_tab_for_tier(DetailTab::Queue, "Worker");
    let d = app.detail.as_mut().unwrap();
    d.loading_queues = false;
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
    app.environments = vec![mk_env("api-prod", "uflexi", "WebServer", "Green")];
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
    d.loading_events = false;
    d.loading_queues = false;
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
    assert!(!d.tab_loading(), "idle must not claim to be loading");
    assert!(!d.cw_alarms.is_first_load());
    assert!(d.cw_alarms.ready().is_none() && d.cw_alarms.error().is_none());

    // First load: running, nothing to show, so the spinner is on.
    d.cw_alarms.begin();
    assert!(d.tab_loading(), "first load is in flight");
    assert!(
        d.cw_alarms.is_first_load(),
        "nothing to show yet, so ui/detail.rs draws `fetching alarms…`"
    );

    // Settled with data.
    d.cw_alarms.settle(Ok(alarm()));
    assert!(!d.tab_loading(), "settled must not claim to be loading");
    assert!(!d.cw_alarms.is_first_load());
    assert_eq!(d.cw_alarms.ready().map(Vec::len), Some(1));

    // THE state a four-variant enum would lose: a refresh in flight with
    // the previous result still displayed.
    d.cw_alarms.begin();
    assert!(
        d.tab_loading(),
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
    d.cw_alarms.settle(Err("DescribeAlarms denied".into()));
    assert!(!d.tab_loading());
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
    app.environments = vec![mk_env("wk-prod", "uflexi", "Worker", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();

    let gen = app.generation;
    // Events fetch fails — AccessDenied on DescribeEvents, say.
    app.handle_msg(crate::app::AppMsg::DetailEvents {
        gen,
        env_name: "wk-prod".to_string(),
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
    app.handle_msg(crate::app::AppMsg::DetailInstances {
        gen,
        env_name: "wk-prod".to_string(),
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
    app.environments = vec![mk_env(env, "uflexi", "Web", "Green")];
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

    // Each case: a message naming `worker-prod` arriving while Detail is
    // open on `api-prod`, and what it would have overwritten.
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
        env_name: "worker-prod".into(),
        groups: vec!["wrong-env-group".into()],
    });
    assert_eq!(
        app.detail.as_ref().unwrap().cw_log_groups,
        Some(vec!["existing-group".to_string()]),
        "another env's log groups were shown against this one"
    );

    // Alarms.
    let mut app = armed();
    app.handle_msg(AppMsg::DetailAlarms {
        gen: app.generation,
        env_name: "worker-prod".into(),
        result: Ok(Vec::new()),
    });
    assert!(
        app.detail.as_ref().unwrap().cw_alarms.ready().is_none(),
        "another env's alarm result settled this env's alarm panel"
    );

    // Recent versions.
    let mut app = armed();
    app.handle_msg(AppMsg::DetailRecentVersions {
        gen: app.generation,
        env_name: "worker-prod".into(),
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

    // And the matching env DOES apply — without this half, a handler
    // that dropped everything would pass all of the above.
    let mut app = armed();
    app.handle_msg(AppMsg::DetailLogGroups {
        gen: app.generation,
        env_name: "api-prod".into(),
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
        env_name: "worker-prod".into(),
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
        env_name: "api-prod".into(),
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
