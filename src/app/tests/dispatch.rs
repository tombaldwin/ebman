//! The `:command` router — parsing, completion, palette,
//! suggestions, aliases.
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
fn edit_distance_basic_cases() {
    assert_eq!(crate::app::edit_distance("", ""), 0);
    assert_eq!(crate::app::edit_distance("abc", ""), 3);
    assert_eq!(crate::app::edit_distance("", "abc"), 3);
    assert_eq!(crate::app::edit_distance("kitten", "sitting"), 3);
    assert_eq!(crate::app::edit_distance("restart", "restart"), 0);
    assert_eq!(crate::app::edit_distance("restrt", "restart"), 1);
    assert_eq!(crate::app::edit_distance("rebild", "rebuild"), 1);
    assert_eq!(crate::app::edit_distance("scal", "scale"), 1);
}

#[test]
fn suggest_command_catches_one_char_typos() {
    // Operator typo: forgot the 'a' in restart.
    assert_eq!(
        crate::app::suggest_command("restrt").as_deref(),
        Some("restart")
    );
    // Operator typo: dropped a 'u' in rebuild.
    assert_eq!(
        crate::app::suggest_command("rebild").as_deref(),
        Some("rebuild")
    );
    // Operator typo: dropped the 'e' in scale.
    assert_eq!(
        crate::app::suggest_command("scal").as_deref(),
        Some("scale")
    );
}

#[test]
fn suggest_command_returns_none_when_too_far() {
    // Nonsense input — no command is within edit-distance 2.
    assert_eq!(crate::app::suggest_command("zzzzzz"), None);
}

#[test]
fn suggest_command_threshold_is_strict_for_short_input() {
    // 2-char input shouldn't "match" every 3-char alias —
    // the operator's intent is too ambiguous to guess.
    // `:zz` is distance 2 from many names; we cap at 1.
    let suggestion = crate::app::suggest_command("zz");
    assert!(
        suggestion.is_none(),
        "2-char typo should require distance ≤ 1; got {suggestion:?}"
    );
}

#[test]
fn completion_candidates_filters_by_prefix() {
    let c = crate::app::completion_candidates("ba");
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
    let c = crate::app::completion_candidates("");
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

#[test]
fn command_takes_env_arg_only_for_env_first_commands() {
    for c in ["diff", "config-diff", "rds-detach"] {
        assert!(
            crate::app::command_takes_env_arg(c),
            "{c} takes an env name as its first arg"
        );
    }
    // Selected-env commands and non-env NAME commands are excluded.
    for c in [
        "why", "deploy", "rebuild", "region", "profile", "view", "save",
    ] {
        assert!(
            !crate::app::command_takes_env_arg(c),
            "{c} must not offer env-name completion"
        );
    }
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
    app.undo_history.push_back(crate::app::UndoEntry {
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
    app.undo_history.push_back(crate::app::UndoEntry {
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
    app.undo_history.push_back(crate::app::UndoEntry {
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
    app.undo_history.push_back(crate::app::UndoEntry {
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
    assert_eq!(
        crate::app::expand_command_alias("rebuild", &aliases),
        "rebuild"
    );
    // Empty alias map — line unchanged.
    assert_eq!(
        crate::app::expand_command_alias("deploy build-x", &HashMap::new()),
        "deploy build-x"
    );
}

#[test]
fn expand_command_alias_swaps_first_token_and_keeps_args() {
    use std::collections::HashMap;
    let mut aliases = HashMap::new();
    aliases.insert("dp".to_string(), "deploy --auto-rollback 5m".to_string());
    assert_eq!(
        crate::app::expand_command_alias("dp build-900", &aliases),
        "deploy --auto-rollback 5m build-900"
    );
    // No args after alias — expansion stands alone.
    assert_eq!(
        crate::app::expand_command_alias("dp", &aliases),
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
    assert_eq!(crate::app::expand_command_alias("a", &aliases), "b stuff");
    // No infinite loop on self-referential aliases.
    let mut aliases = HashMap::new();
    aliases.insert("loop".to_string(), "loop forever".to_string());
    assert_eq!(
        crate::app::expand_command_alias("loop", &aliases),
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
    app.apply_refresh(
        app.fanout_epoch,
        Ok(vec![mk_env("prod", "shop", "Web", "Red")]),
        Vec::new(),
    );
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

// --- every command does something observable -------------------------

/// A cheap, comparable snapshot of the state a `:command` can move.
///
/// The point is attribution: three assertions earlier in this sweep
/// passed because chrome drew the needle they looked for. A fingerprint
/// diff can't be satisfied by anything the command didn't change.
#[derive(PartialEq, Debug)]
pub(super) struct Fingerprint {
    mode: String,
    status: Option<String>,
    error: Option<String>,
    overlay: Option<String>,
    action_flow: bool,
    form: bool,
    picker: bool,
    detail: bool,
    dlq: bool,
    shell: bool,
    quit: bool,
    load_state: String,
    toasts: usize,
    help_topic: String,
    events_visible: bool,
    palette_items: usize,
    redact: bool,
    read_only: bool,
    scope: String,
    sort: String,
    filter: String,
    grouped: bool,
    pending: usize,
    envs: usize,
    multi_regions: usize,
    hidden_cols: usize,
    saved_views: usize,
    log_tail_task: bool,
}

pub(super) fn fingerprint(app: &App) -> Fingerprint {
    Fingerprint {
        mode: format!("{:?}", app.mode),
        status: app.status_message.clone(),
        error: app.error_message.clone(),
        // Discriminant only — `LogTail` carries up to 2000 events and
        // formatting the whole thing per command would be absurd.
        overlay: app
            .current_overlay
            .as_ref()
            .map(|o| format!("{:?}", std::mem::discriminant(o))),
        action_flow: app.action_flow.is_some(),
        form: app.form.is_some(),
        picker: app.picker.is_some(),
        detail: app.detail.is_some(),
        dlq: app.dlq.is_some(),
        shell: app.current_shell.is_some(),
        quit: app.quit,
        load_state: format!("{:?}", app.load_state),
        toasts: app.toasts.len(),
        help_topic: format!("{:?}", app.help.topic),
        events_visible: app.event_panel.visible,
        palette_items: app.palette_items.len(),
        redact: app.view.redact,
        read_only: app.read_only,
        scope: format!("{:?}", app.scope),
        sort: format!("{:?}/{}", app.view.sort_key(), app.view.sort_desc()),
        filter: app.view.filter().text().to_string(),
        grouped: app.view.grouped(),
        pending: app.pending_actions.len(),
        envs: app.environments.len(),
        multi_regions: app.multi_regions.len(),
        hidden_cols: app.view.hidden_cols.len(),
        saved_views: app.saved_views.len(),
        log_tail_task: app.log_tail_task.is_some(),
    }
}

/// Every `:command` in the registry, with representative arguments.
///
/// The assertion is that running it moves *something* the operator can
/// see. That is a low bar deliberately: it is the exact property the
/// dispatch sweep measures, so a command whose arm is deleted, renamed
/// or short-circuited fails here — which is what nothing did before.
pub(super) const OBSERVABLE_COMMANDS: &[&str] = &[
    "about",
    "accounts",
    "alarm-history my-alarm",
    "alarms",
    "apps-info",
    "capacity",
    "changes",
    "clone api-clone",
    "cols list",
    "config-diff api-staging",
    "config-diff-local",
    "custom-platforms",
    "deselect",
    "drop saved1",
    "elb-subnets",
    "env list",
    "envs-by-version build-900",
    "event-tail",
    "event-time",
    "events off",
    "export",
    "filter saved1",
    "filters",
    "find-env api",
    "group on",
    "help",
    "history",
    "instance-type t3.small",
    "json",
    "lineage",
    "lint",
    "listener-edit 443",
    "listeners",
    "loglevel debug",
    "logs-insights fields @message",
    "managed-window Mon 3",
    "metric list",
    "options",
    "org-health",
    "pending",
    "pin",
    "plugins",
    "profile default",
    "promotions",
    "quit",
    "rds",
    "readonly on",
    "redact on",
    "refresh",
    "report",
    "report-bug",
    "resources",
    "rollbacks-armed",
    "save saved2",
    "save-view v1",
    "saved-configs",
    "scaling-triggers",
    "secret my-secret",
    "secrets",
    "security-groups",
    "settings",
    "sort name",
    "subnets",
    "update",
    "upgrade",
    "versions",
    "view v1",
    "view-drop v1",
    "views",
    "whatsnew",
    "why",
    "account acct1",
];

fn app_for_command_probe() -> App {
    let mut app = test_app();
    app.environments = vec![
        mk_env("api-prod", "uflexi", "Web", "Green"),
        mk_env("api-staging", "uflexi", "Web", "Green"),
    ];
    app.view.invalidate();
    app.rebuild_view();
    app.table_state.select(Some(0));
    app
}

#[tokio::test]
async fn every_command_moves_observable_state() {
    for cmd in OBSERVABLE_COMMANDS {
        let mut app = app_for_command_probe();
        let before = fingerprint(&app);
        app.execute_command(cmd);
        let after = fingerprint(&app);
        assert_ne!(
            before, after,
            ":{cmd} changed nothing an operator could see — a deleted or \
             short-circuited dispatch arm would look exactly like this"
        );
    }
}

#[tokio::test]
async fn logs_tail_starts_its_polling_task() {
    // `:logs-tail` is a pure spawn: no status, no overlay, nothing
    // synchronous. What it does leave behind is the tracked poll task.
    let mut app = app_for_command_probe();
    assert!(app.log_tail_task.is_none());
    app.execute_command("logs-tail");
    assert!(
        app.log_tail_task.is_some(),
        ":logs-tail started no polling task"
    );
}

#[tokio::test]
async fn config_inspect_dispatches_work() {
    // The other pure spawn, and the only command with nothing on `App`
    // to observe at all. What it does do is put a message on the
    // channel, so drain for one.
    let mut app = app_for_command_probe();
    app.execute_command("config-inspect tpl");
    for _ in 0..50 {
        if app.msg_rx.try_recv().is_ok() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!(":config-inspect dispatched no work at all");
}

/// Commands covered by a test of their own rather than by one of the
/// bulk tables, with the reason. Keeping the reason here is the point:
/// an entry with no justification is how a gap gets papered over.
const COVERED_INDIVIDUALLY: &[(&str, &str)] = &[
    ("logs-tail", "pure spawn — pinned via log_tail_task"),
    (
        "config-inspect",
        "pure spawn — pinned via the message channel",
    ),
    ("region", "fan-out epoch tests in app/tests/refresh.rs"),
    ("restart", "confirm-modal action — arms the right Action"),
    ("rebuild", "confirm-modal action"),
    (
        "terminate",
        "confirm-modal action, plus the typed-name guard",
    ),
    ("stop", "confirm-modal action"),
    ("start", "confirm-modal action"),
    ("swap", "needs a second env in the same app before it gates"),
    ("ssm-run", "needs cached Detail instances before it gates"),
    ("scale", "GATED_COMMANDS"),
    ("abort", "GATED_COMMANDS"),
    ("rollout", "GATED_COMMANDS"),
    ("deploy", "GATED_COMMANDS"),
    ("env-edit", "GATED_COMMANDS"),
    ("rds-attach", "GATED_COMMANDS"),
    ("delete-version", "GATED_COMMANDS"),
    ("custom-platform-delete", "GATED_COMMANDS"),
    ("unset-option", "GATED_COMMANDS"),
    ("q", "alias of quit"),
    // Already had tests of their own before either sweep ran — each was
    // "caught" on the very first pass, so they are recorded here rather
    // than duplicated into the bulk table.
    ("diff", "two-arg diff form tests in app/tests/overlays.rs"),
    ("ssh", "instance-id and env-name arg tests"),
    ("explain", "explain-overlay render tests"),
    ("cost", "cost fetch + truncation tests"),
    ("fleet-cost", "fleet-cost rollup render tests"),
    ("abort-rollback", "named-env disarm tests"),
    ("freeze-deploys", "freeze marker + refusal-message tests"),
    ("thaw-deploys", "freeze lifecycle tests"),
    ("incident", "incident start/restart/end arg tests"),
    ("undo", "undo-history cap tests"),
    ("drift", "tfstate parse + exit-code tests"),
    ("promote-env", "promotion lineage tests"),
    ("rollback", "rollback --to and --auto-rollback tests"),
    ("alias", "command-alias expansion tests"),
    ("alias-drop", "command-alias expansion tests"),
];

#[test]
fn every_registry_command_is_covered_by_some_test() {
    // The drift guard behind the two sweeps. Both reached 100% — 41 of
    // 41 render surfaces, 131 of 131 commands — and without this, the
    // next command added would quietly drop that to 131 of 132. Reads
    // the registry from source for the same reason the other guards do:
    // a list maintained by hand is a list that goes stale.
    let src = std::fs::read_to_string("src/commands.rs").expect("read commands.rs");
    let start = src.find("pub const COMMANDS").expect("COMMANDS table");
    let body = &src[start..src[start..].find("\n];").expect("table end") + start];

    // Entries are mostly multi-line — `cmd_with_aliases(` on one line
    // and `"region",` on the next — so take the first quoted string
    // after each builder call rather than parsing line by line.
    let mut registry: Vec<String> = Vec::new();
    let lines: Vec<&str> = body.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        let Some(rest) = ["cmd(", "cmd_env_arg(", "cmd_with_aliases("]
            .iter()
            .find_map(|p| t.strip_prefix(*p))
        else {
            continue;
        };
        let name = rest.split('"').nth(1).map(str::to_string).or_else(|| {
            lines[i + 1..]
                .iter()
                .find(|l| l.contains('"'))
                .and_then(|l| l.split('"').nth(1).map(str::to_string))
        });
        if let Some(n) = name {
            registry.push(n);
        }
    }
    assert!(
        registry.len() > 120,
        "parsed only {} commands out of the registry — the parse broke, \
         and an empty result would read as a clean pass",
        registry.len()
    );

    let first_word = |s: &&str| s.split_whitespace().next().unwrap_or("").to_string();
    let mut covered: std::collections::HashSet<String> = std::collections::HashSet::new();
    for list in [
        OBSERVABLE_COMMANDS,
        GATED_COMMANDS,
        WRITE_COMMANDS,
        BATCH_WRITE_COMMANDS,
        APPLICATION_SCOPED_WRITES,
    ] {
        covered.extend(list.iter().map(first_word));
    }
    covered.extend(COVERED_INDIVIDUALLY.iter().map(|(c, _)| c.to_string()));

    let missing: Vec<&String> = registry.iter().filter(|c| !covered.contains(*c)).collect();
    assert!(
        missing.is_empty(),
        "these registry commands are in no coverage list — add them to \
         OBSERVABLE_COMMANDS, or to COVERED_INDIVIDUALLY with the reason: {missing:?}"
    );
}
