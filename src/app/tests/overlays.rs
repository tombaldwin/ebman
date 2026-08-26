//! Overlays, modals, forms, pickers, the splash and help.
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
fn render_config_diff_overlay_states() {
    // No differences → identical message.
    let body = crate::app::render_config_diff_overlay("staging", "prod", &[]);
    assert!(body.contains("identical"));
    // With a diff → the namespace + name + both values appear.
    let diffs = vec![crate::app::ConfigDiff {
        namespace: "aws:autoscaling:asg".into(),
        name: "MinSize".into(),
        left: Some("2".into()),
        right: None,
    }];
    let body = crate::app::render_config_diff_overlay("staging", "prod", &diffs);
    assert!(body.contains("aws:autoscaling:asg"));
    assert!(body.contains("MinSize"));
    assert!(body.contains("L: 2"));
    assert!(body.contains("R: (unset)"));
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
    let body =
        crate::app::render_explain_overlay("arn:aws:iam::123:role/EbmanReadOnly", &rows, false);
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
    let body = crate::app::render_explain_overlay("arn:aws:iam::123:role/X", &rows, false);
    assert!(body.contains("Organizations SCP"));
    assert!(body.contains("permission boundary"));
    // explicitDeny gives the "Remove the Deny" hint instead of
    // the implicitDeny JSON snippet.
    assert!(body.contains("explicit Deny always wins"));
    assert!(!body.contains("\"Effect\": \"Allow\""));
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
fn render_options_overlay_handles_unknown_namespace() {
    let rows = vec![opt("aws:autoscaling:asg", "MinSize", Some("2"), None)];
    let body = crate::app::render_options_overlay(&rows, Some("aws:bogus:ns"), "uflexi-prod");
    assert!(body.contains("No options found"));
    assert!(body.contains("aws:bogus:ns"));
}

#[test]
fn render_secrets_overlay_empty_no_filter_hints_at_iam() {
    let body = crate::app::render_secrets_overlay(&[], None);
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
    let body = crate::app::render_secrets_overlay(&rows, None);
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
    let body = crate::app::render_secrets_overlay(&rows, None);
    assert!(body.contains("rotated: never"));
}

#[test]
fn render_secret_value_overlay_redacts_when_redact_on() {
    let body = crate::app::render_secret_value_overlay("api-key", "hunter2", true);
    assert!(body.contains("<redacted; 7 chars"));
    assert!(body.contains("fingerprint"));
    assert!(!body.contains("hunter2"));
    assert!(body.contains(":redact off"));
}

#[test]
fn render_secret_value_overlay_shows_value_when_redact_off() {
    let body = crate::app::render_secret_value_overlay("api-key", "hunter2", false);
    assert!(body.contains("hunter2"));
    assert!(body.contains("yank"));
}

#[test]
fn render_secret_value_overlay_pretty_prints_json() {
    let body = crate::app::render_secret_value_overlay(
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
    let body = crate::app::render_secret_value_overlay("flat", "ABC-DEF-GHI", false);
    assert!(body.contains("ABC-DEF-GHI"));
}

#[test]
fn render_options_overlay_truncates_long_value_options_list() {
    let mut row = opt("aws:foo", "Enum", Some("a"), None);
    row.value_options = (0..20).map(|i| format!("v{i}")).collect();
    let rows = vec![row];
    let body = crate::app::render_options_overlay(&rows, None, "env");
    assert!(body.contains("oneof: v0, v1, v2, v3, v4, … +15"));
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
    assert!(crate::app::render_changes_overlay("prod", &noise).contains("No deploy"));
    // A deploy event is kept and its version label shown.
    let evs = vec![
        ev("Deploying new version to instance(s).", Some("build-9")),
        ev("Environment health transitioned to Ok.", None),
    ];
    let body = crate::app::render_changes_overlay("prod", &evs);
    assert!(body.contains("Deploying new version"));
    assert!(body.contains("[build-9]"));
    assert!(!body.contains("health transitioned"));
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
        crate::app::classify_update_kind(&evs),
        crate::app::UpdateKind::Platform
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
            assert_eq!(modal.params.deploy_version.as_deref(), Some("build-900"));
            assert_eq!(modal.params.wait_for_green_secs, Some(300));
            assert!(modal.params.auto_rollback_secs.is_none());
        }
        _ => panic!("expected confirm modal open"),
    }
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

// --- render smoke for the form overlay ------------------------------
//
// Found by the 41-surface coverage sweep: `draw_form` could be replaced
// with `return;` and all 1,106 tests still passed. Worth calling out,
// because earlier in this cycle its scroll behaviour was recorded as
// "already fixed, verified" on the strength of reading the code — with
// nothing exercising it.

#[tokio::test]
async fn the_form_overlay_renders_its_fields() {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
    app.view.invalidate();
    app.rebuild_view();
    app.table_state.select(Some(0));

    let mut form = crate::form::Form::loading(
        "FORM-CANARY-TITLE",
        "api-prod",
        "resize the ASG",
        vec![
            crate::form::FormField::text("minsize", "MinSize-CANARY", None::<String>),
            crate::form::FormField::text("maxsize", "MaxSize-CANARY", Some("upper bound")),
        ],
        crate::form::FormSubmit::OptionSettings {
            mappings: Vec::new(),
        },
    );
    form.state = crate::form::FormState::Ready;
    app.form = Some(form);
    app.mode = crate::app::Mode::Form;

    let out = render(&mut app, 160, 44);
    assert!(
        out.contains("FORM-CANARY-TITLE"),
        "the title renders:\n{out}"
    );
    assert!(
        out.contains("MinSize-CANARY") && out.contains("MaxSize-CANARY"),
        "every field renders, not just the focused one:\n{out}"
    );
    assert!(
        out.contains("api-prod"),
        "and the env it will act on — this form writes:\n{out}"
    );
}

// --- render smoke for the body-carrying overlays --------------------
//
// From the 41-surface coverage sweep: each of these could be replaced
// with `return;` and nothing failed. They all follow the same shape —
// an `Overlay` variant carrying text, and a `draw_*` that has to put
// that text on screen — so one table covers the lot. The canary strings
// are deliberately unique so a passing assertion can't be satisfied by
// the chrome drawn around the overlay.
#[tokio::test]
async fn every_body_carrying_overlay_renders_its_body() {
    use crate::app::Overlay;
    let cases: Vec<(&str, Overlay)> = vec![
        ("CANARYDESCRIBE", Overlay::Describe("CANARYDESCRIBE".into())),
        ("CANARYWHATSNEW", Overlay::Whatsnew("CANARYWHATSNEW".into())),
        ("CANARYHISTORY", Overlay::History("CANARYHISTORY".into())),
        (
            "CANARYALARMS",
            Overlay::Alarms {
                env_name: "api-prod".into(),
                body: "CANARYALARMS".into(),
            },
        ),
        ("CANARYDIFF", Overlay::Diff("CANARYDIFF".into())),
        (
            "CANARYSAVEDCFG",
            Overlay::SavedConfigs("CANARYSAVEDCFG".into()),
        ),
        (
            "CANARYTEXTDUMP",
            Overlay::TextDump {
                title: "dump".into(),
                body: "CANARYTEXTDUMP".into(),
            },
        ),
        (
            "CANARYREPORTBUG",
            Overlay::ReportBug {
                body: "CANARYREPORTBUG".into(),
            },
        ),
        (
            "CANARYCFGITEM",
            Overlay::SavedConfigsInteractive {
                items: vec![("CANARYCFGITEM".into(), "/tmp/x.cfg.yml".into())],
                cursor: 0,
                confirm_delete: false,
            },
        ),
    ];

    for (needle, overlay) in cases {
        let mut app = test_app();
        app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
        app.view.invalidate();
        app.rebuild_view();
        app.table_state.select(Some(0));
        app.current_overlay = Some(overlay);

        let out = render(&mut app, 160, 44);
        assert!(
            out.contains(needle),
            "overlay did not render its own body ({needle}):\n{out}"
        );
    }
}

#[tokio::test]
async fn the_command_palette_renders_its_items() {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
    app.view.invalidate();
    app.rebuild_view();
    app.open_palette();
    assert_eq!(app.mode, crate::app::Mode::Palette, "palette opened");
    assert!(!app.palette_items.is_empty(), "and has something to show");

    let out = render(&mut app, 160, 44);
    let first = app.palette_items[0].label.clone();
    assert!(
        out.contains(&first),
        "the first palette entry ({first}) has to be on screen:\n{out}"
    );
}

#[tokio::test]
async fn the_picker_renders_its_choices() {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
    app.view.invalidate();
    app.rebuild_view();
    app.picker = Some(crate::app::Picker {
        kind: crate::app::PickerKind::Region,
        items: vec!["eu-west-2-CANARY".into(), "us-east-1".into()],
        filter: tui_common::TextInput::new(),
        list_state: Default::default(),
    });
    app.mode = crate::app::Mode::Picker;

    let out = render(&mut app, 160, 44);
    assert!(out.contains("eu-west-2-CANARY"), "choices render:\n{out}");
}

#[tokio::test]
async fn toasts_render_over_everything_else() {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
    app.view.invalidate();
    app.rebuild_view();
    app.toasts.push_back(crate::app::Toast {
        text: "TOASTCANARY restart dispatched".into(),
        kind: crate::app::ToastKind::Info,
        shown_at: std::time::Instant::now(),
    });

    let out = render(&mut app, 160, 44);
    assert!(out.contains("TOASTCANARY"), "the toast renders:\n{out}");
}

#[tokio::test]
async fn the_apps_scope_renders_its_own_table() {
    // A whole alternate table — `:apps` swaps it in for the env table.
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "APPCANARY", "Web", "Green")];
    app.applications = vec![crate::aws::Application {
        description: "the one app".into(),
        version_count: 3,
        ..mk_application("APPCANARY")
    }];
    app.view.invalidate();
    app.rebuild_view();
    app.scope = crate::app::Scope::Apps;
    app.rebuild_view();

    let out = render(&mut app, 160, 44);
    // Assert on this table's OWN column headers, not on the app name:
    // the name also reaches the header breadcrumb, so the first cut of
    // this assertion passed with `draw_apps_table` stubbed out entirely
    // — the same mistake the Logs tab test made.
    assert!(
        out.contains("VERSIONS") && out.contains("DESCRIPTION"),
        "the apps table draws its own columns:\n{out}"
    );
    assert!(
        out.contains("the one app"),
        "and the row body, which only this table renders:\n{out}"
    );
}

#[tokio::test]
async fn every_help_topic_renders_something_of_its_own() {
    // `draw_help` fans out to a per-topic renderer, and the sweep found
    // all seven bare. A stubbed one leaves the popup frame drawn by the
    // chrome, so assert on line count inside the popup rather than on
    // the frame itself.
    use crate::app::HelpTopic;
    // Each topic's own pane title. Needles chosen from the help source
    // rather than guessed: earlier attempts asserted on "esc" (the
    // footer keystrip draws it in every mode) and then on "the frame
    // differs from Normal mode" (the footer changes with mode on its
    // own). Both passed with every help renderer stubbed out.
    //
    // It now also pins REACHABILITY, and that distinction is the point.
    // This test used to set `app.help.topic` directly, so it passed for
    // years while `HelpTopic::Shell` was never constructed anywhere in
    // production — a renderer proven to work down a path nothing could
    // take. Driving `:help <topic>` instead exercises the wiring.
    for &topic in HelpTopic::ALL {
        // Exhaustive on purpose: a new topic has to declare its title
        // here, and therefore has to have one.
        let title = match topic {
            HelpTopic::Global => "ebman — keybindings",
            HelpTopic::Detail => "Detail view — keybindings",
            HelpTopic::Dlq => "Queue viewer — keybindings",
            HelpTopic::Action => "Action menu — keybindings",
            HelpTopic::Shell => "Embedded shell — keybindings",
            HelpTopic::SavedConfigs => "Saved configurations — keybindings",
        };
        let mut app = test_app();
        app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
        app.view.invalidate();
        app.rebuild_view();
        app.table_state.select(Some(0));
        app.help.scroll = 0;

        app.execute_command(&format!("help {}", topic.arg_name()));
        assert_eq!(
            app.mode,
            crate::app::Mode::Help,
            "`:help {}` did not open the help screen",
            topic.arg_name()
        );
        assert_eq!(
            app.help.topic,
            topic,
            "`:help {}` opened the wrong topic",
            topic.arg_name()
        );

        let out = render(&mut app, 200, 44);
        assert!(
            out.contains(title),
            "help topic {topic:?} did not draw its own title {title:?}:\n{out}"
        );
    }
}

#[tokio::test]
async fn the_about_overlay_renders() {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
    app.view.invalidate();
    app.rebuild_view();
    app.current_overlay = Some(crate::app::Overlay::About(std::time::Instant::now()));

    let out = render(&mut app, 160, 44);
    assert!(
        out.contains(env!("CARGO_PKG_VERSION")),
        "About names the version it is:\n{out}"
    );
}

#[tokio::test]
async fn the_why_red_overlay_renders_its_findings() {
    // The triage path: `:why` on a red environment. Worth covering for
    // the same reason as the DLQ viewer — it is reached mid-incident.
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Red")];
    app.view.invalidate();
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.current_overlay = Some(crate::app::Overlay::WhyRed {
        env_name: "api-prod".into(),
        tier: "Web".into(),
        events: Some(Ok(vec![make_event("WHYREDCANARY deploy failed")])),
        alarms: Some(Ok(Vec::new())),
        instances: Some(Ok(Vec::new())),
        deploys: Some(Ok(Vec::new())),
        queues: None,
        dlq_messages: None,
        session_id: 1,
        cursor: 0,
    });

    let out = render(&mut app, 180, 44);
    assert!(
        out.contains("WHYREDCANARY"),
        "the event that explains the red renders:\n{out}"
    );
}

#[tokio::test]
async fn the_log_tail_overlay_renders_its_lines() {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
    app.view.invalidate();
    app.rebuild_view();
    let mut events = std::collections::VecDeque::new();
    events.push_back(crate::aws::LogEvent {
        timestamp_ms: 1_700_000_000_000,
        stream: "i-0abc/web.stdout".into(),
        message: "LOGTAILCANARY 500 internal error".into(),
    });
    app.current_overlay = Some(crate::app::Overlay::LogTail {
        log_group: "/aws/elasticbeanstalk/api-prod/var/log/web.stdout.log".into(),
        env_name: "api-prod".into(),
        events,
        since_ms: 0,
        view: Default::default(),
        last_err: None,
        session_id: 1,
    });

    let out = render(&mut app, 180, 44);
    assert!(
        out.contains("LOGTAILCANARY"),
        "the tailed line renders:\n{out}"
    );
}

#[tokio::test]
async fn the_apps_action_menu_renders_its_actions() {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "MENUCANARY", "Web", "Green")];
    app.view.invalidate();
    app.rebuild_view();
    app.current_overlay = Some(crate::app::Overlay::AppsActionMenu {
        app_name: "MENUCANARY".into(),
        env_names: vec!["api-prod".into(), "api-staging".into()],
        cursor: 0,
    });

    let out = render(&mut app, 180, 44);
    // The menu renders counts, not env names. "Rebuild all 2 env(s)" is
    // a string only this overlay builds, so it can't be satisfied by the
    // table or the footer underneath.
    assert!(
        out.contains("Rebuild all 2 env(s)"),
        "the menu offers its fan-out actions with the env count:\n{out}"
    );
    assert!(
        out.contains("MENUCANARY"),
        "and names the application:\n{out}"
    );
}

// ── mutation-sweep triage, 2026-08-26 ────────────────────────────────
//
// `handle_saved_configs_interactive_key` had 18 survivors around a
// delete confirm. It is a y/n gate rather than a typed one, and the
// standing guard did not reach it — which is why that guard now
// enumerates `confirm_*` state as well as typed comparisons, and is
// named `every_confirmation_gate_names_its_test` rather than
// `..._typed_...`. This gate is one of its entries.

fn saved_configs_overlay(confirm_delete: bool) -> App {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "shop", "WebServer", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.current_overlay = Some(crate::app::Overlay::SavedConfigsInteractive {
        items: vec![
            ("shop".into(), "prod-baseline".into()),
            ("shop".into(), "canary".into()),
        ],
        cursor: 0,
        confirm_delete,
    });
    app.mode = crate::app::Mode::Normal;
    app
}

fn overlay_state(app: &App) -> Option<(usize, bool)> {
    match app.current_overlay.as_ref()? {
        crate::app::Overlay::SavedConfigsInteractive {
            cursor,
            confirm_delete,
            ..
        } => Some((*cursor, *confirm_delete)),
        _ => None,
    }
}

/// Enter means APPLY when nothing is armed, and CONFIRM DELETE when the
/// delete prompt is up. Those two arms are told apart by `!confirm_delete`
/// versus `confirm_delete`, and the sweep left every mutant of the first
/// guard alive.
///
/// Deleting that `!` makes both guards identical, so the apply arm — the
/// earlier one — wins on Enter *while the delete confirm is showing*.
/// The operator presses Enter to delete a saved configuration and
/// instead applies it to the environment, rewriting its option settings.
#[tokio::test]
async fn enter_applies_only_when_no_delete_is_armed() {
    // Nothing armed: Enter applies. `spawn_config_apply_template` closes
    // the overlay, so that is the observable.
    let mut app = saved_configs_overlay(false);
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        app.current_overlay.is_none(),
        "Enter with nothing armed applies the selected template"
    );

    // Armed: Enter confirms the DELETE, and must not take the apply arm.
    // Both close the overlay, so the discriminator is which spawn ran —
    // a read-only env refuses the write and names the operation.
    let mut app = saved_configs_overlay(true);
    app.read_only = true;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let msg = app.error_message.clone().unwrap_or_default();
    assert!(
        msg.contains("delete") || msg.contains("read-only") || msg.contains("refus"),
        "Enter while the delete confirm is armed must dispatch the DELETE, \
         not apply the config to the environment: {msg:?}"
    );
}

/// `n` / `N` / `Esc` back out of the delete confirm, and navigation is
/// inert while it is armed so a stray `j` can't silently discard it.
#[tokio::test]
async fn a_delete_confirm_can_be_declined_and_ignores_navigation() {
    for decline in [KeyCode::Char('n'), KeyCode::Char('N'), KeyCode::Esc] {
        let mut app = saved_configs_overlay(true);
        press(&mut app, decline, KeyModifiers::NONE);
        assert_eq!(
            overlay_state(&app),
            Some((0, false)),
            "{decline:?} disarms the confirm and leaves the overlay open"
        );
    }

    // Everything except y/Y/Enter and n/N/Esc is inert while armed.
    //
    // The navigation keys are the obvious case — a stray `j` must not
    // discard the confirm and reset the cursor. But the ones that
    // matter are `q`, `a`, `c`, `x` and `i`, which all have arms in the
    // dispatch block below: the armed branch returns rather than
    // falling through, and without that `q` would CLOSE the overlay and
    // `a` would APPLY the config, both while the operator is looking at
    // a delete prompt. Testing only j/k/G misses every one of those —
    // they match nothing below, so falling through looks inert.
    for inert in [
        KeyCode::Char('j'),
        KeyCode::Char('k'),
        KeyCode::Char('G'),
        KeyCode::Char('g'),
        KeyCode::Char('q'),
        KeyCode::Char('a'),
        KeyCode::Char('c'),
        KeyCode::Char('x'),
        KeyCode::Char('i'),
    ] {
        let mut app = saved_configs_overlay(true);
        press(&mut app, inert, KeyModifiers::NONE);
        assert_eq!(
            overlay_state(&app),
            Some((0, true)),
            "{inert:?} must do nothing while a delete confirm is armed — \
             the overlay stays open, the cursor stays put, and the \
             confirm stays armed"
        );
        assert!(
            app.error_message.is_none(),
            "{inert:?} must not dispatch anything: {:?}",
            app.error_message
        );
    }
}

/// And navigation works when nothing is armed, so "inert" above isn't
/// just "these keys never do anything".
#[tokio::test]
async fn saved_configs_navigation_moves_the_cursor_when_unarmed() {
    let mut app = saved_configs_overlay(false);
    press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
    assert_eq!(overlay_state(&app), Some((1, false)), "j moves down");
    press(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
    assert_eq!(overlay_state(&app), Some((0, false)), "k moves up");
    // Clamped at both ends rather than wrapping.
    press(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
    assert_eq!(overlay_state(&app), Some((0, false)), "k clamps at the top");
    press(&mut app, KeyCode::Char('G'), KeyModifiers::NONE);
    assert_eq!(overlay_state(&app), Some((1, false)), "G jumps to the end");
    press(&mut app, KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(overlay_state(&app), Some((0, false)), "g jumps to the top");
}

/// `x` arms the confirm rather than deleting outright.
#[tokio::test]
async fn x_arms_the_delete_confirm_rather_than_deleting() {
    let mut app = saved_configs_overlay(false);
    press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
    assert_eq!(
        overlay_state(&app),
        Some((0, true)),
        "`x` must arm the confirm and keep the overlay open, not delete"
    );
    assert!(
        app.error_message.is_none(),
        "and must not have dispatched anything: {:?}",
        app.error_message
    );
}
