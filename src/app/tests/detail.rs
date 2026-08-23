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
    // Aliases.
    assert_eq!(alarm_kind_to_metric("req5xx"), alarm_kind_to_metric("5xx"));
    assert_eq!(alarm_kind_to_metric("p90"), alarm_kind_to_metric("latency"));
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
