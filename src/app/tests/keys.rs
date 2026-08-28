//! The keymap: chords, modifiers, filter mode, multi-select.
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
fn parse_ignore_keys_splits_and_lowercases() {
    assert_eq!(crate::app::parse_ignore_keys(None), Vec::<String>::new());
    assert_eq!(
        crate::app::parse_ignore_keys(Some("")),
        Vec::<String>::new()
    );
    assert_eq!(
        crate::app::parse_ignore_keys(Some(" Version_Label , MinSize ,")),
        vec!["version_label".to_string(), "minsize".to_string()]
    );
}

#[test]
fn filter_config_diffs_supports_namespace_qualified_match() {
    // Operators can use `namespace:name` form to scope an ignore-
    // key to a specific namespace (so a generic "MinSize" ignore
    // doesn't drop both the ASG and the LB MinSize).
    let diffs = vec![
        crate::app::ConfigDiff {
            namespace: "aws:autoscaling:asg".into(),
            name: "MinSize".into(),
            left: Some("2".into()),
            right: Some("3".into()),
        },
        crate::app::ConfigDiff {
            namespace: "aws:elasticbeanstalk:command".into(),
            name: "MinSize".into(),
            left: Some("4".into()),
            right: Some("5".into()),
        },
    ];
    let keys = crate::app::parse_ignore_keys(Some("aws:autoscaling:asg:MinSize"));
    let filtered = crate::app::filter_config_diffs(diffs, &keys);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].namespace, "aws:elasticbeanstalk:command");
}

#[test]
fn filter_config_diffs_empty_ignore_keys_is_passthrough() {
    let diffs = vec![crate::app::ConfigDiff {
        namespace: "ns".into(),
        name: "MinSize".into(),
        left: Some("2".into()),
        right: Some("3".into()),
    }];
    let original_len = diffs.len();
    assert_eq!(
        crate::app::filter_config_diffs(diffs, &[]).len(),
        original_len
    );
}

#[test]
fn build_env_edit_body_sorts_keys_and_emits_header() {
    let vars = vec![
        ("LOG_LEVEL".into(), "info".into()),
        ("DB_HOST".into(), "db.example".into()),
        ("DB_PORT".into(), "5432".into()),
    ];
    let body = crate::app::build_env_edit_body("prod", &vars);
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
fn parse_env_edit_body_drops_invalid_keys() {
    let body = "= no-key\n KEY WITH SPACES=foo\nGOOD=val\n";
    let parsed = crate::app::parse_env_edit_body(body);
    assert_eq!(parsed.len(), 1);
    assert!(parsed.contains_key("GOOD"));
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
    let body = crate::app::render_options_overlay(&rows, None, "uflexi-prod");
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
    let body =
        crate::app::render_options_overlay(&rows, Some("aws:autoscaling:asg"), "uflexi-prod");
    assert!(body.contains("MinSize"));
    assert!(!body.contains("DeploymentPolicy"));
}

#[test]
fn delta_toast_key_extracts_bucket_for_delta_shapes() {
    assert_eq!(
        crate::app::delta_toast_key("▲2 Red").as_deref(),
        Some("Red")
    );
    assert_eq!(
        crate::app::delta_toast_key("▼1 Yellow").as_deref(),
        Some("Yellow")
    );
    // Leading whitespace is allowed.
    assert_eq!(
        crate::app::delta_toast_key("  ▲10 Green").as_deref(),
        Some("Green")
    );
}

#[test]
fn parse_s3_url_extracts_bucket_and_key() {
    let (b, k) = crate::app::parse_s3_url("s3://my-bucket/path/to/bundle.zip").unwrap();
    assert_eq!(b, "my-bucket");
    assert_eq!(k, "path/to/bundle.zip");
}

#[test]
fn delta_toast_key_returns_none_for_non_delta_text() {
    assert_eq!(crate::app::delta_toast_key("refreshing…"), None);
    assert_eq!(crate::app::delta_toast_key(""), None);
    assert_eq!(crate::app::delta_toast_key("▲"), None);
    // Arrow with no count.
    assert_eq!(crate::app::delta_toast_key("▲ Red"), None);
    // Arrow + count but no bucket word.
    assert_eq!(crate::app::delta_toast_key("▲5 "), None);
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
async fn apps_scope_space_toggles_apps_selected() {
    let mut app = test_app();
    // Seed two apps + select Apps scope.
    app.applications = vec![mk_application("billing"), mk_application("checkout")];
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
    app.applications = vec![mk_application("billing")];
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

#[tokio::test]
async fn the_control_socket_screen_op_reports_the_last_frame() {
    // `last_rendered_buffer` has exactly one reader — this — and the run
    // loop now only populates it when a control socket is attached.
    // Nothing covered either side of that, so a wrong condition would
    // have made `ebman ctl screen` return the placeholder forever in
    // silence.
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
    app.view.invalidate();
    app.rebuild_view();

    // No frame captured yet — say so rather than lying with a blank screen.
    assert!(
        app.screen_text().contains("no frame rendered yet"),
        "{}",
        app.screen_text()
    );

    // With a frame captured, the op returns its text.
    app.last_rendered_buffer = Some(render_buf(&mut app, 120, 20));
    let text = app.screen_text();
    assert!(
        text.contains("api-prod"),
        "the screenshot carries the rendered fleet:\n{text}"
    );
}

/// Every Ctrl chord in Normal mode must need its Ctrl.
///
/// These arms carry `if key.modifiers.contains(KeyModifiers::CONTROL)`,
/// and the 2026-08-28 sweep replaced those guards with `true` and with
/// `false` — nineteen mutants across the file, none caught. Most of
/// these characters have NO unguarded arm in this block, so the guard is
/// the only thing making the chord mean anything: stuck `true`, plain
/// `x` starts toggling redaction and plain `r` starts refreshing; stuck
/// `false`, the chord does nothing at all.
///
/// Table-driven, and both directions for each: the chord DOES the thing,
/// and the bare key does NOT. Asserting only the first passes a guard
/// stuck `true`; only the second passes one stuck `false`.
#[tokio::test]
async fn ctrl_chords_in_normal_mode_require_their_ctrl() {
    // (key, what it changes, read it back)
    #[allow(clippy::type_complexity)]
    let cases: Vec<(char, &str, Box<dyn Fn(&App) -> String>)> = vec![
        (
            'x',
            "redaction",
            Box::new(|a: &App| a.view.redact.to_string()),
        ),
        (
            'g',
            "grouping",
            Box::new(|a: &App| a.view.grouped().to_string()),
        ),
        (
            'e',
            "the events panel",
            Box::new(|a: &App| a.event_panel.visible.to_string()),
        ),
        (
            'd',
            "the view mode",
            Box::new(|a: &App| format!("{:?}", a.view.mode)),
        ),
        (
            'k',
            "the palette",
            Box::new(|a: &App| format!("{:?}", a.mode)),
        ),
    ];

    for (ch, what, read) in cases {
        let fresh = || {
            let mut app = test_app();
            app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
            app.rebuild_view();
            app.table_state.select(Some(0));
            app
        };

        let mut with_ctrl = fresh();
        let before = read(&with_ctrl);
        press(&mut with_ctrl, KeyCode::Char(ch), KeyModifiers::CONTROL);
        assert_ne!(
            read(&with_ctrl),
            before,
            "Ctrl-{ch} should change {what}; the guard may be stuck false"
        );

        let mut without = fresh();
        press(&mut without, KeyCode::Char(ch), KeyModifiers::NONE);
        assert_eq!(
            read(&without),
            before,
            "a bare `{ch}` changed {what} — the Ctrl guard is not holding, \
             so an ordinary keystroke performs a chord's action"
        );
    }
}
