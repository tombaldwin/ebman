//! The DLQ browser and its redrive path.
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
fn app_rollup_worker_dlq_alert_counts() {
    let envs = vec![crate::aws::Environment {
        name: "worker-prod".into(),
        application: "wapp".into(),
        status: "Ready".into(),
        health: "Green".into(),
        platform: "Java 17".into(),
        solution_stack: String::new(),
        tier: "Worker".into(),
        cname: String::new(),
        version_label: String::new(),
        arn: None,
        updated: None,
        id: None,
        region: None,
    }];
    let mut dlq: HashMap<String, i64> = HashMap::new();
    dlq.insert("worker-prod".into(), 7);
    let r = crate::app::app_rollup(&envs, "wapp", &dlq);
    // EB calls it Green; ebman flags it because the DLQ is non-empty.
    assert_eq!(r.env_count, 1);
    assert_eq!(r.red_count, 0, "EB health stays Green");
    assert_eq!(
        r.worker_dlq_alerts, 1,
        "worker env with DLQ depth > 0 counts as alerting"
    );
}

#[test]
fn compute_red_alerts_counts_eb_red_and_worker_dlq() {
    use crate::aws::Environment;
    let mk = |name: &str, tier: &str, health: &str| Environment {
        name: name.into(),
        application: "uflexi".into(),
        status: "Ready".into(),
        health: health.into(),
        platform: "Java 17".into(),
        solution_stack: String::new(),
        tier: tier.into(),
        cname: String::new(),
        version_label: String::new(),
        arn: None,
        updated: None,
        id: None,
        region: None,
    };
    let envs = vec![
        mk("web-prod", "Web", "Green"),
        mk("web-red", "Web", "Red"),
        mk("worker-green-dlq", "Worker", "Green"),
        mk("worker-clean", "Worker", "Green"),
        mk("worker-red", "Worker", "Severe"),
    ];
    let mut dlq = std::collections::HashMap::new();
    dlq.insert("worker-green-dlq".to_string(), 3);
    dlq.insert("worker-clean".to_string(), 0);
    // EB-Red + DLQ-Red + EB-Red-on-worker = 3 alerts (worker-red counted once).
    assert_eq!(crate::app::compute_red_alerts(&envs, &dlq), 3);
}

#[test]
fn compute_red_alerts_ignores_dlq_for_web_tier() {
    use crate::aws::Environment;
    let env = Environment {
        name: "web-prod".into(),
        application: "uflexi".into(),
        status: "Ready".into(),
        health: "Green".into(),
        platform: "Java 17".into(),
        solution_stack: String::new(),
        tier: "Web".into(),
        cname: String::new(),
        version_label: String::new(),
        arn: None,
        updated: None,
        id: None,
        region: None,
    };
    // Even with a spurious "web-prod" entry in dlq_depths, a Web env
    // never counts as DLQ-red. Belt-and-braces against a stale cache
    // entry surviving a tier change.
    let mut dlq = std::collections::HashMap::new();
    dlq.insert("web-prod".to_string(), 99);
    assert_eq!(crate::app::compute_red_alerts(&[env], &dlq), 0);
}

#[test]
fn compute_red_alerts_zero_dlq_is_not_alert_worthy() {
    use crate::aws::Environment;
    let env = Environment {
        name: "worker-clean".into(),
        application: "uflexi".into(),
        status: "Ready".into(),
        health: "Green".into(),
        platform: "Java 17".into(),
        solution_stack: String::new(),
        tier: "Worker".into(),
        cname: String::new(),
        version_label: String::new(),
        arn: None,
        updated: None,
        id: None,
        region: None,
    };
    let mut dlq = std::collections::HashMap::new();
    dlq.insert("worker-clean".to_string(), 0);
    assert_eq!(crate::app::compute_red_alerts(&[env], &dlq), 0);
}

#[tokio::test]
async fn worker_queue_fetch_error_keeps_previous_dlq_depth() {
    // 0.27 fix: a failed fetch must not clear the env's alert —
    // the old clear-and-rebuild dropped it every errored tick.
    let mut app = test_app();
    app.handle_msg(AppMsg::WorkerQueueCheck {
        gen: app.generation,
        results: vec![("wk-prod".into(), Ok(Some(7)))],
    });
    assert_eq!(app.worker_dlq_depths.get("wk-prod"), Some(&7));
    // Fetch error → depth survives, marked stale.
    app.handle_msg(AppMsg::WorkerQueueCheck {
        gen: app.generation,
        results: vec![("wk-prod".into(), Err("AccessDenied".into()))],
    });
    assert_eq!(
        app.worker_dlq_depths.get("wk-prod"),
        Some(&7),
        "error must not read as 'no DLQ'"
    );
    assert!(app.worker_dlq_stale.contains("wk-prod"));
    // Successful re-check clears the staleness.
    app.handle_msg(AppMsg::WorkerQueueCheck {
        gen: app.generation,
        results: vec![("wk-prod".into(), Ok(Some(3)))],
    });
    assert!(!app.worker_dlq_stale.contains("wk-prod"));
    assert_eq!(app.worker_dlq_depths.get("wk-prod"), Some(&3));
    // Genuine no-DLQ → cleared; fresh depth → updated.
    app.handle_msg(AppMsg::WorkerQueueCheck {
        gen: app.generation,
        results: vec![("wk-prod".into(), Ok(None))],
    });
    assert!(!app.worker_dlq_depths.contains_key("wk-prod"));
}

#[tokio::test]
async fn render_dlq_depth_tints_the_ready_pill_amber() {
    // A Worker env that EB reports Green but whose DLQ has messages
    // gets its `Ready` pill rendered in health_yellow — the row-level
    // "this isn't actually fine" signal. Differential: same env with
    // an empty DLQ shows no amber pill.
    let mut app = test_app();
    app.environments = vec![mk_env("worker-x", "uflexi", "Worker", "Green")];
    app.rebuild_view();
    app.table_state.select(None);
    let theme = app.theme.clone();

    let clean = render_buf(&mut app, 140, 30);
    let clean_amber = count_fg(&clean, theme.health_yellow);

    app.worker_dlq_depths.insert("worker-x".into(), 12);
    let backed_up = render_buf(&mut app, 140, 30);
    let dlq_amber = count_fg(&backed_up, theme.health_yellow);

    assert!(
        dlq_amber > clean_amber,
        "a non-empty DLQ should add amber (Ready-pill) cells \
             (dlq={dlq_amber}, clean={clean_amber})"
    );
}

#[tokio::test]
async fn dlq_destructive_operations_are_refused_in_read_only_mode() {
    // Purge and replay are irreversible and driven from the DLQ
    // viewer's keymap rather than a `:command`, so the command-level
    // property tests above never reach them.
    /// One destructive DLQ handler, driven from the viewer's keymap.
    type DlqOp = fn(&mut App);
    let cases: Vec<(&str, DlqOp)> = vec![
        ("purge", |app: &mut App| {
            app.spawn_dlq_purge("api-prod".into(), "https://sqs/q-dlq".into())
        }),
        ("replay", |app: &mut App| app.spawn_dlq_replay_batch(vec![])),
        ("resend", |app: &mut App| app.spawn_dlq_resend_selected()),
        ("delete", |app: &mut App| app.spawn_dlq_delete_one("m-1")),
    ];
    for (name, op) in cases {
        let mut app = read_only_app_with_env();
        // `replay`/`resend`/`delete` read the env from DLQ state and
        // return early without it, so they'd never reach the gate.
        app.dlq = Some(open_dlq_state("api-prod"));
        op(&mut app);
        let err = app.error_message.as_deref().unwrap_or_default();
        assert!(
            err.contains("read-only mode"),
            "DLQ {name} was not refused — got {err:?}"
        );
    }
}

#[tokio::test]
async fn dlq_destructive_operations_honour_a_per_env_pin() {
    let mut app = read_only_app_with_env();
    app.read_only = false;
    app.cfg.safety_envs.insert("api-prod".into(), true);
    app.spawn_dlq_purge("api-prod".into(), "https://sqs/q-dlq".into());
    let err = app.error_message.as_deref().unwrap_or_default();
    assert!(err.contains("safety.envs"), "got {err:?}");
}

// --- per-row work goes to the row's region -----------------------------

#[tokio::test]
async fn detail_and_why_and_dlq_use_the_rows_own_region() {
    // Under a multi-region fan-out the selected row is routinely in
    // some other region, but every per-row background fetch used
    // `self.aws`, whose region is `context.region`. Detail showed the
    // environment's name beside the home region's instances, metrics,
    // events and alarms — wrong data wearing the right label.
    let mut app = test_app();
    let mut env = mk_env("api-prod", "uflexi", "Web", "Green");
    env.region = Some("eu-west-2".into());
    app.environments = vec![env.clone()];
    app.rebuild_view();
    app.table_state.select(Some(0));
    assert_eq!(app.context.region, "us-east-1", "home region differs");

    // The lookup all four accessors share.
    assert_eq!(app.region_for_name("api-prod"), "eu-west-2");
    // An env we hold no row for falls back to home — a modal opened
    // before the refresh landed. That's the pre-fan-out behaviour.
    assert_eq!(app.region_for_name("not-in-the-table"), "us-east-1");

    app.open_detail();
    assert_eq!(
        app.detail_client().region_for_tests(),
        "eu-west-2",
        "Detail must fetch from where the environment actually is"
    );
    app.dlq = Some(crate::app::DlqState {
        env_name: "api-prod".into(),
        main_queue_url: String::new(),
        dlq_url: String::new(),
        messages: Vec::new(),
        list_state: Default::default(),
        loading: false,
        error: None,
        confirm_purge: false,
        purge_typed: tui_common::TextInput::new(),
        viewing: crate::app::QueueView::Dlq,
        confirm_delete_id: None,
        replay_input: None,
    });
    assert_eq!(
        app.dlq_client().region_for_tests(),
        "eu-west-2",
        "an SQS queue URL doesn't even exist in the home region"
    );
}

// --- render smoke for the screens the ui.rs split moved -----------------
//
// Measured after the split: `draw_dlq`, `draw_shell` and `draw_events`
// could each be replaced with `return;` and all 1,098 tests still
// passed. Three whole screens with no render coverage — and they are
// the ones an operator reaches during an incident, which is when a
// panic or a blank pane costs most. These are smoke tests, not
// golden-frame tests: they assert the screen draws and puts its own
// identifying content on the buffer.
//
// All three are covered now. `draw_shell` was left out at first on the
// belief that it needed a real PTY; it does not, and that belief was
// recorded twice without being checked. Each test was verified by
// stubbing its screen out and confirming the test fails.

#[tokio::test]
async fn the_dlq_viewer_renders_its_messages() {
    let mut app = test_app();
    app.environments = vec![mk_env("wk-prod", "uflexi", "Worker", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.dlq = Some(crate::app::DlqState {
        env_name: "wk-prod".into(),
        main_queue_url: "https://sqs.eu-west-2.amazonaws.com/1/awseb-main".into(),
        dlq_url: "https://sqs.eu-west-2.amazonaws.com/1/awseb-main-dlq".into(),
        messages: vec![crate::aws::QueueMessage {
            id: "MSG-CANARY-1".into(),
            receipt_handle: "rh".into(),
            body: "poison pill payload".into(),
            receive_count: 7,
            sent_at: None,
        }],
        list_state: Default::default(),
        loading: false,
        error: None,
        confirm_purge: false,
        purge_typed: tui_common::TextInput::new(),
        viewing: crate::app::QueueView::Dlq,
        confirm_delete_id: None,
        replay_input: None,
    });
    app.mode = crate::app::Mode::Dlq;

    let out = render(&mut app, 150, 30);
    assert!(
        out.contains("MSG-CANARY-1"),
        "the message id renders:\n{out}"
    );
    assert!(out.contains("wk-prod"), "and the env it belongs to:\n{out}");
}

#[tokio::test]
async fn the_embedded_shell_pane_renders_its_transcript() {
    // This one was recorded twice as "needs a PTY, can't test". That was
    // wrong: `ShellSession`'s `writer` / `master` / `child` are all
    // `Option` precisely so `--demo` can build a session with no
    // subprocess, and `resize` is a no-op when `master` is `None`. So
    // the pane renders perfectly well in a test — the blocker was a
    // guess that never got checked.
    let mut app = test_app();
    let shell = crate::shell::ShellSession::demo(
        "i-0canary123".into(),
        "$ systemctl status web\nactive (running)\n",
        28,
        140,
    );
    // `demo` parks the transcript in the typewriter, not the parser —
    // the run loop drains it a couple of characters per frame. Drain it
    // fully here so the assertion is about the render, not the pacing.
    for _ in 0..500 {
        shell.tick_demo_typer();
    }
    shell.resize(28, 140);
    app.current_shell = Some(Box::new(shell));
    app.mode = crate::app::Mode::Shell;

    let out = render(&mut app, 150, 30);
    assert!(
        out.contains("i-0canary123"),
        "the pane titles itself with the instance it is attached to:\n{out}"
    );
    assert!(
        out.contains("F12 detach"),
        "and keeps the detach hint visible — it is the way out:\n{out}"
    );
    assert!(
        out.contains("active (running)"),
        "vt100 screen contents reach the ratatui buffer:\n{out}"
    );
}

// ── mutation-sweep triage, 2026-08-26 ────────────────────────────────
//
// `app/msg.rs`'s DLQ handlers carried eleven survivors between them.
// They are worth the attention because the DLQ view is the one place
// where a keystroke destroys a message: `x` deletes the *selected* row
// and `p` purges the queue. A cursor pointing at the wrong row, or a
// result from the wrong env landing in the view, is a real hazard
// rather than a cosmetic one.

fn msg(id: &str) -> crate::aws::QueueMessage {
    crate::aws::QueueMessage {
        id: id.into(),
        receipt_handle: format!("rh-{id}"),
        body: "{}".into(),
        receive_count: 1,
        sent_at: None,
    }
}

/// A peek result belongs to the env and the queue it was launched for.
#[tokio::test]
async fn dlq_messages_from_another_env_or_queue_are_dropped() {
    use crate::app::AppMsg;

    let armed = || {
        let mut app = test_app();
        app.dlq = Some(open_dlq_state("api-prod"));
        app
    };
    let ids = |app: &App| -> Vec<String> {
        app.dlq
            .as_ref()
            .unwrap()
            .messages
            .iter()
            .map(|m| m.id.clone())
            .collect()
    };

    // Baseline: the right env and the currently-viewed queue apply.
    let mut app = armed();
    app.handle_msg(AppMsg::DlqMessages {
        gen: app.generation,
        env_name: "api-prod".into(),
        queue_url: "https://sqs/q-dlq".into(),
        result: Ok(vec![msg("new-1"), msg("new-2")]),
    });
    assert_eq!(
        ids(&app),
        vec!["new-1", "new-2"],
        "the matching peek applies"
    );

    // Another env's peek must not land in this view.
    let mut app = armed();
    app.handle_msg(AppMsg::DlqMessages {
        gen: app.generation,
        env_name: "worker-prod".into(),
        queue_url: "https://sqs/q-dlq".into(),
        result: Ok(vec![msg("wrong-env")]),
    });
    assert_eq!(ids(&app), vec!["m-1"], "another env's messages were shown");

    // A peek that raced an `m`-toggle: right env, but the main queue
    // while the DLQ is being viewed.
    let mut app = armed();
    app.handle_msg(AppMsg::DlqMessages {
        gen: app.generation,
        env_name: "api-prod".into(),
        queue_url: "https://sqs/q".into(),
        result: Ok(vec![msg("main-queue")]),
    });
    assert_eq!(
        ids(&app),
        vec!["m-1"],
        "the main queue's messages were shown in the DLQ view"
    );
}

/// The cursor after a refetch. `x` deletes whatever the cursor points
/// at, so a cursor left past the end of a shorter page is the shape that
/// destroys the wrong message.
#[tokio::test]
async fn a_dlq_refetch_clamps_the_cursor_into_the_new_page() {
    use crate::app::AppMsg;

    let with_cursor = |at: Option<usize>| {
        let mut app = test_app();
        let mut dlq = open_dlq_state("api-prod");
        dlq.list_state.select(at);
        app.dlq = Some(dlq);
        app
    };
    let sel = |app: &App| app.dlq.as_ref().unwrap().list_state.selected();
    let fetch = |app: &mut App, n: usize| {
        let gen = app.generation;
        app.handle_msg(AppMsg::DlqMessages {
            gen,
            env_name: "api-prod".into(),
            queue_url: "https://sqs/q-dlq".into(),
            result: Ok((0..n).map(|i| msg(&format!("m{i}"))).collect()),
        });
    };

    // Cursor exactly at the new length is OUT of bounds — indices stop
    // at len - 1. This is the case `<=` would wave through.
    let mut app = with_cursor(Some(2));
    fetch(&mut app, 2);
    assert_eq!(sel(&app), Some(0), "a cursor at index == len must reset");

    // Well past the end.
    let mut app = with_cursor(Some(9));
    fetch(&mut app, 2);
    assert_eq!(sel(&app), Some(0));

    // A cursor validly inside the new page is KEPT — without this the
    // clamp could reset every time and still pass the cases above.
    let mut app = with_cursor(Some(1));
    fetch(&mut app, 3);
    assert_eq!(sel(&app), Some(1), "a valid cursor survives the refetch");

    // Nothing selected yet → row 0, so Enter/x/r are live immediately.
    let mut app = with_cursor(None);
    fetch(&mut app, 3);
    assert_eq!(sel(&app), Some(0));

    // An empty page has nothing to point at.
    let mut app = with_cursor(Some(1));
    fetch(&mut app, 0);
    assert_eq!(sel(&app), None, "an empty queue selects nothing");
}

/// A completed DLQ operation removes exactly the message it names.
#[tokio::test]
async fn a_dlq_op_removes_only_the_message_it_names() {
    use crate::app::{AppMsg, DlqOp};

    let armed = || {
        let mut app = test_app();
        let mut dlq = open_dlq_state("api-prod");
        dlq.messages = vec![msg("m-1"), msg("m-2"), msg("m-3")];
        app.dlq = Some(dlq);
        app
    };
    let ids = |app: &App| -> Vec<String> {
        app.dlq
            .as_ref()
            .unwrap()
            .messages
            .iter()
            .map(|m| m.id.clone())
            .collect()
    };

    for op in [
        DlqOp::Deleted {
            message_id: "m-2".into(),
        },
        DlqOp::Resent {
            message_id: "m-2".into(),
        },
    ] {
        let mut app = armed();
        app.handle_msg(AppMsg::DlqActionResult {
            gen: app.generation,
            env_name: "api-prod".into(),
            result: Ok(op.clone()),
        });
        assert_eq!(
            ids(&app),
            vec!["m-1", "m-3"],
            "{op:?} must drop only its own message — inverting the retain \
             predicate would leave only that one"
        );
    }

    // Another env's result must not touch this view.
    let mut app = armed();
    app.handle_msg(AppMsg::DlqActionResult {
        gen: app.generation,
        env_name: "worker-prod".into(),
        result: Ok(DlqOp::Purged),
    });
    assert_eq!(
        ids(&app),
        vec!["m-1", "m-2", "m-3"],
        "wrong env purged the view"
    );

    // Purge clears everything.
    let mut app = armed();
    app.handle_msg(AppMsg::DlqActionResult {
        gen: app.generation,
        env_name: "api-prod".into(),
        result: Ok(DlqOp::Purged),
    });
    assert!(ids(&app).is_empty());
}

/// A partial replay is an error, not a success toast. `failures == 0`
/// flipped to `!=` reports every clean replay as a failure and every
/// failed one as clean.
#[tokio::test]
async fn a_replay_with_failures_reports_as_an_error() {
    use crate::app::{AppMsg, DlqOp};

    let replay = |count: usize, failures: usize| {
        let mut app = test_app();
        app.dlq = Some(open_dlq_state("api-prod"));
        app.handle_msg(AppMsg::DlqActionResult {
            gen: app.generation,
            env_name: "api-prod".into(),
            result: Ok(DlqOp::Replayed { count, failures }),
        });
        app
    };

    let clean = replay(5, 0);
    assert!(
        clean
            .status_message
            .as_deref()
            .unwrap_or("")
            .contains("replayed 5"),
        "a clean replay is a status message: {:?}",
        clean.status_message
    );
    assert!(clean.error_message.is_none(), "and not an error");

    let partial = replay(5, 2);
    assert!(
        partial
            .error_message
            .as_deref()
            .unwrap_or("")
            .contains("2 failed"),
        "a partial replay is an error: {:?}",
        partial.error_message
    );
}

/// The type-the-env-name gate on a queue purge.
///
/// `KeyCode::Enter if dlq.purge_typed.text() == dlq.env_name` had all
/// three of its mutants survive the sweep, and one of them is the worst
/// single finding in it: `==` flipped to `!=` purges when the typed name
/// is WRONG and refuses when it is right. The other two bypass the gate
/// entirely (guard → true) or wedge it shut (guard → false).
///
/// `p` on a DLQ is not recoverable. Nothing was checking this.
#[tokio::test]
async fn a_purge_fires_only_when_the_typed_name_matches() {
    let armed = |typed: &str| {
        let mut app = test_app();
        let mut dlq = open_dlq_state("api-prod");
        dlq.confirm_purge = true;
        for c in typed.chars() {
            dlq.purge_typed.handle_key(crossterm::event::KeyEvent {
                code: KeyCode::Char(c),
                modifiers: KeyModifiers::NONE,
                kind: crossterm::event::KeyEventKind::Press,
                state: crossterm::event::KeyEventState::NONE,
            });
        }
        app.dlq = Some(dlq);
        app.mode = crate::app::Mode::Dlq;
        app
    };
    let still_confirming = |app: &App| app.dlq.as_ref().unwrap().confirm_purge;

    // The exact name dispatches: the confirm closes and the buffer clears.
    let mut app = armed("api-prod");
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        !still_confirming(&app),
        "typing the env name exactly must confirm the purge"
    );
    assert!(app.dlq.as_ref().unwrap().purge_typed.text().is_empty());

    // Anything else is a no-op that leaves the prompt up. A near-miss
    // is the realistic case — the operator meant to type it and didn't.
    for wrong in ["api-prod ", "api-pro", "API-PROD", "", "worker-prod"] {
        let mut app = armed(wrong);
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(
            still_confirming(&app),
            "{wrong:?} is not the env name and must NOT purge"
        );
    }

    // Esc abandons the confirm and clears what was typed.
    let mut app = armed("api-pro");
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!still_confirming(&app), "Esc leaves the confirm");
    assert!(app.dlq.as_ref().unwrap().purge_typed.text().is_empty());
}

/// And the matching arm must actually reach the dispatch, not merely
/// clear the flag. A read-only env refuses at `deny_write`, which is
/// only reachable if `spawn_dlq_purge` was called at all — the same
/// wiring-vs-predicate distinction the rollout freeze needed.
#[tokio::test]
async fn a_confirmed_purge_reaches_the_write_gate() {
    let mut app = read_only_app_with_env();
    let mut dlq = open_dlq_state("api-prod");
    dlq.confirm_purge = true;
    for c in "api-prod".chars() {
        dlq.purge_typed.handle_key(crossterm::event::KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::NONE,
            kind: crossterm::event::KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        });
    }
    app.dlq = Some(dlq);
    app.mode = crate::app::Mode::Dlq;

    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let msg = app.error_message.clone().unwrap_or_default();
    assert!(
        msg.contains("read-only") || msg.contains("refus"),
        "a confirmed purge must reach the write gate — a read-only env \
         should have refused it: {msg:?}"
    );
}

/// The single-message delete confirm: `y` / `Y` / `Enter` go ahead,
/// **anything else cancels**. The confirming arm was deletable, and the
/// cancel-on-anything-else behaviour is what makes a stray keypress safe.
#[tokio::test]
async fn a_delete_confirm_takes_yes_and_cancels_on_anything_else() {
    let armed = || {
        let mut app = read_only_app_with_env();
        let mut dlq = open_dlq_state("api-prod");
        dlq.confirm_delete_id = Some("m-1".into());
        app.dlq = Some(dlq);
        app.mode = crate::app::Mode::Dlq;
        app
    };

    for confirm in [KeyCode::Char('y'), KeyCode::Char('Y'), KeyCode::Enter] {
        let mut app = armed();
        press(&mut app, confirm, KeyModifiers::NONE);
        assert!(
            app.dlq.as_ref().unwrap().confirm_delete_id.is_none(),
            "{confirm:?} answers the confirm"
        );
        let msg = app.error_message.clone().unwrap_or_default();
        assert!(
            msg.contains("read-only") || msg.contains("refus"),
            "{confirm:?} must reach the write gate: {msg:?}"
        );
    }

    // Anything else cancels WITHOUT dispatching.
    for other in [KeyCode::Char('n'), KeyCode::Esc, KeyCode::Char('z')] {
        let mut app = armed();
        press(&mut app, other, KeyModifiers::NONE);
        assert!(
            app.dlq.as_ref().unwrap().confirm_delete_id.is_none(),
            "{other:?} clears the confirm"
        );
        assert!(
            app.error_message.is_none(),
            "{other:?} must not dispatch anything: {:?}",
            app.error_message
        );
    }
}

/// The DLQ cursor wraps both ways. Ten survivors sat on these two
/// expressions, and this cursor is what `x` deletes and what `r`
/// resends — the same reason the refetch clamp mattered.
#[tokio::test]
async fn the_dlq_cursor_wraps_at_both_ends() {
    let mut app = test_app();
    let mut dlq = open_dlq_state("api-prod");
    dlq.messages = vec![msg("m-1"), msg("m-2"), msg("m-3")];
    dlq.list_state.select(Some(0));
    app.dlq = Some(dlq);
    app.mode = crate::app::Mode::Dlq;
    let sel = |app: &App| app.dlq.as_ref().unwrap().list_state.selected();

    press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
    assert_eq!(sel(&app), Some(1));
    press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
    assert_eq!(sel(&app), Some(2));
    press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
    assert_eq!(sel(&app), Some(0), "j wraps past the last message");

    press(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
    assert_eq!(sel(&app), Some(2), "k wraps past the first");
    press(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
    assert_eq!(sel(&app), Some(1));

    // The arrow keys are the same arms.
    press(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(sel(&app), Some(2));
    press(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(sel(&app), Some(1));

    // An empty queue must not panic on either key — `if n == 0`.
    let mut app = test_app();
    let mut dlq = open_dlq_state("api-prod");
    dlq.messages.clear();
    dlq.list_state.select(None);
    app.dlq = Some(dlq);
    app.mode = crate::app::Mode::Dlq;
    press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
    press(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
    assert_eq!(sel(&app), None, "an empty queue has nothing to select");
}
