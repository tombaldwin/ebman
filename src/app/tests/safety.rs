//! Rule for every mutating path — `deny_write`, read-only,
//! the freeze window, and the tables that pin which commands write.
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
            assert_eq!(modal.params.deploy_version.as_deref(), Some("build-820"));
            // No watchdog when --auto-rollback wasn't passed.
            assert!(modal.params.auto_rollback_secs.is_none());
        }
        _ => panic!("expected confirm modal open"),
    }
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
            assert_eq!(modal.params.deploy_version.as_deref(), Some("build-900"));
            assert!(matches!(modal.action, Action::Deploy));
        }
        _ => panic!("expected confirm modal open on target"),
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

// --- the destructive commands actually route --------------------------
//
// From the 131-command dispatch sweep: each of these could be turned
// into a no-op and the whole suite stayed green. The safety tests pin
// the 29 declared WRITE_COMMANDS, but those are the option-setting ones
// that gate inside their own handler. The confirm-modal actions —
// restart, rebuild, terminate, stop, start — were pinned by nothing, so
// a broken or renamed dispatch arm would silently do nothing at all.

#[tokio::test]
async fn the_confirm_modal_commands_arm_the_right_action() {
    for (cmd, expected) in [
        ("restart", Action::RestartAppServer),
        ("rebuild", Action::Rebuild),
        ("stop", Action::Scale),
        ("start", Action::Scale),
    ] {
        let mut app = test_app();
        app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
        app.view.invalidate();
        app.rebuild_view();
        app.table_state.select(Some(0));

        app.execute_command(cmd);

        let Some(ActionFlow::Confirm(modal)) = app.action_flow.as_ref() else {
            panic!(
                ":{cmd} armed no confirm modal at all (error: {:?})",
                app.error_message
            );
        };
        assert_eq!(modal.action, expected, ":{cmd} armed the wrong action");
        assert_eq!(
            modal.target_env, "api-prod",
            ":{cmd} aimed at the wrong env"
        );
        assert_eq!(app.mode, Mode::Action, ":{cmd} left the mode behind");
    }
}

#[tokio::test]
async fn terminate_routes_to_the_strict_typed_name_guard() {
    // Terminate deliberately does NOT use the Y/N confirm the others
    // do — it goes through the action menu so the operator has to type
    // the env name. That difference is the whole safety story for the
    // one irreversible action, and nothing pinned it.
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
    app.view.invalidate();
    app.rebuild_view();
    app.table_state.select(Some(0));

    app.execute_command("terminate");

    let Some(ActionFlow::Confirm(modal)) = app.action_flow.as_ref() else {
        panic!(
            ":terminate armed no confirm at all (error: {:?})",
            app.error_message
        );
    };
    assert_eq!(modal.action, Action::Terminate);
    assert_eq!(modal.target_env, "api-prod");
    assert_eq!(
        modal.kind,
        ConfirmKind::TypeName,
        ":terminate must demand the typed env name, not a Y/N"
    );
}

#[tokio::test]
async fn destructive_commands_still_refuse_under_deny_write() {
    // The routing tests above arm a modal; this pins that the same
    // route still refuses when writes are denied. Without it, a fix to
    // routing could quietly bypass the gate and both other tests would
    // still pass.
    for cmd in ["restart", "rebuild", "terminate", "stop", "start"] {
        let mut app = read_only_app_with_env();
        app.execute_command(cmd);
        assert!(
            app.action_flow.is_none(),
            ":{cmd} armed an action despite --deny-write"
        );
        assert!(
            app.error_message.is_some(),
            ":{cmd} refused silently — the operator needs to be told"
        );
    }
}

// --- the mutating commands the write tables never listed -------------
//
// From the 131-command dispatch sweep. `WRITE_COMMANDS` pins the
// option-setting commands — the ones with no `deny_write` of their own.
// Everything that gates *inside* its own handler was therefore in no
// list at all, so nothing pinned that it kept doing so. All of these
// were verified to refuse before being listed here; none of them was a
// hole, but none of them was pinned either.

#[tokio::test]
async fn every_gated_command_is_refused_in_read_only_mode() {
    for cmd in GATED_COMMANDS {
        let mut app = read_only_app_with_env();
        app.execute_command(cmd);
        let err = app.error_message.as_deref().unwrap_or_default();
        assert!(
            err.contains("read-only mode"),
            ":{cmd} was not refused by the safety gate — got {err:?}"
        );
    }
}

#[tokio::test]
async fn swap_is_refused_in_read_only_mode() {
    // Needs a second env in the same application, or it is turned away
    // on the argument before the gate is ever consulted — which is what
    // made it look ungated on first inspection.
    let mut app = read_only_app_with_env();
    app.environments
        .push(mk_env("api-staging", "uflexi", "Web", "Green"));
    app.view.invalidate();
    app.rebuild_view();
    app.table_state.select(Some(0));

    app.execute_command("swap api-staging");
    let err = app.error_message.as_deref().unwrap_or_default();
    assert!(err.contains("read-only mode"), ":swap got {err:?}");
}

#[tokio::test]
async fn ssm_run_is_refused_in_read_only_mode() {
    // Same shape: it needs cached instances from an open Detail pane
    // before it reaches the gate.
    let mut app = read_only_app_with_env();
    app.open_detail();
    if let Some(d) = app.detail.as_mut() {
        d.instances = vec![crate::aws::Instance {
            id: "i-0abc".into(),
            health: "Ok".into(),
            color: "Green".into(),
            causes: Vec::new(),
            instance_type: "t3.medium".into(),
            availability_zone: "eu-west-2a".into(),
            launched_at: None,
        }];
    }

    app.execute_command("ssm-run uptime");
    let err = app.error_message.as_deref().unwrap_or_default();
    assert!(err.contains("read-only mode"), ":ssm-run got {err:?}");
}

/// `safety.envs.NAME.read_only` must protect an env from being swapped
/// INTO, not just out of.
///
/// A CNAME swap rewrites BOTH environments' DNS, so it is a write to the
/// target as much as to the source. But the only `deny_write` on this
/// path was in `open_action_menu`, against the *selected* env — and the
/// target is chosen afterwards, from a picker. So a pin on `green` did
/// nothing if you selected `blue` first and swapped towards it.
///
/// This drives the real flow — open the menu on the unpinned env, pick
/// the pinned one — rather than calling `deny_write` directly, which
/// would only prove the gate function works and not that anything calls
/// it. The first version of this test made exactly that mistake and
/// passed against the unfixed code.
#[tokio::test]
async fn a_read_only_env_cannot_be_swapped_into() {
    let mut app = test_app();
    app.environments = vec![
        mk_env("blue", "shop", "WebServer", "Green"),
        mk_env("green", "shop", "WebServer", "Green"),
    ];
    app.rebuild_view();
    app.cfg.safety_envs.insert("green".into(), true);
    assert!(app.is_read_only_for("green"), "the pin must be in effect");
    assert!(!app.is_read_only_for("blue"), "the source is writable");

    // Select the UNPINNED env; the menu opens because only it is checked.
    app.table_state.select(Some(0));
    assert!(app.open_action_menu(), "`blue` is writable");
    app.advance_action_flow(crate::app::Action::SwapCnames);

    // The picker should be offering `green` as the swap target.
    let picking = matches!(
        app.action_flow,
        Some(crate::app::ActionFlow::SwapTarget { .. })
    );
    assert!(picking, "swap opens a target picker");

    // Choose it. This is the moment the target becomes known.
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);

    // It must NOT have reached a confirm modal.
    let confirmed = matches!(app.action_flow, Some(crate::app::ActionFlow::Confirm(_)));
    assert!(
        !confirmed,
        "a swap INTO a read-only env must be refused before the confirm \
         modal, not dispatched"
    );
    let msg = app
        .error_message
        .clone()
        .or_else(|| app.status_message.clone())
        .unwrap_or_default();
    assert!(
        msg.contains("green"),
        "the refusal must name the pinned env so the operator knows which \
         pin stopped it, got: {msg:?}"
    );
}

/// The command path has the same hole as the picker path, so it needs
/// the same gate. `:swap TARGET` routes through
/// `open_parameterised_action`, which checks the env it was handed —
/// the SOURCE — and never looked at the target.
///
/// Separate test from the picker one because they are separate entry
/// points into the same write, and fixing one is exactly how the other
/// gets left behind.
#[tokio::test]
async fn swap_cnames_command_also_gates_the_target() {
    let mut app = test_app();
    app.environments = vec![
        mk_env("blue", "shop", "WebServer", "Green"),
        mk_env("green", "shop", "WebServer", "Green"),
    ];
    app.rebuild_view();
    app.cfg.safety_envs.insert("green".into(), true);
    app.table_state.select(Some(0)); // `blue` — writable

    app.execute_command("swap green");

    let confirmed = matches!(app.action_flow, Some(crate::app::ActionFlow::Confirm(_)));
    assert!(
        !confirmed,
        "`:swap green` must be refused when `green` is pinned \
         read-only, even though the selected env `blue` is writable"
    );
    let msg = app
        .error_message
        .clone()
        .or_else(|| app.status_message.clone())
        .unwrap_or_default();
    assert!(
        msg.contains("green"),
        "the refusal must name the pinned env, got: {msg:?}"
    );
}
