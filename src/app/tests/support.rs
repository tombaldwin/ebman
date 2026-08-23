//! Shared fixtures: `test_app`, `mk_env`, the render harness
//! (`render` / `render_buf` / `find_row` / `row_has_fg`) and the
//! write-command tables the safety tests iterate.
//!
//! Split out of the 9,515-line `app/tests.rs`. Bodies moved
//! unchanged apart from one rewrite: `super::` meant `crate::app` in
//! the flat file and would mean `crate::app::tests` here, so every
//! explicit `super::` path was re-anchored (rustfmt reflowed some
//! lines as a result, since the new path is longer).

use super::super::*;
pub(super) use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

pub(super) fn fake_env_with(
    name: &str,
    status: &str,
    health: &str,
    updated_minutes_ago: Option<i64>,
) -> Environment {
    let updated = updated_minutes_ago.map(|m| chrono::Utc::now() - chrono::Duration::minutes(m));
    Environment {
        name: name.into(),
        application: "app".into(),
        status: status.into(),
        health: health.into(),
        platform: "Java 17".into(),
        solution_stack: String::new(),
        tier: "Web".into(),
        cname: "x.elb".into(),
        version_label: "v1".into(),
        arn: None,
        updated,
        id: None,
        region: None,
    }
}

pub(super) fn opt(
    ns: &str,
    name: &str,
    value: Option<&str>,
    default: Option<&str>,
) -> crate::aws::ConfigOption {
    crate::aws::ConfigOption {
        namespace: ns.into(),
        name: name.into(),
        value: value.map(String::from),
        default_value: default.map(String::from),
        value_type: "Scalar".into(),
        value_options: vec![],
        change_severity: None,
        user_defined: Some(true),
        min_value: None,
        max_value: None,
        max_length: None,
    }
}

pub(super) fn empty_resources() -> crate::aws::EnvResources {
    crate::aws::EnvResources::default()
}

pub(super) fn make_event(msg: &str) -> crate::aws::Event {
    crate::aws::Event {
        at: Some(chrono::Utc::now()),
        env: "uflexi-prod".into(),
        application: "uflexi".into(),
        message: msg.into(),
        severity: "INFO".into(),
        version_label: None,
    }
}

pub(super) fn fake_env(name: &str, status: &str, health: &str, version: &str) -> Environment {
    Environment {
        name: name.into(),
        application: "my-app".into(),
        status: status.into(),
        health: health.into(),
        platform: "Java 17".into(),
        solution_stack: String::new(),
        tier: "Web".into(),
        cname: format!("{name}.elb.amazonaws.com"),
        version_label: version.into(),
        arn: None,
        updated: None,
        id: None,
        region: None,
    }
}

/// Build a minimal App in a deterministic state. Useful for tests
/// that don't care about real AWS data — just keyboard flow + mode
/// transitions. Seed envs / overlays / detail state by mutating
/// the returned App directly.
pub(super) fn test_app() -> App {
    // Match the unicode/dark defaults so the renderer's per-theme
    // branches are exercised on the common path.
    let cfg = crate::config::Config {
        theme: "dark".into(),
        icons: "unicode".into(),
        ..crate::config::Config::default()
    };
    App::for_tests(crate::aws::AwsClient::stub(), cfg)
}

/// Synthesize a `KeyEvent::Press` and dispatch it through
/// `handle_event`. Mirrors how `run()` feeds real terminal events.
pub(super) fn press(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    app.handle_event(Event::Key(KeyEvent {
        code,
        modifiers: mods,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    }));
}

/// Render the App into a fixed-size `TestBackend` buffer and return
/// the flattened string (one row per line, joined with `\n`).
/// Useful for grep-style assertions on rendered output.
pub(super) fn render(app: &mut App, w: u16, h: u16) -> String {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal.draw(|f| crate::ui::draw(f, app)).expect("draw");
    let buf = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

/// Render the App into a `TestBackend` and return the raw `Buffer`,
/// so tests can assert on *style* (fg/bg/modifier per cell), not just
/// the flattened symbols `render` gives. Pairs with `find_row` /
/// `row_has_fg` below for grep-then-check-colour assertions.
pub(super) fn render_buf(app: &mut App, w: u16, h: u16) -> ratatui::buffer::Buffer {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal.draw(|f| crate::ui::draw(f, app)).expect("draw");
    terminal.backend().buffer().clone()
}

/// First row (y) whose flattened symbols contain `needle`, if any.
pub(super) fn find_row(buf: &ratatui::buffer::Buffer, needle: &str) -> Option<u16> {
    (0..buf.area.height).find(|&y| {
        let mut row = String::new();
        for x in 0..buf.area.width {
            row.push_str(buf[(x, y)].symbol());
        }
        row.contains(needle)
    })
}

/// Whether any cell in row `y` is painted with foreground `color`.
pub(super) fn row_has_fg(
    buf: &ratatui::buffer::Buffer,
    y: u16,
    color: ratatui::style::Color,
) -> bool {
    (0..buf.area.width).any(|x| buf[(x, y)].fg == color)
}

/// Cells in row `y` whose symbol == `sym` and foreground == `fg`.
pub(super) fn count_symbol_fg(
    buf: &ratatui::buffer::Buffer,
    y: u16,
    sym: &str,
    fg: ratatui::style::Color,
) -> usize {
    (0..buf.area.width)
        .filter(|&x| buf[(x, y)].symbol() == sym && buf[(x, y)].fg == fg)
        .count()
}

/// Total cells painted with foreground `color` — for differential
/// assertions ("match adds N green cells vs no-match") that ignore
/// constant chrome (header/footer) painted in the same colour.
pub(super) fn count_fg(buf: &ratatui::buffer::Buffer, color: ratatui::style::Color) -> usize {
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .filter(|&x| buf[(x, y)].fg == color)
                .count()
        })
        .sum()
}

pub(super) fn mk_env(name: &str, app: &str, tier: &str, health: &str) -> crate::aws::Environment {
    crate::aws::Environment {
        name: name.into(),
        application: app.into(),
        status: "Ready".into(),
        health: health.into(),
        platform: "Java 17".into(),
        solution_stack: String::new(),
        tier: tier.into(),
        cname: format!("{name}.example.com"),
        version_label: "build-1".into(),
        arn: Some(format!("arn:aws:eb:us-east-1:0:env/{name}")),
        updated: None,
        id: None,
        region: None,
    }
}

/// Helper for the cancel-window tests — build a ConfirmModal for
/// the given Action / env. Mirrors the shape `advance_action_flow`
/// produces; pre-flight fields stay None (the cancel-window code
/// path doesn't read them).
pub(super) fn mk_modal(action: Action, env: &str) -> ConfirmModal {
    ConfirmModal {
        action,
        target_env: env.into(),
        swap_with: None,
        typed: TextInput::new(),
        kind: ConfirmKind::YesNo,
        dryrun: None,
        loading_dryrun: false,
        recent_events: None,
        loading_events: false,
        traffic_warning: None,
        deploy_version: None,
        upgrade_platform_arn: None,
        upgrade_platform_label: None,
        clone_target: None,
        scale_min: None,
        scale_max: None,
        auto_rollback_secs: None,
        wait_for_green_secs: None,
        version_preview: None,
        loading_version_preview: false,
        health_check_probe: None,
        loading_health_check: false,
        unavailability_line: None,
        loading_unavailability: false,
        lint_issues: None,
        loading_lint: false,
        ssm_run_command: None,
        ssm_run_instances: None,
    }
}

// ── :event-tail ─────────────────────────────────────────────────

pub(super) fn mk_fleet_event(
    env: &str,
    severity: &str,
    message: &str,
    at_ms: i64,
) -> crate::aws::Event {
    crate::aws::Event {
        at: chrono::DateTime::from_timestamp_millis(at_ms),
        env: env.into(),
        application: "shop".into(),
        message: message.into(),
        severity: severity.into(),
        version_label: None,
    }
}

/// A wrapped string literal without a `\` continuation embeds the
/// newline *and* the next line's indentation, so the message reaches
/// the operator with a long run of spaces in the middle of a sentence —
/// and the TUI's error bar is one line, so a narrow terminal pushes the
/// actionable half off-screen. This has now happened twice; assert on
/// the rendered text rather than trusting the literal.
#[track_caller]
pub(super) fn assert_no_run_on_spaces(msg: &str) {
    assert!(
        !msg.contains("  "),
        "message contains a double space (missing a `\\` continuation, or a \
         stray space before a `{{}}` placeholder?): {msg:?}"
    );
}

// ── every write command refuses under read-only ────────────────────
//
// The safety gate itself is well covered; what wasn't covered is that
// each write command actually reaches it. Several whole modules of
// setters (`cmd_option`, `cmd_settings`) contain no `deny_write` call
// at all — they're safe only because every one of them routes through
// `spawn_option_settings_update` or `spawn_tag_update`, which gate.
// That is a convention, and this is what turns it into a checked one:
// a new setter that dispatches directly fails here.

/// Write commands, with arguments plausible enough to get past their
/// own usage validation and reach the gate.
pub(super) const WRITE_COMMANDS: &[&str] = &[
    // option-setting setters (cmd_option.rs — no deny_write of its own)
    "deployment-policy Rolling",
    "rolling-update on",
    "health-check-url /healthz",
    "keypair my-key",
    "service-role arn:aws:iam::1:role/svc",
    "instance-profile arn:aws:iam::1:instance-profile/eb",
    "public-ip on",
    "elb-scheme internal",
    "set-option aws:autoscaling:asg MinSize 2",
    // per-env settings (cmd_settings.rs — likewise)
    "tag Owner platform",
    "untag Owner",
    "logs-stream on",
    "notify ops@example.com",
    "rds-detach api-prod",
    // alarms and templates (these gate directly)
    "alarm-create my-alarm 5xx 20",
    "alarm-delete ebman-api-prod-5xx",
    "config-save my-template",
    "config-apply my-template",
];

/// Writes that aren't scoped to one environment — a saved-configuration
/// template belongs to an *application*. These gate with an empty env
/// name, so they honour the global toggle but deliberately can't match
/// a per-env pin. Listed separately so that distinction is stated
/// rather than silently absent from the table above.
pub(super) const APPLICATION_SCOPED_WRITES: &[&str] = &["config-delete uflexi my-template"];

pub(super) fn read_only_app_with_env() -> App {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.read_only = true;
    app
}

/// Bulk writes. These gate through `deny_write_batch` rather than
/// `deny_write` — a separate code path that a per-command test of the
/// single-env surface wouldn't exercise at all.
pub(super) const BATCH_WRITE_COMMANDS: &[&str] = &[
    "batch-rebuild",
    "batch-restart",
    "batch-deploy build-900",
    "batch-tag Owner platform",
    "batch-untag Owner",
    "batch-set-option aws:autoscaling:asg MinSize 2",
];

// ── DLQ destructive operations gate too ────────────────────────────

/// A DLQ viewer open on `env`, with one message selected — enough for
/// the destructive handlers to get as far as the safety gate.
pub(super) fn open_dlq_state(env: &str) -> crate::app::DlqState {
    let mut list_state = ratatui::widgets::ListState::default();
    list_state.select(Some(0));
    crate::app::DlqState {
        env_name: env.into(),
        main_queue_url: "https://sqs/q".into(),
        dlq_url: "https://sqs/q-dlq".into(),
        messages: vec![crate::aws::QueueMessage {
            id: "m-1".into(),
            receipt_handle: "rh-1".into(),
            body: "{}".into(),
            receive_count: 1,
            sent_at: None,
        }],
        list_state,
        loading: false,
        error: None,
        confirm_purge: false,
        purge_typed: Default::default(),
        viewing: crate::app::QueueView::Dlq,
        confirm_delete_id: None,
        replay_input: None,
    }
}

/// Is this path test code rather than production code?
///
/// The two source-scanning guards below walk `src/` looking for a
/// pattern in production code, and both name that pattern in their own
/// assertions — so they have to skip themselves. This used to be
/// `file_name() == "tests.rs"`, which stopped being true the moment
/// `app/tests.rs` became `app/tests/*.rs`: both guards then found their
/// own literals and failed. Matching the whole test subtree is what
/// they meant all along, and it keeps holding as more test modules
/// appear.
pub(super) fn is_test_source(path: &std::path::Path) -> bool {
    path.file_name().and_then(|f| f.to_str()) == Some("tests.rs")
        || path.components().any(|c| c.as_os_str() == "tests")
}

#[test]
fn is_test_source_excludes_test_trees_and_nothing_else() {
    use std::path::Path;
    // Test code, in both shapes that exist in this repo.
    assert!(is_test_source(Path::new("src/aws/tests.rs")));
    assert!(is_test_source(Path::new("src/ui/tests.rs")));
    assert!(is_test_source(Path::new("src/app/tests/refresh.rs")));
    assert!(is_test_source(Path::new("src/app/tests/support.rs")));
    // Production code — over-excluding here would silently switch the
    // two source-scanning guards off, which is worse than the failure
    // they were written to catch.
    assert!(!is_test_source(Path::new("src/app.rs")));
    assert!(!is_test_source(Path::new("src/app/cmd_misc.rs")));
    assert!(!is_test_source(Path::new("src/app/spawn_refresh.rs")));
    assert!(!is_test_source(Path::new("src/ui/detail.rs")));
    // A production file whose name merely contains "test".
    assert!(!is_test_source(Path::new("src/app/latest_stacks.rs")));
}
