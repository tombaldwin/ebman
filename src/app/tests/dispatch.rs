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

    // Deletions must cost. The recurrence is
    // `(prev[j+1] + 1).min(curr[j] + 1).min(prev[j] + cost)`, and
    // turning that first `+ 1` into `* 1` makes dropping a character
    // from the longer string free. None of the cases above notice —
    // their minimum comes from another term. A pure-suffix difference
    // does: every edit here is a deletion.
    assert_eq!(crate::app::edit_distance("abc", "abcdef"), 3);
    assert_eq!(crate::app::edit_distance("abcdef", "abc"), 3);
    assert_eq!(crate::app::edit_distance("a", "aaaa"), 3);

    // Transpositions cost TWO. Plain Levenshtein has no swap operation,
    // so `ab` → `ba` is a delete plus an insert.
    //
    // This is the only shape that notices `curr[j] + 1` becoming
    // `curr[j] * 1` — free insertion lets a swap be done in one step.
    // Every other pair above agrees under that mutation, including the
    // suffix cases added for the neighbouring `+`, which is why it
    // survived a run that killed 197 of 206. Transposed characters are
    // also one of the commonest real typos, and this function is what
    // `suggest_command` ranks with.
    assert_eq!(crate::app::edit_distance("ab", "ba"), 2);
    assert_eq!(crate::app::edit_distance("restart", "restrat"), 2);

    // Symmetric, in both argument orders. The implementation picks a
    // `short`/`long` pair purely to bound the DP row's memory, so which
    // one it picks must not change the answer.
    for (a, b) in [("kitten", "sitting"), ("abc", "abcdef"), ("", "xyz")] {
        assert_eq!(
            crate::app::edit_distance(a, b),
            crate::app::edit_distance(b, a),
            "edit_distance({a:?}, {b:?}) is not symmetric"
        );
    }
}

/// The tie-break, against a fixed candidate list.
///
/// `suggest_command` walks the live command registry, so testing this
/// through it would pin the registry's contents and ordering rather than
/// the selection rule, and break the next time a command is added.
/// `suggest_from` takes the names, so the two cases below are exact.
#[test]
fn suggest_from_keeps_the_first_of_equally_close_names() {
    // Both one edit from "ab", so the FIRST wins: the comparison is a
    // strict `d < best`. With `<=` the last equally-close name would
    // win instead, which makes the suggestion depend on registry order.
    assert_eq!(
        crate::app::suggest_from("ab", ["ac", "ad"]).as_deref(),
        Some("ac")
    );
    // A strictly better later candidate does replace the earlier one.
    // With `==` the comparison never improves on the first match found.
    assert_eq!(
        crate::app::suggest_from("ab", ["ac", "ab"]).as_deref(),
        Some("ab")
    );
    // Nothing within the threshold → no suggestion. Short inputs get a
    // threshold of 1, so a 2-edit name must not match.
    assert_eq!(crate::app::suggest_from("ab", ["xy"]), None);
    assert_eq!(crate::app::suggest_from("ab", []), None);
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
/// see. A low bar deliberately: it is the exact property the dispatch
/// sweep measures, so a command whose arm is short-circuited fails here.
///
/// It does **not** catch a *deleted* or renamed arm — that falls through
/// to the `other =>` catch-all, which sets "unknown command: …", so the
/// fingerprint moves and this test passes. Deletion is caught by
/// `every_registry_name_has_a_dispatch_arm` in `src/commands.rs`, which
/// scans the source. The two together cover both, and neither covers
/// both alone.
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
            ":{cmd} changed nothing an operator could see — a \
             short-circuited dispatch arm looks exactly like this"
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
    // Match on `const COMMANDS`, not `pub const COMMANDS`. The API
    // narrowing turned it into `pub(crate) const` and this guard broke —
    // a drift guard that itself drifts on an unrelated edit is worse than
    // none, because the failure looks like the thing it guards.
    let start = src
        .find("const COMMANDS")
        .expect("COMMANDS table in src/commands.rs");
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

    // `COVERED_INDIVIDUALLY` is a free-text promise that a test exists
    // somewhere. Check the cheapest thing that would be false if the
    // promise were fiction: the command name appears in the test tree at
    // all. It cannot prove the test is any good — but a name that
    // appears nowhere is a claim with nothing behind it, which is the
    // failure mode an honour-system list invites.
    let mut test_src = String::new();
    for entry in std::fs::read_dir("src/app/tests").expect("tests dir") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            test_src.push_str(&std::fs::read_to_string(&path).expect("read"));
        }
    }
    // Cut the list's own declaration out before searching. Without this
    // the check is self-satisfying: `COVERED_INDIVIDUALLY` lives in a
    // file under `src/app/tests`, so every entry "appears in the test
    // tree" by virtue of being written down. The first version of this
    // check had exactly that hole and passed a deliberately fictional
    // entry.
    if let Some(start) = test_src.find("const COVERED_INDIVIDUALLY") {
        if let Some(len) = test_src[start..].find("\n];") {
            test_src.replace_range(start..start + len, "");
        }
    }
    // Match the command as a WORD, not a substring. A plain
    // `contains` backed a fictional entry because the name occurred
    // inside a longer word somewhere in 9k lines of tests, and a
    // one-letter entry was backed by any occurrence of that letter.
    //
    // Residual limit, stated rather than pretended away: a name that
    // appears as a word anywhere in the test PROSE still counts as
    // backed. This check can only ask "is there any trace of this
    // name"; it cannot ask "is there a test that exercises it". That is
    // why COVERED_INDIVIDUALLY carries a written reason — the reason is
    // for a human, and this is the tripwire for an entry invented out
    // of nothing.
    let word_appears = |needle: &str| {
        test_src.match_indices(needle).any(|(i, _)| {
            let before = test_src[..i].chars().next_back();
            let after = test_src[i + needle.len()..].chars().next();
            let boundary =
                |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric() && c != '_' && c != '-');
            boundary(before) && boundary(after)
        })
    };
    let unbacked: Vec<&str> = COVERED_INDIVIDUALLY
        .iter()
        .filter(|(c, _)| !word_appears(c))
        .map(|(c, _)| *c)
        .collect();
    assert!(
        unbacked.is_empty(),
        "these commands claim individual coverage but their name appears \
         nowhere in src/app/tests — the claim has nothing behind it: {unbacked:?}"
    );

    let missing: Vec<&String> = registry.iter().filter(|c| !covered.contains(*c)).collect();
    assert!(
        missing.is_empty(),
        "these registry commands are in no coverage list — add them to \
         OBSERVABLE_COMMANDS, or to COVERED_INDIVIDUALLY with the reason: {missing:?}"
    );
}

#[test]
fn every_render_surface_is_accounted_for() {
    // The command side has `every_registry_command_is_covered_by_some_test`;
    // the render side had nothing, so a 42nd `draw_*` added next cycle
    // would silently take render coverage from 41 of 41 back to 41 of 42.
    // This is the missing half of that pair.
    //
    // It pins the SET of surfaces, not their coverage — proving each one
    // is exercised needs the stub-and-see sweep, which can't run inside
    // the suite. What it does is force the question: a new surface fails
    // here, and whoever adds it has to either cover it and add the name,
    // or say why not.
    let mut found: Vec<String> = Vec::new();
    let dir = std::path::Path::new("src/ui");
    let mut files = vec![std::path::PathBuf::from("src/ui.rs")];
    for entry in std::fs::read_dir(dir).expect("src/ui") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some("rs") && !is_test_source(&path) {
            files.push(path);
        }
    }
    for path in &files {
        let text = std::fs::read_to_string(path).expect("read");
        for line in text.lines() {
            let t = line
                .strip_prefix("pub(crate) ")
                .or_else(|| line.strip_prefix("pub(super) "))
                .or_else(|| line.strip_prefix("pub "))
                .unwrap_or(line);
            if let Some(rest) = t.strip_prefix("fn draw_") {
                let name = rest
                    .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .next()
                    .unwrap_or("");
                found.push(format!("draw_{name}"));
            }
        }
    }
    found.sort();
    found.dedup();

    // Guard the parse itself. An empty result would otherwise read as a
    // clean pass — the failure shape this repo has now hit four times.
    assert!(
        found.len() > 30,
        "parsed only {} render surfaces — the parse broke",
        found.len()
    );

    const KNOWN: usize = 41;
    assert_eq!(
        found.len(),
        KNOWN,
        "the set of `draw_*` surfaces changed ({} now, {KNOWN} when the \
         coverage sweep last ran and reached 41 of 41). If you added one, \
         cover it — stub it with an early return and check a test fails — \
         then bump KNOWN. If you removed one, just bump KNOWN.\nfound: {found:?}",
        found.len()
    );
}

/// String literals in `src` that embed a raw newline — the
/// wrapped-literal bug.
///
/// Uses a real lexer. Five hand-rolled scanners preceded this one and
/// each was wrong differently: a `//` inside a URL, `'"'` char literals,
/// the closing half of a `\`-continued literal reading as a fresh
/// opening quote, raw strings, and finally a runaway that flagged every
/// line after the first mistake. `proc_macro2` already lexes Rust
/// correctly and is already in the lock file; classifying its output is
/// the only part that needs writing.
pub(super) fn literals_with_embedded_newlines(src: &str) -> Vec<String> {
    fn walk(ts: proc_macro2::TokenStream, out: &mut Vec<String>) {
        for tree in ts {
            match tree {
                proc_macro2::TokenTree::Group(g) => walk(g.stream(), out),
                proc_macro2::TokenTree::Literal(l) => {
                    let text = l.to_string();
                    // Only plain string literals. Raw strings (`r"…"`,
                    // `r#"…"#`) are multi-line by design.
                    if !text.starts_with('"') {
                        continue;
                    }
                    // A `\` immediately before the newline is the
                    // continuation — that is the CORRECT form.
                    // A literal opening with `"\` + newline is the
                    // idiom for "pre-formatted block": `--help`, the
                    // WHATSNEW text, YAML fixtures. The author has
                    // declared the layout is deliberate, so every
                    // newline in it is too. Threshold tuning cannot
                    // separate those from the bug — `--help` aligns its
                    // options deeply — but this author signal can.
                    if text.starts_with("\"\\\n") {
                        continue;
                    }
                    const SOURCE_INDENT: usize = 2;
                    let chars: Vec<char> = text.chars().collect();
                    for (i, c) in chars.iter().enumerate() {
                        if *c != '\n' || (i > 0 && chars[i - 1] == '\\') {
                            continue;
                        }
                        let run = chars[i + 1..].iter().take_while(|c| **c == ' ').count();
                        if run >= SOURCE_INDENT {
                            let preview: String = text.chars().take(70).collect();
                            out.push(preview.replace('\n', "\\n"));
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let Ok(ts) = src.parse::<proc_macro2::TokenStream>() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk(ts, &mut out);
    out
}

#[test]
fn the_wrapped_literal_scanner_is_accurate() {
    // Every case here broke one of the five hand-rolled predecessors.
    let bad = "fn f() { let m = \"some text\n              more text\"; }";
    assert_eq!(
        literals_with_embedded_newlines(bad).len(),
        1,
        "the actual bug: a literal split with no continuation"
    );

    for ok in [
        "fn f() { let m = \"some text \\\n     more text\"; }", // `\` continuation
        "fn f() { let u = \"https://sqs.eu-west-2.amazonaws.com/1/q\"; }",
        "fn f() { out.push('\"'); }",
        "fn f() { let v = raw.trim_matches('\"'); }",
        "fn f() { /* a \"quote\" in a comment */ }",
        "fn f() { body.push_str(\"ENV        TARGET      DEADLINE\\n\"); }",
        "fn f() { let s = r\"raw\nmultiline\"; }", // raw strings are fine
    ] {
        assert!(
            literals_with_embedded_newlines(ok).is_empty(),
            "false positive on: {ok}"
        );
    }
}

#[test]
fn no_wrapped_string_literal_leaves_an_indentation_hole() {
    // A literal split across lines WITHOUT a trailing `\` embeds the
    // newline AND the next line's indentation, so the operator sees a
    // 20-space gap mid-sentence — and the status bar is one line, so a
    // narrow terminal pushes the rest off-screen.
    //
    // CLAUDE.md records this shipping twice. It had shipped a third
    // time, in main.rs's --control-socket length error, and an outside
    // reviewer found it rather than us: the only guard was
    // `assert_no_run_on_spaces`, called from two test sites, so it
    // caught the bug solely for messages someone remembered to route
    // through it.
    let mut offenders: Vec<String> = Vec::new();
    let mut stack = vec![std::path::PathBuf::from("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("src dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") || is_test_source(&path) {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read");
            for lit in literals_with_embedded_newlines(&text) {
                offenders.push(format!("{}: {lit}", path.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "string literals with an embedded newline — a wrapped literal \
         missing its `\\` continuation embeds the next line's indentation \
         too: {offenders:#?}"
    );
}

#[test]
fn terminal_restore_goes_through_the_best_effort_helper() {
    // `restore_terminal` cannot be unit-tested: calling it would
    // disable raw mode on the machine running the suite, and this
    // project's rule is that tests never touch the developer's terminal,
    // clipboard, config or cache. So the protection is structural
    // instead — nothing may re-create the sequence it replaced.
    //
    // That sequence was `disable_raw_mode()?` followed by a separate `?`
    // on `LeaveAlternateScreen`, written twice (main's `leave_tui` and
    // the `$EDITOR` hand-off). A failure in the first meant the second
    // never ran, leaving the operator on a dead alternate screen with
    // mouse capture on, typing `reset` blind.
    let mut offenders: Vec<String> = Vec::new();
    let mut stack = vec![std::path::PathBuf::from("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("src dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") || is_test_source(&path) {
                continue;
            }
            // `lib.rs` defines the helper; it is allowed to call it.
            if path.file_name().and_then(|f| f.to_str()) == Some("lib.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read");
            for (n, line) in text.lines().enumerate() {
                let code = super::scan::strip_line_comment(line);
                if code.contains("disable_raw_mode()?") {
                    offenders.push(format!("{}:{}", path.display(), n + 1));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "`disable_raw_mode()?` bails before the alternate screen is left \
         — use `ebman::restore_terminal`, which attempts every step: \
         {offenders:?}"
    );
}

#[test]
fn every_cached_index_is_checked() {
    // `ViewState`'s rows hold indices into `environments`, which
    // `ViewState` does not own — so a mutation that forgets
    // `view.invalidate()` leaves them pointing past the end. Indexing
    // unchecked there panics, in the alternate screen, which is the
    // exact outcome `assert_fresh`'s release-mode softening exists to
    // avoid: it chose "one wrong frame over a panic", and unchecked
    // indexing made the wrong frame BE the panic.
    //
    // `app/view.rs` is the one legitimate exception: `rebuild_view`
    // indexes a `filtered` list it computed itself, in the same
    // function, so those indices cannot be stale.
    let mut offenders: Vec<String> = Vec::new();
    let mut stack = vec![std::path::PathBuf::from("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("src dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") || is_test_source(&path) {
                continue;
            }
            if path.ends_with("app/view.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read");
            for (n, line) in text.lines().enumerate() {
                let code = super::scan::strip_line_comment(line);
                // `environments[…]` is always the fleet list. For the
                // shorter `envs`, only a DERFERENCED index is a cached
                // view index (`envs[*i]` from a `DisplayRow::Env(i)`) —
                // `envs[0]` on a locally-built vector is unrelated, and
                // matching it flagged six innocent sites in terraform
                // and the MCP server.
                if code.contains("environments[") || code.contains("envs[*") {
                    offenders.push(format!("{}:{}", path.display(), n + 1));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "unchecked index into the env list — a cached view index can \
         outlive a mutation of it, and this panics in the alt screen. \
         Use `.get()` / `App::env_at`: {offenders:?}"
    );
}

#[test]
fn the_test_suite_does_not_mutate_the_environment() {
    // `std::env::set_var` is process-global and `cargo test` is
    // parallel by default. Three tests used to mutate `HOME`,
    // `AWS_CONFIG_FILE` and `AWS_SHARED_CREDENTIALS_FILE`; one of them
    // never restored `HOME`, so every test that ran after it in the
    // same process saw `/tmp/fake-home` — and several production paths
    // read `HOME` live. One file serialised itself with a lock while
    // another mutated the same variable with no lock at all, under a
    // `// SAFETY: tests run single-threaded by default` comment that
    // was simply false.
    //
    // It is also `unsafe` under the 2024 env API and a hard error on
    // that edition, so this is a migration blocker as well as a flake.
    // The fix each time was to split the pure half out and pass the
    // value in; this stops the pattern coming back.
    let mut offenders: Vec<String> = Vec::new();
    let mut stack = vec![std::path::PathBuf::from("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("src dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read");
            for (n, line) in text.lines().enumerate() {
                let code = super::scan::strip_line_comment(line);
                // Needles assembled at runtime so this detector does
                // not match its own source — the first version flagged
                // exactly one offender: itself.
                let set = format!("env{}set_var", "::");
                let remove = format!("env{}remove_var", "::");
                if code.contains(&set) || code.contains(&remove) {
                    offenders.push(format!("{}:{}", path.display(), n + 1));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "`set_var` / `remove_var` are process-global and the suite runs \
         in parallel — split the pure half out and pass the value in \
         instead: {offenders:?}"
    );
}

/// Docs drift guards for the two release-checklist items that were still
/// manual.
///
/// `CLAUDE.md`'s release procedure step 1 mandates hand-walking
/// `docs/commands.md`, `keys.md`, `configuration.md` and `headless.md`
/// before tagging. One third of that is already automated —
/// `command_names_cited_in_prose_actually_exist` has caught two real
/// gaps (`:alarm-add`, `:env-vars`). These are the other two, and they
/// are the same shape: a flat `match` in source that a doc file is
/// supposed to mirror.
///
/// A manual checklist that has already failed twice is a backlog entry,
/// not a gate. Both pass today, so they cost nothing now and catch the
/// next addition at commit time instead of at release time.
mod docs_drift {
    /// `server.json`'s version must match the crate's.
    ///
    /// The release workflow rewrites this field from the tag before
    /// publishing, so a stale value never reaches the MCP Registry — which
    /// is exactly why it drifted to `0.32.0` while the crate was at
    /// `0.34.2` and nothing complained. It is still a checked-in file that
    /// reads as authoritative to anyone opening the repo, and the release
    /// procedure already bumps `Cargo.toml`.
    #[test]
    fn server_json_version_matches_the_crate() {
        let raw = std::fs::read_to_string("server.json").expect("read server.json");
        let crate_version = env!("CARGO_PKG_VERSION");

        // Deliberately not a JSON dependency for one field: the shape is
        // `"version": "x.y.z"` and the file is ours.
        let versions: Vec<&str> = raw
            .lines()
            .filter_map(|l| l.trim().strip_prefix("\"version\":"))
            .map(|v| v.trim().trim_matches(|c| c == '"' || c == ',').trim())
            .collect();

        assert!(
            !versions.is_empty(),
            "no `version` field found in server.json — this guard is \
             looking at the wrong shape"
        );
        for v in &versions {
            assert_eq!(
                *v, crate_version,
                "server.json declares version {v:?} but the crate is \
                 {crate_version:?}. Bump server.json alongside Cargo.toml."
            );
        }
    }

    /// Every key `config::parse` accepts must appear in
    /// `docs/configuration.md`. An operator cannot use a key they cannot
    /// find, and a key that silently exists is indistinguishable from a
    /// typo they got wrong.
    #[test]
    fn every_config_key_is_documented() {
        let src = std::fs::read_to_string("src/config.rs").expect("read config.rs");
        let docs = std::fs::read_to_string("docs/configuration.md").expect("read configuration.md");

        let mut keys: Vec<String> = Vec::new();
        for line in src.lines() {
            let t = line.trim();
            // The parser is a flat `match key { "name" => … }`.
            if let Some(rest) = t.strip_prefix('"') {
                if let Some((name, tail)) = rest.split_once('"') {
                    if tail.trim_start().starts_with("=>")
                        && !name.is_empty()
                        && name
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c == '_' || c == '.')
                    {
                        keys.push(name.to_string());
                    }
                }
            }
        }
        keys.sort();
        keys.dedup();
        assert!(
            keys.len() > 15,
            "found only {} config keys — the extractor is broken, and a guard \
             over nothing passes vacuously: {keys:?}",
            keys.len()
        );

        let missing: Vec<&String> = keys.iter().filter(|k| !docs.contains(*k)).collect();
        assert!(
            missing.is_empty(),
            "config keys accepted by the parser but absent from \
             docs/configuration.md: {missing:?}"
        );
    }

    /// Every subcommand `cli::SUBCOMMANDS` advertises must appear in
    /// `docs/headless.md`, which is the reference for anything scripting
    /// against ebman.
    #[test]
    fn every_subcommand_is_documented() {
        let src = std::fs::read_to_string("src/cli/mod.rs").expect("read cli/mod.rs");
        let docs = std::fs::read_to_string("docs/headless.md").expect("read headless.md");

        let start = src
            .find("SUBCOMMANDS")
            .expect("SUBCOMMANDS const in src/cli/mod.rs");
        let body = &src[start..start + src[start..].find("];").expect("const end")];
        let subs: Vec<String> = body
            .split('"')
            .skip(1)
            .step_by(2)
            .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase()))
            .map(str::to_string)
            .collect();

        assert!(
            subs.len() >= 8,
            "found only {} subcommands — extractor broken: {subs:?}",
            subs.len()
        );
        let missing: Vec<&String> = subs
            .iter()
            .filter(|s| !docs.contains(&format!("ebman {s}")))
            .collect();
        assert!(
            missing.is_empty(),
            "subcommands advertised by the CLI but absent from \
             docs/headless.md: {missing:?}"
        );
    }
}

/// Gaps found by `cargo mutants --in-diff` on the 0.34.0 lineup.
///
/// Each of these survived the whole suite, which means the behaviour was
/// asserted nowhere. They are grouped because they share a cause: the
/// confirm-modal flow was tested for the paths that *do* something
/// visible, and not for the deliberate exclusions and the routing.
mod mutants_found_these {
    use super::*;

    /// Deleting the `Action::AbortUpdate` arm from `advance_action_flow`
    /// survived: nothing asserted that `:abort` opens a confirm modal
    /// at all. It would silently do nothing.
    #[tokio::test]
    async fn abort_opens_a_confirm_modal() {
        let mut app = test_app();
        app.environments = vec![mk_env("api-prod", "uflexi", "WebServer", "Updating")];
        app.rebuild_view();
        app.table_state.select(Some(0));

        app.execute_command("abort");

        let Some(crate::app::ActionFlow::Confirm(modal)) = &app.action_flow else {
            panic!("`:abort` must open a confirm modal, got {:?}", app.mode);
        };
        assert_eq!(modal.action, Action::AbortUpdate);
        assert_eq!(modal.target_env, "api-prod");
    }

    /// `loading_lint: !self.demo_mode && action != Action::SsmRun`.
    /// Flipping the `&&` to `||` survived, so neither exclusion was
    /// pinned — and both are deliberate. Running an ad-hoc shell command
    /// is not gated by EB-config-health rules, and demo mode makes no
    /// AWS calls at all.
    #[tokio::test]
    async fn the_confirm_lint_probe_skips_ssm_run_and_demo_mode() {
        // SsmRun: excluded because the rules do not apply to it.
        let mut app = test_app();
        app.environments = vec![mk_env("api-prod", "uflexi", "WebServer", "Green")];
        app.rebuild_view();
        app.table_state.select(Some(0));
        app.open_parameterised_action(
            Action::SsmRun,
            crate::app::ParameterisedAction {
                ssm_run_command: Some("uptime".into()),
                ssm_run_instances: Some(vec!["i-1".into()]),
                ..Default::default()
            },
        );
        let Some(crate::app::ActionFlow::Confirm(modal)) = &app.action_flow else {
            panic!("ssm-run should open a confirm modal");
        };
        assert!(
            !modal.loading_lint,
            "an ad-hoc shell command is not gated by EB-config-health rules"
        );

        // Demo mode never gets this far, which is worth pinning because
        // it makes the `!self.demo_mode` half of that condition dead
        // defensive code: `deny_write` refuses every write in demo mode,
        // and `open_parameterised_action_on` consults it before building
        // the modal. Belt and braces rather than a bug — but the belt is
        // what actually holds.
        let mut demo = test_app();
        demo.demo_mode = true;
        demo.environments = vec![mk_env("api-prod", "uflexi", "WebServer", "Green")];
        demo.rebuild_view();
        demo.table_state.select(Some(0));
        demo.open_parameterised_action(Action::Rebuild, Default::default());
        assert!(
            demo.action_flow.is_none(),
            "demo mode must refuse the write outright, not open a modal"
        );

        // And the normal path DOES arm it — or the two asserts above pass
        // vacuously against a field that is never true.
        let mut real = test_app();
        real.environments = vec![mk_env("api-prod", "uflexi", "WebServer", "Green")];
        real.rebuild_view();
        real.table_state.select(Some(0));
        real.open_parameterised_action(Action::Rebuild, Default::default());
        let Some(crate::app::ActionFlow::Confirm(modal)) = &real.action_flow else {
            panic!("rebuild should open a confirm modal");
        };
        assert!(
            modal.loading_lint,
            "a real rebuild SHOULD arm the lint probe — without this the \
             exclusions above prove nothing"
        );
    }
}

/// Every `append_rollout` call must pass a profile.
///
/// The 0.34.2 review verified all four call sites pass the RIGHT value —
/// the profile the client was built from — but mutation showed nothing
/// holds them there: replacing `self.context.profile.as_deref()` with
/// `None` in `spawn_rollout.rs` left the whole suite green.
///
/// Rollout is the only command taking `--profile`, is multi-region by
/// construction, and its audit lines are the record of which account a
/// fleet-wide deploy landed in. A `None` here renders `profile=-`, which
/// is the state the field was added to remove.
///
/// Source-scanned in the shape of
/// `every_spawn_declares_whether_it_is_per_env`: the call sites are in
/// spawned async paths that need AWS, so this is the reachable check.
#[test]
fn every_rollout_audit_call_passes_a_profile() {
    let mut offenders: Vec<String> = Vec::new();
    for (path, text) in crate::app::tests::scan::source_files() {
        if crate::app::tests::scan::is_test_path(&path) || path.ends_with("audit.rs") {
            continue;
        }
        // `append_rollout(` then its next non-empty argument line.
        let lines: Vec<&str> = text.lines().collect();
        for (i, l) in lines.iter().enumerate() {
            if !crate::app::tests::scan::strip_line_comment(l).contains("append_rollout(") {
                continue;
            }
            // Second argument is `profile`; scan the next few lines for
            // a bare `None` in that position.
            let window = lines[i..(i + 4).min(lines.len())].join(" ");
            if window.contains("None,") && !window.contains("profile") {
                offenders.push(format!("{path}:{}", i + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "these `append_rollout` calls pass no profile, so their audit lines \
         read `profile=-` — the state the field exists to remove: {offenders:?}"
    );
}

// ── mutation-sweep triage, 2026-08-26 ────────────────────────────────
//
// `app/action_flow.rs` had 69 reachable survivors — the confirm-modal →
// undo-window → dispatch path for destructive actions, so the
// highest-consequence logic left in the sweep.

/// The undo window has to actually hold the dispatch back.
///
/// `tick_pending_dispatch_fires_after_deadline` covers only the elapsed
/// direction, so `if now < pd.deadline` was interchangeable with
/// `now == pd.deadline` — under which the dispatch fires on the very
/// next tick and the operator's cancel window does not exist. That is
/// the whole point of the feature, and nothing tested it.
#[tokio::test]
async fn tick_pending_dispatch_holds_until_the_deadline() {
    let mut app = test_app();
    let modal = mk_modal(Action::Rebuild, "waiting");
    app.pending_dispatch = Some(PendingDispatch {
        deadline: std::time::Instant::now() + Duration::from_secs(30),
        label: "Rebuild env".into(),
        target: "waiting".into(),
        kind: PendingDispatchKind::Single { modal },
    });

    // Several ticks, all well inside the window.
    for _ in 0..3 {
        app.tick_pending_dispatch();
        assert!(
            app.pending_dispatch.is_some(),
            "the dispatch fired before its cancel window elapsed"
        );
    }
}

/// `push_pending` caps the panel at `PENDING_CAP`, dropping the oldest.
/// `len() >= CAP` flipped to `<` pops on every push instead, so the
/// panel never holds more than one row.
#[tokio::test]
async fn push_pending_caps_the_panel_and_drops_the_oldest() {
    let mut app = test_app();
    let cap = crate::app::PENDING_CAP;
    for i in 0..cap {
        app.push_pending(format!("Action{i}"), format!("env{i}"));
    }
    assert_eq!(app.pending_actions.len(), cap, "fills to the cap");
    assert_eq!(app.pending_actions.front().unwrap().label, "Action0");

    // One more evicts the oldest, and only the oldest.
    app.push_pending("Overflow", "envN");
    assert_eq!(app.pending_actions.len(), cap, "stays at the cap");
    assert_eq!(
        app.pending_actions.front().unwrap().label,
        "Action1",
        "the oldest row is the one dropped"
    );
    assert_eq!(app.pending_actions.back().unwrap().label, "Overflow");
}

/// `complete_pending` matches on *unfinished* AND label AND target.
/// Four survivors sat on that predicate, so each conjunct needs a case
/// where it alone is what rejects the row.
#[tokio::test]
async fn complete_pending_matches_on_all_three_conditions() {
    let seed = || {
        let mut app = test_app();
        app.push_pending("Restart", "api-prod");
        app
    };
    let done = |app: &App| app.pending_actions[0].completed.is_some();

    // Happy path.
    let mut app = seed();
    app.complete_pending("Restart", "api-prod", Ok(()));
    assert!(done(&app), "the matching row completes");

    // Wrong label.
    let mut app = seed();
    app.complete_pending("Rebuild", "api-prod", Ok(()));
    assert!(!done(&app), "a different action must not complete this row");

    // Wrong target — the case that matters most, since the same action
    // against a different env is the realistic collision.
    let mut app = seed();
    app.complete_pending("Restart", "worker-prod", Ok(()));
    assert!(!done(&app), "a different env must not complete this row");

    // Already finished: a second result must not re-stamp it. Two rows,
    // the first already complete, so the second is the one to take it.
    let mut app = seed();
    app.push_pending("Restart", "api-prod");
    app.complete_pending("Restart", "api-prod", Ok(()));
    app.complete_pending("Restart", "api-prod", Err("second".into()));
    assert!(
        app.pending_actions[0].completed.as_ref().unwrap().1.is_ok(),
        "the first row keeps its original outcome"
    );
    assert!(
        app.pending_actions[1].completed.is_some(),
        "the second result lands on the second row"
    );
}

/// Completed rows linger for `PENDING_COMPLETED_TTL`, then go. Both
/// directions, because keeping everything and dropping everything each
/// pass a one-sided test.
#[tokio::test]
async fn expire_pending_drops_only_rows_past_the_ttl() {
    let mut app = test_app();
    app.push_pending("Old", "env-old");
    app.push_pending("Fresh", "env-fresh");
    app.push_pending("Running", "env-running");

    let ttl = crate::app::PENDING_COMPLETED_TTL;
    let now = std::time::Instant::now();
    app.pending_actions[0].completed = Some((now - ttl - Duration::from_secs(1), Ok(())));
    app.pending_actions[1].completed = Some((now, Ok(())));
    // [2] stays in flight — `completed: None` must never be expired.

    app.expire_pending();
    let labels: Vec<&str> = app
        .pending_actions
        .iter()
        .map(|e| e.label.as_str())
        .collect();
    assert_eq!(
        labels,
        vec!["Fresh", "Running"],
        "only the row past its TTL is dropped"
    );
}

/// Every action the menu offers has its own arm in `advance_action_flow`,
/// and the sweep found six of them individually deletable — Rebuild,
/// Deploy, UpgradePlatform, Clone, Scale and Capacity. Deleting one drops
/// it into the catch-all, so the menu entry silently does the wrong
/// thing (or nothing) while every other action still works.
///
/// The parameterised ones are distinguishable by what they pre-fill into
/// the command bar, which is exactly what the operator then types into.
#[tokio::test]
async fn every_menu_action_advances_to_its_own_next_step() {
    use crate::app::{Action, ActionFlow};

    let open_on_env = || {
        let mut app = test_app();
        app.environments = vec![mk_env("api-prod", "shop", "WebServer", "Green")];
        app.rebuild_view();
        app.table_state.select(Some(0));
        assert!(app.open_action_menu(), "the env is writable");
        app
    };

    // Command-bar hand-offs: each closes the menu and pre-fills its own
    // prefix. A deleted arm leaves the prefix empty or wrong.
    for (action, prefix) in [
        (Action::Deploy, "deploy "),
        (Action::UpgradePlatform, "upgrade "),
        (Action::Clone, "clone "),
        (Action::Scale, "scale "),
    ] {
        let mut app = open_on_env();
        app.advance_action_flow(action);
        assert_eq!(
            app.mode,
            crate::app::Mode::Command,
            "{action:?} hands off to the command bar"
        );
        assert_eq!(app.command_input.text(), prefix, "{action:?} pre-fill");
        assert!(
            app.action_flow.is_none(),
            "{action:?} closes the menu behind it"
        );
        assert!(
            app.status_message.is_some(),
            "{action:?} tells the operator what to type"
        );
    }

    // Rebuild takes no arguments, so it goes straight to the confirm
    // modal rather than the command bar.
    let mut app = open_on_env();
    app.advance_action_flow(Action::Rebuild);
    assert!(
        matches!(app.action_flow, Some(ActionFlow::Confirm(_))),
        "Rebuild opens a confirm modal, not the command bar: {:?}",
        app.action_flow.is_some()
    );
    assert_ne!(app.mode, crate::app::Mode::Command);

    // Capacity opens the pre-filled form instead.
    let mut app = open_on_env();
    app.advance_action_flow(Action::Capacity);
    assert!(app.form.is_some(), "Capacity opens the capacity form");
    assert!(app.action_flow.is_none(), "and closes the menu");
}

/// Every key that answers a destructive Y/N confirm.
///
/// The sweep found each of these arms individually deletable —
/// `y`, `Enter`, `n`, `Esc` and `q` — which is to say nothing checked
/// that answering the modal does anything at all. The `n` and `Esc`
/// cases matter most: a deleted arm falls into the catch-all, so the
/// modal simply ignores the keypress and stays open over a destructive
/// action the operator has just tried to back out of.
#[tokio::test]
async fn a_yes_no_confirm_answers_to_every_documented_key() {
    use crate::app::{ActionFlow, ConfirmKind};

    let armed = || {
        let mut app = test_app();
        app.environments = vec![mk_env("api-prod", "shop", "WebServer", "Green")];
        app.rebuild_view();
        app.table_state.select(Some(0));
        app.mode = crate::app::Mode::Action;
        app.action_flow = Some(ActionFlow::Confirm(mk_modal(Action::Rebuild, "api-prod")));
        app
    };

    // Confirming queues the dispatch behind the cancel window.
    for confirm in [KeyCode::Char('y'), KeyCode::Enter] {
        let mut app = armed();
        press(&mut app, confirm, KeyModifiers::NONE);
        assert!(app.action_flow.is_none(), "{confirm:?} closes the modal");
        assert!(
            app.pending_dispatch.is_some(),
            "{confirm:?} must queue the dispatch — a deleted arm leaves \
             the modal doing nothing"
        );
    }

    // Declining closes it and queues NOTHING. This is the direction that
    // has to work: an ignored `n` leaves a destructive confirm on screen.
    for decline in [KeyCode::Char('n'), KeyCode::Esc, KeyCode::Char('q')] {
        let mut app = armed();
        press(&mut app, decline, KeyModifiers::NONE);
        assert!(app.action_flow.is_none(), "{decline:?} closes the modal");
        assert!(
            app.pending_dispatch.is_none(),
            "{decline:?} must NOT dispatch anything"
        );
    }

    // `q` is deliberately NOT bound on type-the-name confirms, because
    // the operator is typing an env name and `q` may be part of it.
    let mut app = armed();
    if let Some(ActionFlow::Confirm(m)) = app.action_flow.as_mut() {
        m.kind = ConfirmKind::TypeName;
    }
    press(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
    assert!(
        app.action_flow.is_some(),
        "`q` must not cancel a type-the-name confirm — it is a character \
         in the env name being typed"
    );
}

/// The action menu's cursor wraps in both directions.
///
/// `(cur + 1) % n` and `(cur + n - 1) % n` carried ten survivors between
/// them — every operator on both expressions — because nothing walked
/// the cursor off either end.
#[tokio::test]
async fn the_action_menu_cursor_wraps_at_both_ends() {
    use crate::app::{ActionFlow, ACTIONS};

    let selected = |app: &App| match app.action_flow.as_ref() {
        Some(ActionFlow::Menu { list_state }) => list_state.selected(),
        _ => panic!("the menu should still be open"),
    };

    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "shop", "WebServer", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    assert!(app.open_action_menu());
    app.mode = crate::app::Mode::Action;

    let n = ACTIONS.len();
    assert!(n > 2, "the wrap test needs more than two entries");
    assert_eq!(selected(&app), Some(0), "opens on the first entry");

    // Down through the whole list and one past the end.
    for expected in 1..n {
        press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(selected(&app), Some(expected));
    }
    press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
    assert_eq!(selected(&app), Some(0), "j wraps past the last entry");

    // And back off the front.
    press(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
    assert_eq!(
        selected(&app),
        Some(n - 1),
        "k wraps past the first entry to the last"
    );
    press(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
    assert_eq!(selected(&app), Some(n - 2));

    // The arrow keys are the same arms.
    press(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(selected(&app), Some(n - 1));
    press(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(selected(&app), Some(n - 2));

    // Esc closes the menu.
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.action_flow.is_none(), "Esc closes the action menu");
}

// ── mutation-sweep triage, 2026-08-26 ────────────────────────────────
//
// `app/forms.rs` had 56 reachable survivors, 52 of them in
// `handle_form_key` — the same shape as the action menu: field
// navigation with wrap-around arithmetic, plus per-field-kind input
// rules that nothing exercised.

fn a_form(fields: Vec<crate::form::FormField>) -> crate::form::Form {
    crate::form::Form {
        title: "Test".into(),
        fields,
        cursor: 0,
        state: crate::form::FormState::Ready,
        submit: crate::form::FormSubmit::OptionSettings { mappings: vec![] },
        summary: "test".into(),
        env_name: "api-prod".into(),
        banner: String::new(),
        scroll: 0,
    }
}

fn form_with_three_text_fields() -> crate::form::Form {
    a_form(vec![
        crate::form::FormField::text("a", "A", None::<String>),
        crate::form::FormField::text("b", "B", None::<String>),
        crate::form::FormField::text("c", "C", None::<String>),
    ])
}

/// Tab and Shift-Tab walk the fields, and wrap at both ends.
#[tokio::test]
async fn form_field_navigation_wraps_both_ways() {
    let mut app = test_app();
    app.form = Some(form_with_three_text_fields());
    app.mode = crate::app::Mode::Form;
    let cur = |app: &App| app.form.as_ref().unwrap().cursor;

    assert_eq!(cur(&app), 0);
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(cur(&app), 1, "Tab moves forward");
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(cur(&app), 2);
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(cur(&app), 0, "Tab wraps past the last field");

    press(&mut app, KeyCode::BackTab, KeyModifiers::NONE);
    assert_eq!(cur(&app), 2, "Shift-Tab wraps past the first field");
    press(&mut app, KeyCode::BackTab, KeyModifiers::NONE);
    assert_eq!(cur(&app), 1, "Shift-Tab moves backward");

    // Up/Down are the same movement on a non-MultiSelect field, and the
    // directions must not be swapped.
    press(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(cur(&app), 2, "Down is forward");
    press(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(cur(&app), 1, "Up is backward");
}

/// An Integer field takes digits, and a minus only where a minus can
/// legally go. `is_ascii_digit() || (c == '-' && value.is_empty())`
/// flipped to `&&` accepts nothing at all.
#[tokio::test]
async fn an_integer_field_accepts_digits_and_a_leading_minus_only() {
    let typed = |keys: &str| {
        let mut app = test_app();
        app.form = Some(a_form(vec![crate::form::FormField::integer(
            "n",
            "N",
            None::<String>,
            None,
            None,
            true,
        )]));
        app.mode = crate::app::Mode::Form;
        for c in keys.chars() {
            press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        }
        app.form.unwrap().fields[0].value.clone()
    };

    assert_eq!(typed("42"), "42", "digits go in");
    assert_eq!(typed("-5"), "-5", "a leading minus is allowed");
    assert_eq!(typed("4-2"), "42", "a minus mid-number is not");
    assert_eq!(typed("abc"), "", "letters are rejected outright");
    assert_eq!(typed("1a2"), "12", "and rejected in the middle too");

    // Backspace pops, and its arm was separately deletable.
    let mut app = test_app();
    app.form = Some(a_form(vec![crate::form::FormField::integer(
        "n",
        "N",
        None::<String>,
        None,
        None,
        true,
    )]));
    app.mode = crate::app::Mode::Form;
    for c in "123".chars() {
        press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(app.form.unwrap().fields[0].value, "12", "Backspace pops");
}

/// A MultiSelect field takes Up/Down for its own option cursor rather
/// than moving between fields, and wraps. Ten survivors sat on the
/// `((cur + delta) % n + n) % n` expression.
#[tokio::test]
async fn multi_select_up_down_moves_the_option_cursor_and_wraps() {
    let mut app = test_app();
    app.form = Some(a_form(vec![
        crate::form::FormField::multi_select(
            "subnets",
            "Subnets",
            vec!["a".into(), "b".into(), "c".into()],
            vec![],
            None::<String>,
        ),
        crate::form::FormField::text("other", "Other", None::<String>),
    ]));
    app.mode = crate::app::Mode::Form;

    let opt = |app: &App| app.form.as_ref().unwrap().fields[0].option_cursor;
    let field = |app: &App| app.form.as_ref().unwrap().cursor;

    assert_eq!((field(&app), opt(&app)), (0, 0));
    press(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(
        (field(&app), opt(&app)),
        (0, 1),
        "Down moves the option cursor, NOT between fields"
    );
    press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
    assert_eq!(opt(&app), 2);
    press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
    assert_eq!(opt(&app), 0, "the option cursor wraps forward");
    press(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
    assert_eq!(opt(&app), 2, "and backward");

    // Tab still leaves the field, which is the distinction the `is_multi`
    // guard exists to preserve.
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(field(&app), 1, "Tab still moves between fields");
}
