//! Unit tests for `cli::mcp::writes` — the write-gate, refusal and
//! elicitation paths.
//!
//! Moved verbatim out of `writes.rs`, which was 4,305 lines of which
//! 1,655 were this module.
//!
//! Attached with `#[path]` rather than declared under `mcp::tests`,
//! and that is not a style preference. This module is a CHILD of
//! `writes`, which is what lets it reach `writes`'s private items —
//! `WriteVerb`, `CONFIRM_TOOL`, `write_verb_names`, `CONFIRM_TTL_SECS`
//! and a dozen more. Re-parenting it under `mcp::tests` compiled the
//! paths fine and then failed on visibility, and the only way to make
//! that work would have been to widen a dozen items to `pub(crate)` —
//! paying for a file move with permanent API surface, on the module
//! that owns the write gate.
//!
//! So: same parent, same `use super::*`, different file.

use super::*;

#[test]
fn write_gate_refuses_under_freeze_and_pin() {
    let cfg = crate::config::Config::default();
    // No freeze, no pin -> clear.
    assert!(crate::cli::write_refusal(&cfg, "prod", &None, None, None, "Test").is_none());
    // Active freeze -> refusal names it + the remedy.
    let m = crate::freeze::FreezeMarker {
        pid: 4242,
        reason: "checkout 5xx".into(),
        incident: true,
        at: "now".into(),
    };
    let msg =
        crate::cli::write_refusal(&cfg, "prod", &None, Some(m), None, "Test").expect("refused");
    assert!(msg.contains("freeze active") && msg.contains(":incident END"));
    // Pin -> refusal (no freeze).
    let mut pinned = crate::config::Config::default();
    pinned.safety_envs.insert("prod".into(), true);
    let msg2 =
        crate::cli::write_refusal(&pinned, "prod", &None, None, None, "Test").expect("pin refused");
    assert!(msg2.contains("pinned by"));
}

#[test]
fn a_superseded_token_says_so_rather_than_unknown() {
    // Confirming a token that a newer plan replaced used to return
    // "unknown confirm_token" — the same answer a TYPO gets. The
    // two want different next moves from the agent: re-read the
    // newer plan, versus re-send the token it already holds.
    let mut retired = std::collections::VecDeque::new();
    retired.push_back("tok-old".to_string());

    let msg = mismatched_token_message(&retired, "tok-old");
    assert!(msg.contains("superseded"), "{msg}");
    assert!(
        msg.contains("confirm that one"),
        "and says what to do instead: {msg}"
    );

    let msg = mismatched_token_message(&retired, "tok-typo");
    assert!(msg.contains("unknown"), "a token we never minted: {msg}");
    assert!(!msg.contains("superseded"), "{msg}");

    // The wiring, not just the branch: installing a second plan
    // must be what puts the first token into `retired`. Pinning
    // only the message left "nothing ever retires anything" green.
    let mut st = WriteState::default();
    let plan = |tok: &str| PendingWrite {
        token: tok.to_string(),
        verb: WriteVerb::Restart,
        env: "api-prod".into(),
        version: None,
        settings: Vec::new(),
        profile: None,
        region: None,
        expires_at: tokio::time::Instant::now() + std::time::Duration::from_secs(60),
        dlq_visible: None,
        caller: CallerIdentity::Known {
            arn: "arn:aws:iam::123456789012:user/test".into(),
            account: "123456789012".into(),
        },
        name_retry_used: false,
        dlq_targets: Vec::new(),
        dlq_url: None,
        dlq_main_url: None,
    };
    st.install(plan("tok-a"));
    assert!(st.retired.is_empty(), "the first plan replaces nothing");
    st.install(plan("tok-b"));
    assert!(
        mismatched_token_message(&st.retired, "tok-a").contains("superseded"),
        "installing a second plan retires the first"
    );
    assert_eq!(st.pending.as_ref().map(|p| p.token.as_str()), Some("tok-b"));

    // Bounded, so past the cap a retired token falls back to the
    // unknown answer — honest, because we genuinely no longer know.
    for i in 0..RETIRED_TOKEN_MEMORY + 4 {
        st.install(plan(&format!("tok-{i}")));
    }
    assert_eq!(st.retired.len(), RETIRED_TOKEN_MEMORY, "memory is bounded");
    assert!(mismatched_token_message(&st.retired, "tok-a").contains("unknown"));
}

/// `can_ask` must track the argument in BOTH directions. A test
/// that only pinned the `false` case would pass against a hardcoded
/// `false` — which is precisely the shape the elicitation flag
/// would degrade into if the plumbing came loose.
#[test]
fn audit_extras_record_whether_the_client_could_be_asked() {
    let find = |extras: &[(&'static str, String)], key: &str| -> Option<String> {
        extras
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.clone())
    };

    let cannot = write_extras_parts("some-agent", false, None, 0, None, None);
    assert_eq!(find(&cannot, "can_ask").as_deref(), Some("false"));
    assert_eq!(find(&cannot, "client").as_deref(), Some("some-agent"));
    assert_eq!(find(&cannot, "via").as_deref(), Some("mcp"));

    let can = write_extras_parts("some-agent", true, None, 0, None, None);
    assert_eq!(
        find(&can, "can_ask").as_deref(),
        Some("true"),
        "a client that declared elicitation must be recorded as such"
    );
}

#[test]
fn audit_extras_omit_optional_context_when_absent() {
    let bare = write_extras_parts("agent", false, None, 0, None, None);
    assert!(
        !bare
            .iter()
            .any(|(k, _)| *k == "version" || *k == "settings"),
        "absent context must not appear as an empty value: {bare:?}"
    );

    let full = write_extras_parts("agent", false, Some("app-v3"), 2, None, None);
    assert!(full.contains(&("version", "app-v3".to_string())));
    assert!(full.contains(&("settings", "2".to_string())));
}

/// Pins the WIRING, not just the helper: a test that handed the flag
/// to the pure function only ever proved the value it supplied
/// itself came back. The suite stayed green with the audit line
/// hardcoded to `can_ask=false` — so this drives a real
/// `initialize` and reads the extras the dispatch path would build.
#[tokio::test]
async fn the_audit_line_reflects_the_capability_the_client_declared() {
    async fn can_ask_in_audit(caps: serde_json::Value) -> String {
        let s = Server::with_scope(true, false, WriteScope::None);
        let _ = s
            .handle_request(&json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": caps,
                    "clientInfo": {"name": "probe", "version": "1"}
                }
            }))
            .await;
        s.write_extras("probe", None, 0, None, None)
            .iter()
            .find(|(k, _)| *k == "can_ask")
            .map(|(_, v)| v.clone())
            .expect("dispatch audit line must record can_ask")
    }

    assert_eq!(
        can_ask_in_audit(json!({"elicitation": {}})).await,
        "true",
        "a client that CAN be asked must be auditable as such"
    );
    assert_eq!(
        can_ask_in_audit(json!({})).await,
        "false",
        "a client that cannot be asked must not be logged as if it could"
    );
}

/// The headline case for `stage=refused`: an agent asks to
/// terminate a pinned environment, and the attempt leaves a trace.
///
/// Before this, it left none. The refusal happens before any AWS
/// call, so no dispatched/completed pair was ever written — six
/// attempts against prod and an empty log looked identical.
///
/// Driven through the real tool rather than through `write_refusal`
/// directly, because the funnel is the thing under test: a call
/// site that skipped it would pass a helper-level test cleanly.
#[tokio::test]
async fn a_refused_mcp_write_is_recorded_against_the_agent() {
    let env_name = "mcp-refusal-probe-env";
    let mut cfg = crate::config::Config::default();
    cfg.safety_envs.insert(env_name.into(), true);
    let s = Server::with_config(false, false, WriteScope::All, cfg);

    let path = crate::util::cache_dir().join("audit.log");
    let before = std::fs::read_to_string(&path).unwrap_or_default();

    let err = s
        .tool_write_plan(
            WriteVerb::Terminate,
            &json!({"env": env_name, "region": "eu-west-2"}),
        )
        .await
        .expect_err("a pinned env must refuse");
    assert!(err.to_string().contains("safety.envs"), "{err}");

    let after = std::fs::read_to_string(&path).unwrap_or_default();
    let delta = after
        .strip_prefix(&before)
        .expect("the audit log is append-only");
    let lines: Vec<&str> = delta.lines().filter(|l| l.contains(env_name)).collect();
    assert_eq!(lines.len(), 1, "exactly one refusal line: {delta}");
    let line = lines[0];

    assert!(line.contains("stage=refused"), "{line}");
    assert!(
        line.contains("action=Terminate"),
        "the log must name what was attempted, not just that \
             something was: {line}"
    );
    assert!(line.contains("rule=env_pinned"), "{line}");
    assert!(
        line.contains("region=eu-west-2"),
        "the region the agent asked for, not the home region: {line}"
    );
}

/// A demo MCP server must refuse the same way and write nothing.
///
/// Demo gets `Config::default()` (no pins), but `freeze::read_active`
/// reads the REAL cross-process marker — so a demo write attempted
/// during a live `:freeze-deploys` was appending a real line to the
/// real audit log, against this module's stated contract.
#[tokio::test]
async fn a_demo_server_refuses_without_writing_an_audit_line() {
    let env_name = "mcp-demo-refusal-probe-env";
    let mut cfg = crate::config::Config::default();
    cfg.safety_envs.insert(env_name.into(), true);

    let path = crate::util::cache_dir().join("audit.log");

    // Real backend: refuses AND records.
    let real = Server::with_config(false, false, WriteScope::All, cfg.clone());
    let before = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = real
        .tool_write_plan(WriteVerb::Terminate, &json!({"env": env_name}))
        .await
        .expect_err("pinned env must refuse");
    let after = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        after
            .strip_prefix(&before)
            .unwrap_or(&after)
            .contains(env_name),
        "a real refusal must still be recorded"
    );

    // Demo backend: refuses, records NOTHING.
    let demo = Server::with_config(true, false, WriteScope::All, cfg);
    let before = std::fs::read_to_string(&path).unwrap_or_default();
    let err = demo
        .tool_write_plan(WriteVerb::Terminate, &json!({"env": env_name}))
        .await
        .expect_err("demo must still refuse — the verdict is real");
    assert!(err.to_string().contains("safety.envs"), "{err}");
    let after = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        !after
            .strip_prefix(&before)
            .unwrap_or(&after)
            .contains(env_name),
        "demo mode writes NO audit lines"
    );
}

/// The plan carries the message ID, never a receipt handle.
///
/// SQS deletes by handle, and a handle is valid only while the
/// message is invisible: the peek that issues one uses a 5-second
/// visibility timeout, while a confirm token lives 60. So a handle
/// captured at plan time is dead for 55 of the 60 seconds the plan
/// stays confirmable — carrying it would fail almost always, which
/// at least fails loudly. The variant that ships is re-receiving at
/// confirm and acting on whatever comes back, which removes a
/// different message than the plan named and says nothing about it.
#[test]
fn the_confirm_window_outlives_a_receipt_handle() {
    // The arithmetic that makes this the default path rather than a
    // race. If either constant moves, the reasoning in
    // `dispatch_dlq_message` needs re-reading.
    // Read the peek's visibility timeout out of the source rather
    // than restating it: `assert!(CONFIRM_TTL_SECS > 5)` is
    // constant-folded, so clippy rightly calls it an assertion that
    // cannot fail — the "test that is worse than none" shape this
    // repo keeps finding.
    let sqs = crate::app::tests::scan::production_source("aws/sqs.rs");
    let visibility: u64 = sqs
        .split(".visibility_timeout(")
        .nth(1)
        .and_then(|r| r.split(')').next())
        .and_then(|n| n.trim().parse().ok())
        .expect("the peek sets a visibility timeout");
    assert!(
        CONFIRM_TTL_SECS > visibility,
        "a confirm token ({CONFIRM_TTL_SECS}s) outliving the peek's \
             {visibility}s visibility timeout is WHY the plan carries an \
             id rather than a receipt handle. If this ever stops being \
             true, re-read the comment on `PendingWrite::dlq_targets` \
             before simplifying anything."
    );
    // And the plan type must not be able to carry a handle.
    let src = crate::app::tests::scan::production_source("cli/mcp/writes.rs");
    let decl = src
        .split("pub(super) struct PendingWrite {")
        .nth(1)
        .and_then(|r| r.split('}').next())
        .expect("PendingWrite is declared here");
    assert!(
        !decl.contains("receipt_handle"),
        "a receipt handle in the plan is dead before the token \
             expires: {decl}"
    );
    assert!(
        decl.contains("dlq_targets"),
        "the plan must still carry message IDS — the field a handle would \
             have replaced: {decl}"
    );
}

/// The dispatch backstop: a resend plan with no main queue sends
/// NOTHING, rather than falling back to some other url.
///
/// The plan refuses this case first, so only a broken plan invariant
/// reaches here — which is exactly when a quiet fallback would do the
/// most damage. No send or delete rule is registered, so any attempt
/// at either panics the mock and fails the test.
#[tokio::test]
async fn a_resend_plan_with_no_main_queue_sends_nothing() {
    use aws_sdk_sqs::operation::receive_message::ReceiveMessageOutput;
    use aws_sdk_sqs::types::Message;
    let peek = aws_smithy_mocks::mock!(aws_sdk_sqs::Client::receive_message).then_output(|| {
        ReceiveMessageOutput::builder()
            .messages(
                Message::builder()
                    .message_id("m-1")
                    .receipt_handle("rh-1")
                    .body("job")
                    .build(),
            )
            .build()
    });
    let sqs =
        aws_smithy_mocks::mock_client!(aws_sdk_sqs, aws_smithy_mocks::RuleMode::MatchAny, [&peek]);
    let cfg = aws_config::SdkConfig::builder()
        .region(aws_config::Region::new("us-west-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    let client = crate::aws::AwsClient::for_tests(
        aws_sdk_elasticbeanstalk::Client::new(&cfg),
        sqs,
        aws_sdk_cloudwatch::Client::new(&cfg),
        aws_sdk_cloudwatchlogs::Client::new(&cfg),
        aws_sdk_s3::Client::new(&cfg),
        aws_sdk_ec2::Client::new(&cfg),
    );
    let mut p = pending_for(WriteVerb::DlqResend);
    p.dlq_url = Some("https://sqs/some-dead-letter-queue".into());
    p.dlq_targets = vec![DlqTarget {
        id: "m-1".into(),
        task: "t".into(),
    }];

    let out = dispatch_dlq_batch(&client, &p)
        .await
        .expect("the batch runs");
    let err = out[0]
        .result
        .as_ref()
        .expect_err("with nowhere to send it, the message must not be moved");
    assert!(err.contains("nothing was sent"), "{err}");
}

/// Resend sends BEFORE deleting.
///
/// The other order can lose the message outright: delete succeeds,
/// send fails, and the message exists nowhere. This order can
/// duplicate it, and a duplicate in a worker queue is the
/// recoverable failure — sqsd tasks are retried by design, so a
/// task running twice is a known shape and a task vanishing is not.
///
/// Source-pinned because the dispatch needs SQS: mutating the
/// ordering away left the whole suite green.
///
/// **This guard spent from `beae2bd` until 2026-09-22 asserting on
/// its own source.** It was correct when written (`8ad7066`), then
/// `beae2bd` renamed `dispatch_dlq_message` to
/// `dispatch_one_dlq_message` and did not update the string here.
/// The `.expect()` should have fired — except the test module lived
/// in `writes.rs` too, so the old name was still in the file it read,
/// in this very line. `.nth(1)` returned the text after THIS literal,
/// which contains a `send_message(` before a `delete_message(`, and
/// every assertion below passed on it. Relocating the tests to their
/// own file is what broke it, which is the only reason anyone knows.
///
/// Hence `production_source`: it reads the production file by name
/// from the crate root, so a guard cannot be handed its own text.
#[test]
fn a_resend_sends_before_it_deletes() {
    let src = crate::app::tests::scan::production_source("cli/mcp/writes.rs");
    let body = src
        .split("async fn dispatch_one_dlq_message(")
        .nth(1)
        .and_then(|r| r.split("\n}").next())
        .expect("the dispatch is defined here");
    // `expect`, not `Option` ordering: `None < Some(_)`, so a missing
    // `send_message` would have SATISFIED the ordering assertion.
    let send = body
        .find("send_message(")
        .expect("the resend path must send");
    let del = body
        .find("delete_message(")
        .expect("the resend path must delete");
    assert!(
        send < del,
        "send must come before delete — the other order loses the \
             message when the send fails, and there is nothing to \
             recover it from: {body}"
    );
    // The send must be conditional on the verb: a plain delete that
    // also resent would put the message back every time.
    assert!(
        body.contains("verb == WriteVerb::DlqResend"),
        "only a resend sends: {body}"
    );
}

/// The confirm-time scope gate can actually fire.
///
/// It is unreachable through the normal path — a plan only exists
/// because the plan gate let it through — which makes it exactly
/// the kind of guard that rots unnoticed. A guard that cannot be
/// shown to fail is worse than none, because it reads as coverage.
/// So reach it the only way anything could: install a plan whose
/// verb the scope does not admit, as a client would if the two
/// gates ever disagreed.
#[tokio::test]
async fn the_confirm_gate_refuses_a_plan_outside_the_scope() {
    let s = Server::with_scope(
        true,
        false,
        crate::cli::mcp::WriteScope::Only(vec!["dlq_delete".into()]),
    );
    {
        let mut st = s.writes.lock().await;
        st.install(PendingWrite {
            token: "tok".into(),
            verb: WriteVerb::Terminate,
            env: "prod".into(),
            version: None,
            settings: Vec::new(),
            profile: None,
            region: None,
            expires_at: tokio::time::Instant::now() + std::time::Duration::from_secs(60),
            dlq_visible: None,
            caller: CallerIdentity::Known {
                arn: "arn:aws:iam::123456789012:user/test".into(),
                account: "123456789012".into(),
            },
            name_retry_used: false,
            dlq_targets: Vec::new(),
            dlq_url: None,
            dlq_main_url: None,
        });
    }

    let err = s
        .tool_confirm_action(&json!({"confirm_token": "tok", "confirm_name": "prod"}))
        .await
        .expect_err("terminate is outside the grant");
    assert!(
        err.to_string().contains("not in this server's write scope"),
        "the refusal must name the scope: {err}"
    );

    // And the plan is DROPPED, not left confirmable: a rejected
    // plan that survives is one retry away from dispatching.
    let st = s.writes.lock().await;
    assert!(
        st.pending.is_none(),
        "a plan the scope rejects must not stay confirmable"
    );
}

/// A verb refused for being ungranted leaves a trace.
///
/// Modelled on `a_refused_mcp_write_is_recorded_against_the_agent`,
/// and for a sharper reason: an ungranted verb is not advertised,
/// so a client that calls one is working from a stale tool list or
/// probing the surface. Without a line, six such attempts and none
/// look identical — the pre-0.37 blind spot, reintroduced by a new
/// refusal path rather than by regressing an old one.
#[tokio::test]
async fn an_out_of_scope_write_is_recorded_against_the_agent() {
    let env_name = "mcp-scope-refusal-probe-env";
    let s = Server::with_config(
        false,
        false,
        crate::cli::mcp::WriteScope::Only(vec!["dlq_delete".into()]),
        crate::config::Config::default(),
    );

    let path = crate::util::cache_dir().join("audit.log");
    let before = std::fs::read_to_string(&path).unwrap_or_default();

    let err = s
        .tool_write_plan(
            WriteVerb::Terminate,
            &json!({"env": env_name, "region": "eu-west-2"}),
        )
        .await
        .expect_err("terminate was not granted");
    assert!(
        err.to_string().contains("not in this server's write scope"),
        "{err}"
    );

    let after = std::fs::read_to_string(&path).unwrap_or_default();
    let delta = after
        .strip_prefix(&before)
        .expect("the audit log is append-only");
    let lines: Vec<&str> = delta.lines().filter(|l| l.contains(env_name)).collect();
    assert_eq!(lines.len(), 1, "exactly one refusal line: {delta}");
    let line = lines[0];

    assert!(line.contains("stage=refused"), "{line}");
    assert!(
        line.contains("action=Terminate"),
        "the log must name what was attempted: {line}"
    );
    assert!(
        line.contains("rule=not_granted"),
        "and why, distinctly from a pin or a freeze — the remedy is \
             a different one: {line}"
    );
    assert!(
        line.contains("--allow-writes=terminate"),
        "and the remedy names the exact flag: {line}"
    );
    assert!(
        line.contains("region=eu-west-2"),
        "against the region the call named, not home: {line}"
    );
}

/// The DLQ verbs plan from the fixture in demo, never from AWS.
///
/// The module promises demo "plans and dispatches synthetically —
/// no AWS, no audit, no webhook", and `restart` honoured it while
/// these three did not: they built a real client and called
/// `describe_worker_queues`, then `peek_messages`. Dispatch was
/// already demo-guarded, so no real message could be deleted — but
/// `peek_messages` is not side-effect-free. Its own tool
/// description says it increments `receive_count` on every message
/// it returns, so `--demo` could alter metadata on a live queue.
///
/// The demo `AwsClient` is a fail-loudly stub, which is what makes
/// this test sharp: a path that reaches for AWS does not quietly
/// return something plausible, it errors. So a PLAN coming back at
/// all is proof the fixture served it.
#[tokio::test]
async fn the_dlq_verbs_plan_from_the_fixture_in_demo() {
    let s = Server::with_scope(true, false, crate::cli::mcp::WriteScope::All);
    let msg = crate::demo_fixture::dlq_messages_for_env("poly-batch");
    let id = msg.first().expect("the fixture has a message").id.clone();

    for (verb, args) in [
        (
            WriteVerb::DlqDelete,
            json!({"env": "poly-batch", "message_id": id}),
        ),
        (
            WriteVerb::DlqResend,
            json!({"env": "poly-batch", "message_id": id}),
        ),
        (WriteVerb::DlqPurge, json!({"env": "poly-batch"})),
    ] {
        let body = s
            .tool_write_plan(verb, &args)
            .await
            .unwrap_or_else(|e| panic!("{:?} must plan from the fixture: {e}", verb));
        let v: Value = serde_json::from_str(&body).expect("json");
        assert_eq!(v["pending"], json!(true), "{verb:?}: {v}");
        assert!(
            v["plan"]["queue"]
                .as_str()
                .is_some_and(|q| q.ends_with("poly-batch-dlq")),
            "{verb:?} must name the fixture's dead-letter queue: {v}"
        );
        // The WIRING, not just the function. A mutation replacing
        // the rendered value with "" left the suite green: the
        // foreclosure was unit-tested and unreachable-in-practice,
        // which is the shape CLAUDE.md warns about — pin the wiring,
        // not just the branch.
        let f = v["plan"]["forecloses"]
            .as_str()
            .unwrap_or_else(|| panic!("{verb:?} plan carries no forecloses: {v}"));
        assert!(
            f.len() > 40,
            "{verb:?} must say what it destroys, in the plan itself: {f:?}"
        );
        assert!(
            f.contains("queue"),
            "{verb:?} is a queue operation and its foreclosure should say so: {f:?}"
        );
    }

    // The pending plan must RETAIN what the audit will need. A
    // mutation dropping `dlq_task = Some(task)` left the suite
    // green: the extras builder was tested, the wiring into it was
    // not — third time today that distinction has bitten.
    //
    // Re-planned deliberately: the loop above ends on `dlq_purge`,
    // which names no single message and correctly carries neither
    // field. Asserting on whatever the loop happened to leave
    // behind would have been testing the wrong plan.
    s.tool_write_plan(
        WriteVerb::DlqDelete,
        &json!({"env": "poly-batch", "message_id": id}),
    )
    .await
    .expect("plan");
    {
        let st = s.writes.lock().await;
        let p = st.pending.as_ref().expect("a plan is pending");
        assert_eq!(
            p.dlq_targets,
            vec![DlqTarget {
                id: id.clone(),
                task: "Remove unattended jobs".into(),
            }],
            "the plan must carry the id the audit line will name, and the task — \
                 or the log can say which message but not what"
        );
    }

    // And an id the fixture does not hold is refused, rather than
    // demo accepting anything — a plan that names a message which
    // is not there is the silent target swap this surface exists
    // to prevent.
    let err = s
        .tool_write_plan(
            WriteVerb::DlqDelete,
            &json!({"env": "poly-batch", "message_id": "not-a-real-id"}),
        )
        .await
        .expect_err("an unknown id must be refused even in demo");
    assert!(
        err.to_string().contains("not in the dead-letter queue"),
        "{err}"
    );
}

/// Every verb says what it forecloses, and says it in one clean line.
///
/// Two properties in one test because they failed together. The
/// foreclosure text must exist for every verb — a plan silent about
/// stakes reads as complete, which is worse than a vague one — and
/// it must render without an indentation hole.
///
/// The hole is not hypothetical: these literals shipped with
/// 14-space gaps twice while being written, because a generator ate
/// the `\` continuations. `no_wrapped_string_literal_leaves_an_
/// indentation_hole` passes clean against the collapsed form, so
/// the only thing that catches it is asserting on the RENDERED
/// string.
#[test]
fn every_verb_states_what_it_forecloses() {
    for verb in WriteVerb::ALL {
        for depth in [None, Some(1), Some(12)] {
            let t = forecloses(verb, depth, 1);
            assert!(t.len() > 40, "{verb:?} must say what it destroys: {t:?}");
            assert!(
                !t.contains("  "),
                "{verb:?} has an indentation hole — a wrapped literal lost its \
                     continuation, and this renders into a one-line prompt: {t:?}"
            );
            assert!(t.ends_with('.'), "{verb:?}: {t:?}");
        }
    }

    // The destructive tail must state the LIMIT of recovery, and
    // the two differ — which is the point. Without this the test
    // passes on eight copies of "nothing happens".
    let del = forecloses(WriteVerb::DlqDelete, Some(12), 1);
    assert!(
        del.contains("dlq_undo") && del.contains(&UNDO_WINDOW_SECS.to_string()),
        "a delete is briefly recoverable and must say how and for how long: {del:?}"
    );
    assert!(
        del.contains("after that nothing can"),
        "and must say the window ENDS — a recovery offer without an expiry is \
             the more dangerous half of the claim: {del:?}"
    );

    let purge = forecloses(WriteVerb::DlqPurge, Some(12), 1);
    assert!(
        purge.contains("None of them can be recovered"),
        "a purge is never recoverable: {purge:?}"
    );
    assert!(
        !purge.contains("dlq_undo"),
        "and must not offer an undo it does not have — a purge is deliberately \
             not captured, because a capped sample would restore SOME of what it \
             destroyed and call that an undo: {purge:?}"
    );
    // And the cheap end must say it is cheap, or the variance that
    // makes the field informative is lost.
    let restart = forecloses(WriteVerb::Restart, None, 1);
    assert!(
        restart.contains("Nothing else"),
        "restart is cheap and should read as cheap: {restart:?}"
    );

    // The count is SQS's approximate one and must not be stated as
    // fact — "the only message in the queue" is a firmer claim than
    // the source supports.
    let one = forecloses(WriteVerb::DlqDelete, Some(1), 1);
    assert!(one.contains("approximately"), "{one:?}");
    assert!(
        !one.contains("only message"),
        "an approximate count must not be rendered as certainty: {one:?}"
    );
    // No queue known, no queue sentence invented.
    assert!(
        !forecloses(WriteVerb::DlqDelete, None, 1).contains("SQS reports"),
        "with no depth available, say nothing about the depth"
    );
}

/// A DLQ write records WHICH message, and what it was.
///
/// The target of a DLQ write is the environment, so the audit line
/// used to say a message was deleted from `poly-batch` and never
/// which one. For every other verb that is survivable — you can go
/// and look at the environment afterwards. For a delete it is not:
/// the thing the log declines to name is exactly the thing that no
/// longer exists.
///
/// Tier 1 of the retention design in `docs/design/runtime-grants.md`,
/// and the prerequisite for the rest: a copy is worth little if the
/// log cannot say what it was a copy of.
#[test]
fn a_dlq_write_records_which_message_it_destroyed() {
    let extras = write_extras_parts(
        "agent",
        false,
        None,
        0,
        Some("d3b07384-d9a0-4f1e-9f3a-11c0ffee0001"),
        Some("Remove unattended jobs"),
    );
    let get = |k: &str| extras.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
    assert_eq!(
        get("message_id").as_deref(),
        Some("d3b07384-d9a0-4f1e-9f3a-11c0ffee0001"),
        "the log must name the message that no longer exists"
    );
    assert_eq!(
        get("task").as_deref(),
        Some("Remove unattended jobs"),
        "and what it was — an id alone answers neither question asked afterwards"
    );

    // Non-DLQ writes carry neither, rather than empty fields: an
    // `message_id=` on a deploy line would be noise that reads as
    // missing data.
    let deploy = write_extras_parts("agent", false, Some("app-v3"), 0, None, None);
    assert!(
        deploy
            .iter()
            .all(|(n, _)| *n != "message_id" && *n != "task"),
        "a non-queue write must not carry empty queue fields: {deploy:?}"
    );
}

/// A resend carries the task identity, not just the body.
///
/// The defect this pins, shipped in 0.40.0 and found a day later
/// by a change that needed the same data: all three resend paths
/// called `send_message(url, &msg.body)` and dropped the custom
/// attributes.
///
/// For a cron-style task that is not a partial loss, it is total.
/// The codebase already records why: the body is the fixed literal
/// "elasticbeanstalk scheduled job" and carries nothing, while
/// `beanstalk.sqsd.task_name` / `.path` / `.scheduled_time` carry
/// every fact about which task it was. A resend that dropped them
/// put a husk on the main queue.
///
/// What follows from that about sqsd's behaviour — that it would
/// have no path to route to — is inference, not something verified
/// against a live worker. The attribute loss itself is not.
#[test]
fn a_resend_carries_the_sqsd_attributes() {
    let m = crate::demo_fixture::dlq_messages_for_env("poly-batch")
        .into_iter()
        .find(|m| m.task.is_some())
        .expect("the fixture has an EB task");

    // The premise: for this shape the body is worthless on its own.
    assert_eq!(
        m.body, "elasticbeanstalk scheduled job",
        "if this stops being a fixed literal, re-read why attributes matter"
    );

    let names: Vec<&str> = m.attributes.iter().map(|(n, _, _)| n.as_str()).collect();
    for want in [
        "beanstalk.sqsd.task_name",
        "beanstalk.sqsd.path",
        "beanstalk.sqsd.scheduled_time",
    ] {
        assert!(
            names.contains(&want),
            "a restorable message must retain {want}: {names:?}"
        );
    }

    // And the source scan. Not a match against the one spelling the
    // bug happened to have: a mutation passing `&[]` instead of
    // `&msg.attributes` is the identical defect and walked straight
    // past the first version of this. So every production
    // `send_message(` call must name `attributes` in its arguments.
    // Located from the crate root, not relative to this file. When
    // this module moved into `tests/`, `include_str!("writes.rs")`
    // silently became a read of THIS file — a source guard scanning
    // its own test and passing.
    let sources = ["cli/mcp/writes.rs", "app/spawn_dlq.rs"];
    let mut calls = 0;
    for name in sources {
        let prod = crate::app::tests::scan::production_source(name);
        for (i, _) in prod.match_indices(".send_message(") {
            calls += 1;
            // The call text up to its closing `.await`, which is
            // where the argument list has certainly ended.
            let tail = &prod[i..];
            let call = &tail[..tail.find(".await").unwrap_or(tail.len()).min(400)];
            assert!(
                call.contains("attributes"),
                "{name}: a send_message that does not pass attributes delivers a \
                     husk for any sqsd task — the body alone is a fixed literal:\n{call}"
            );
        }
    }
    assert!(
        calls >= 2,
        "the scan found {calls} send_message calls — it has stopped seeing them, \
             which is worse than finding a defect"
    );
}
/// A deleted message is briefly recoverable, and the offer is
/// honest about its limits.
#[tokio::test]
async fn a_deleted_message_can_be_put_back_within_the_window() {
    let s = Server::with_scope(true, false, crate::cli::mcp::WriteScope::All);
    let msg = crate::demo_fixture::dlq_messages_for_env("poly-batch")
        .into_iter()
        .find(|m| m.task.is_some())
        .expect("fixture");
    let id = msg.id.clone();

    let window = s
        .remember_deleted(
            &deleted_from(
                "poly-batch",
                Some("https://sqs.eu-west-2.amazonaws.com/1/poly-batch-dlq".into()),
            ),
            msg,
        )
        .await;
    assert_eq!(
        window,
        Some(UNDO_WINDOW_SECS),
        "the caller is told how long it has"
    );

    // `None` means NOT HELD, which is a different fact from a
    // window that has expired. Reporting `Some(0)` would claim the
    // message was kept and is merely too late to recover.
    assert_eq!(
        s.remember_deleted(
            &deleted_from("poly-batch", None),
            crate::demo_fixture::dlq_messages_for_env("poly-batch")[1].clone()
        )
        .await,
        None,
        "with no queue url there is nothing to restore to, and saying so is \
             not the same as offering a zero-second window"
    );

    // Listing names what is there, and what the offer does not cover.
    let listed: Value =
        serde_json::from_str(&s.tool_dlq_undo(&json!({})).await.expect("list")).expect("json");
    assert_eq!(
        listed["recoverable"][0]["message_id"],
        json!(id),
        "{listed}"
    );
    assert_eq!(
        listed["recoverable"][0]["task"],
        json!("Remove unattended jobs"),
        "the list must be readable without looking the id up: {listed}"
    );
    let note = listed["note"].as_str().expect("note");
    assert!(
        note.contains("restart"),
        "an in-memory buffer dies with the process: {note}"
    );
    assert!(note.contains("Purges are never recoverable"), "{note}");

    // Restoring reports what it could NOT restore. An "undo" that
    // silently changes the id and resets the retry count would be
    // the over-claim the foreclosure line exists to prevent.
    let done: Value = serde_json::from_str(
        &s.tool_dlq_undo(&json!({"message_id": id}))
            .await
            .expect("undo"),
    )
    .expect("json");
    assert_eq!(done["restored"], json!(true), "{done}");
    let not = done["not_restored"].as_array().expect("not_restored");
    assert_eq!(not.len(), 3, "{done}");
    let joined = not
        .iter()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(joined.contains("new one on send"), "{joined}");
    assert!(joined.contains("receive_count"), "{joined}");

    // An id that was never deleted is refused, and the refusal says
    // where to look rather than just failing.
    let err = s
        .tool_dlq_undo(&json!({"message_id": "never-existed"}))
        .await
        .expect_err("unknown id");
    assert!(err.to_string().contains("not recoverable"), "{err}");
    assert!(
        err.to_string().contains("no arguments"),
        "point at the listing: {err}"
    );
}

/// A resend is not recoverable, because nothing was destroyed.
///
/// The message still exists — on the main queue — so offering to
/// "restore" it would enqueue a second copy and call that a
/// recovery. Only a delete captures.
#[tokio::test]
async fn only_a_delete_fills_the_undo_buffer() {
    let s = Server::with_scope(true, false, crate::cli::mcp::WriteScope::All);
    let msg = crate::demo_fixture::dlq_messages_for_env("poly-batch")
        .into_iter()
        .next()
        .expect("fixture");
    let id = msg.id.clone();

    for verb in [WriteVerb::DlqResend, WriteVerb::DlqPurge] {
        let plan = s
            .tool_write_plan(verb, &json!({"env": "poly-batch", "message_id": id}))
            .await
            .expect("plan");
        let token = serde_json::from_str::<Value>(&plan).expect("json")["confirm_token"]
            .as_str()
            .expect("token")
            .to_string();
        s.tool_confirm_action(&json!({"confirm_token": token}))
            .await
            .expect("dispatch");
    }

    assert!(
        s.recoverable().await.is_empty(),
        "resend and purge destroy nothing this server can put back; offering an \
             undo for either would enqueue a duplicate and call it a recovery"
    );

    // The decision itself, so this holds for the LIVE dispatch path
    // too. The loop above runs in demo, where the real
    // `dispatch_dlq_message` is never called — a mutation there was
    // invisible to it until the condition moved into one function.
    assert!(captures_for_undo(WriteVerb::DlqDelete));
    for verb in WriteVerb::ALL {
        if verb != WriteVerb::DlqDelete {
            assert!(
                !captures_for_undo(verb),
                "{verb:?} must not offer an undo it cannot honour"
            );
        }
    }
}

/// The window expires, and the offer goes with it.
///
/// Time is advanced rather than waited on. Without this the buffer
/// could retain forever and every assertion above still passes —
/// an undo with no expiry is the more dangerous half of the claim,
/// because it is the half an operator relies on later.
#[tokio::test(start_paused = true)]
async fn the_undo_window_expires() {
    let s = Server::with_scope(true, false, crate::cli::mcp::WriteScope::All);
    let msg = crate::demo_fixture::dlq_messages_for_env("poly-batch")
        .into_iter()
        .next()
        .expect("fixture");
    let id = msg.id.clone();
    s.remember_deleted(
        &deleted_from("poly-batch", Some("https://q/poly-batch-dlq".into())),
        msg,
    )
    .await;
    assert_eq!(s.recoverable().await.len(), 1, "held immediately after");

    tokio::time::advance(std::time::Duration::from_secs(UNDO_WINDOW_SECS - 1)).await;
    assert_eq!(s.recoverable().await.len(), 1, "still inside the window");

    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    assert!(
        s.recoverable().await.is_empty(),
        "past the window, it is gone"
    );

    let err = s
        .tool_dlq_undo(&json!({"message_id": id}))
        .await
        .expect_err("expired");
    assert!(err.to_string().contains("not recoverable"), "{err}");
}

/// A client whose only SQS behaviour is `send_message`: succeed, or
/// fail with AccessDenied.
fn undo_client(send_ok: bool) -> crate::aws::AwsClient {
    use aws_sdk_sqs::operation::send_message::{SendMessageError, SendMessageOutput};
    let send = if send_ok {
        aws_smithy_mocks::mock!(aws_sdk_sqs::Client::send_message).then_output(|| {
            SendMessageOutput::builder()
                .message_id("restored-1")
                .build()
        })
    } else {
        aws_smithy_mocks::mock!(aws_sdk_sqs::Client::send_message).then_error(|| {
            SendMessageError::generic(
                aws_smithy_types::error::ErrorMetadata::builder()
                    .code("AccessDenied")
                    .message("not authorized to perform sqs:SendMessage")
                    .build(),
            )
        })
    };
    let sqs =
        aws_smithy_mocks::mock_client!(aws_sdk_sqs, aws_smithy_mocks::RuleMode::MatchAny, [&send]);
    let cfg = aws_config::SdkConfig::builder()
        .region(aws_config::Region::new("us-west-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    crate::aws::AwsClient::for_tests(
        aws_sdk_elasticbeanstalk::Client::new(&cfg),
        sqs,
        aws_sdk_cloudwatch::Client::new(&cfg),
        aws_sdk_cloudwatchlogs::Client::new(&cfg),
        aws_sdk_s3::Client::new(&cfg),
        aws_sdk_ec2::Client::new(&cfg),
    )
}

fn audit_delta_for(before: &str, env: &str) -> Vec<String> {
    let after =
        std::fs::read_to_string(crate::util::cache_dir().join("audit.log")).unwrap_or_default();
    after
        .strip_prefix(before)
        .expect("the audit log is append-only")
        .lines()
        .filter(|l| l.contains(env))
        .map(str::to_owned)
        .collect()
}

/// An undo restores a message at most ONCE, and is audited.
///
/// `recoverable()` hands back a copy, and the restore used to read
/// from that copy and leave the held entry in place — so calling
/// `dlq_undo` with the same id N times inside the window enqueued N
/// copies. And the restore, a real `SendMessage`, wrote no audit line:
/// the log recorded the delete and never that it was undone.
#[tokio::test]
async fn an_undo_restores_once_and_is_audited() {
    let env = "mcp-undo-once-probe-env";
    let s = Server::with_injected_client(
        crate::cli::mcp::WriteScope::All,
        crate::config::Config::default(),
        undo_client(true),
    );
    let msg = crate::demo_fixture::dlq_messages_for_env("poly-batch")
        .into_iter()
        .next()
        .expect("fixture");
    let id = msg.id.clone();
    s.remember_deleted(&deleted_from(env, Some("https://q/undo-dlq".into())), msg)
        .await;
    let before =
        std::fs::read_to_string(crate::util::cache_dir().join("audit.log")).unwrap_or_default();

    s.tool_dlq_undo(&json!({"message_id": id}))
        .await
        .expect("the first undo restores it");
    let err = s
        .tool_dlq_undo(&json!({"message_id": id}))
        .await
        .expect_err("a second undo of the same id must not restore it again");
    assert!(err.to_string().contains("not recoverable"), "{err}");

    let lines = audit_delta_for(&before, env);
    assert_eq!(
        lines.len(),
        2,
        "one dispatched and one completed line for the ONE restore: {lines:#?}"
    );
    assert!(lines[0].contains("stage=dispatched"), "{}", lines[0]);
    assert!(lines[1].contains("stage=completed"), "{}", lines[1]);
    for l in &lines {
        assert!(l.contains("action=dlq-undo"), "{l}");
        assert!(
            l.contains(&format!("message_id={id}")),
            "names the message: {l}"
        );
    }
}

/// An undo's audit line names a non-worker message the way the
/// delete's did, so the two correlate by task.
///
/// The delete recorded `NOT_A_WORKER_TASK`; the undo wrote `task=""`.
#[tokio::test]
async fn an_undo_of_a_non_worker_message_audits_the_deletes_task_label() {
    let env = "mcp-undo-task-label-probe-env";
    let s = Server::with_injected_client(
        crate::cli::mcp::WriteScope::All,
        crate::config::Config::default(),
        undo_client(true),
    );
    let mut msg = crate::demo_fixture::dlq_messages_for_env("poly-batch")
        .into_iter()
        .next()
        .expect("fixture");
    msg.task = None;
    let id = msg.id.clone();
    s.remember_deleted(&deleted_from(env, Some("https://q/undo-dlq".into())), msg)
        .await;
    let before =
        std::fs::read_to_string(crate::util::cache_dir().join("audit.log")).unwrap_or_default();
    s.tool_dlq_undo(&json!({"message_id": id}))
        .await
        .expect("restored");
    let lines = audit_delta_for(&before, env);
    assert!(!lines.is_empty(), "the restore is audited");
    for l in &lines {
        assert!(l.contains(NOT_A_WORKER_TASK), "the delete's label: {l}");
    }
}

/// A restore that FAILS leaves the message recoverable, and the
/// failure is on the record.
#[tokio::test]
async fn a_failed_undo_keeps_the_message_recoverable() {
    let env = "mcp-undo-fail-probe-env";
    let s = Server::with_injected_client(
        crate::cli::mcp::WriteScope::All,
        crate::config::Config::default(),
        undo_client(false),
    );
    let msg = crate::demo_fixture::dlq_messages_for_env("poly-batch")
        .into_iter()
        .next()
        .expect("fixture");
    let id = msg.id.clone();
    s.remember_deleted(&deleted_from(env, Some("https://q/undo-dlq".into())), msg)
        .await;
    let before =
        std::fs::read_to_string(crate::util::cache_dir().join("audit.log")).unwrap_or_default();

    s.tool_dlq_undo(&json!({"message_id": id}))
        .await
        .expect_err("the send was refused");
    assert!(
        s.recoverable().await.iter().any(|d| d.original_id == id),
        "a failed restore must not cost the operator their only copy"
    );
    // Held AND free: retrying at once reaches the send again rather than
    // being told another restore is in progress. "Still in the buffer"
    // alone was satisfied by a message left claimed after a failure.
    let retry = s
        .tool_dlq_undo(&json!({"message_id": id}))
        .await
        .expect_err("the send still fails");
    assert!(
        !retry.to_string().contains("mid-restore"),
        "a failed restore must release its claim: {retry}"
    );
    let lines = audit_delta_for(&before, env);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("stage=completed") && l.contains("err=")),
        "the failure is recorded: {lines:#?}"
    );
}

/// The held message remembers where it was deleted, so an undo goes
/// back through the same profile and region.
#[tokio::test]
async fn a_held_message_remembers_its_profile_and_region() {
    let s = Server::with_scope(true, false, crate::cli::mcp::WriteScope::All);
    let msg = crate::demo_fixture::dlq_messages_for_env("poly-batch")
        .into_iter()
        .next()
        .expect("fixture");
    let mut p = deleted_from("poly-batch", Some("https://q/dlq".into()));
    p.profile = Some("ops".into());
    p.region = Some("eu-west-2".into());
    s.remember_deleted(&p, msg).await;
    let held = s.recoverable().await;
    assert_eq!(held[0].profile.as_deref(), Some("ops"));
    assert_eq!(held[0].region.as_deref(), Some("eu-west-2"));
}

/// A restore whose call is DROPPED mid-flight does not lose the message.
///
/// `dlq_undo` runs under the 30 s tool timeout, and a timeout drops the
/// future — no error path runs. The first take-once fix removed the
/// held message before restoring and put it back only on an ERROR, so a
/// slow credential load or send lost the only copy of the body. Found by
/// the re-review. Simulated here exactly: claim, then never finish.
#[tokio::test(start_paused = true)]
async fn a_dropped_restore_does_not_lose_the_message() {
    let s = Server::with_scope(true, false, crate::cli::mcp::WriteScope::All);
    let msg = crate::demo_fixture::dlq_messages_for_env("poly-batch")
        .into_iter()
        .next()
        .expect("fixture");
    let id = msg.id.clone();
    s.remember_deleted(
        &deleted_from("poly-batch", Some("https://q/dlq".into())),
        msg,
    )
    .await;

    // A restore starts, and its future is dropped: no finish ever runs.
    assert!(matches!(s.claim_for_restore(&id).await, Claim::Claimed(_)));

    assert!(
        s.recoverable().await.iter().any(|d| d.original_id == id),
        "the message must still be held — the claim did not take it"
    );
    let err = s
        .tool_dlq_undo(&json!({"message_id": id}))
        .await
        .expect_err("a second undo must not race the one in flight");
    // It says when to retry: after a timeout there is no other call,
    // only a claim that has not lapsed yet.
    assert!(err.to_string().contains("mid-restore"), "{err}");
    assert!(
        err.to_string()
            .contains(&format!("at most {RESTORE_CLAIM_SECS}s")),
        "the retry horizon, computed from the claim: {err}"
    );
    // And the listing shows the claim.
    let listing = s.tool_dlq_undo(&json!({})).await.expect("listing");
    assert!(listing.contains("\"restoring\":true"), "{listing}");

    // The abandoned claim lapses, and the message can be restored.
    tokio::time::advance(std::time::Duration::from_secs(RESTORE_CLAIM_SECS + 1)).await;
    s.tool_dlq_undo(&json!({"message_id": id}))
        .await
        .expect("restorable again once the abandoned claim lapses");
    assert!(
        !s.recoverable().await.iter().any(|d| d.original_id == id),
        "and gone once restored"
    );
}

/// A full buffer evicts the oldest UNCLAIMED message, never one being
/// restored: if that restore then failed, the message would be gone.
#[tokio::test]
async fn capacity_eviction_spares_a_message_being_restored() {
    let s = Server::with_scope(true, false, crate::cli::mcp::WriteScope::All);
    let base = crate::demo_fixture::dlq_messages_for_env("poly-batch")
        .into_iter()
        .next()
        .expect("fixture");
    let plan = deleted_from("poly-batch", Some("https://q/dlq".into()));
    let msg = |n: usize| {
        let mut m = base.clone();
        m.id = format!("m-{n}");
        m
    };
    for n in 0..UNDO_CAPACITY {
        s.remember_deleted(&plan, msg(n)).await;
    }
    // The OLDEST is mid-restore when one more delete lands.
    assert!(matches!(
        s.claim_for_restore("m-0").await,
        Claim::Claimed(_)
    ));
    s.remember_deleted(&plan, msg(UNDO_CAPACITY)).await;

    let held: Vec<String> = s
        .recoverable()
        .await
        .into_iter()
        .map(|d| d.original_id)
        .collect();
    assert_eq!(held.len(), UNDO_CAPACITY, "the capacity bound holds");
    assert!(
        held.iter().any(|i| i == "m-0"),
        "the claimed message survives: {held:?}"
    );
    assert!(
        !held.iter().any(|i| i == "m-1"),
        "the oldest unclaimed one goes: {held:?}"
    );
}

/// Remembering a new message evicts ones that have expired.
///
/// `remember_deleted` prunes before it pushes, and the expiry test
/// above cannot see that: it pushes once, so the prune runs on an
/// empty buffer and every predicate behaves identically. The sweep
/// found three surviving mutants on that one comparison for
/// exactly this reason — the line was executed and could not
/// affect anything.
#[tokio::test(start_paused = true)]
async fn remembering_a_new_message_evicts_expired_ones() {
    let s = Server::with_scope(true, false, crate::cli::mcp::WriteScope::All);
    let msgs = crate::demo_fixture::dlq_messages_for_env("poly-batch");
    let (old_id, new_id) = (msgs[0].id.clone(), msgs[1].id.clone());

    s.remember_deleted(
        &deleted_from("poly-batch", Some("https://q/dlq".into())),
        msgs[0].clone(),
    )
    .await;
    tokio::time::advance(std::time::Duration::from_secs(UNDO_WINDOW_SECS + 1)).await;

    // The prune happens HERE, on a buffer holding one expired entry.
    s.remember_deleted(
        &deleted_from("poly-batch", Some("https://q/dlq".into())),
        msgs[1].clone(),
    )
    .await;

    // The BUFFER, before `recoverable()` prunes again on read.
    // Reading through `recoverable` cannot see this: it applies the
    // same filter, so anything the push-time prune leaks is
    // cleaned up before observation and every mutant looks
    // identical. The push-time prune exists to bound MEMORY, and
    // memory is only observable here.
    assert_eq!(
        s.deleted.lock().await.len(),
        1,
        "the expired entry must be evicted when the next one is remembered — \
             otherwise the buffer grows until someone happens to read it"
    );

    let held: Vec<String> = s
        .recoverable()
        .await
        .into_iter()
        .map(|d| d.original_id)
        .collect();
    assert_eq!(
        held,
        vec![new_id],
        "the expired entry must be dropped when the next one arrives, not \
             left to be filtered on read — a buffer that only prunes on read \
             grows without bound while nobody is looking"
    );
    assert!(!held.contains(&old_id));
}

/// Every ambiguous request shape is refused, not guessed at.
///
/// Each case here is one where picking a reading would act on a
/// set the agent did not ask for, and the operator would approve a
/// count that does not match what happens.
#[test]
fn an_ambiguous_batch_request_is_refused() {
    let ok = |v: Value| requested_message_ids(&v).expect("valid");
    assert_eq!(ok(json!({"message_id": "a"})), vec!["a"]);
    assert_eq!(ok(json!({"message_ids": ["a", "b"]})), vec!["a", "b"]);
    // An explicit null is the same as absent — clients send it for
    // an omitted optional, and treating it as "an empty selection"
    // would turn a missing argument into a no-op success.
    assert_eq!(
        ok(json!({"message_id": "a", "message_ids": null})),
        vec!["a"]
    );

    let err = |v: Value| requested_message_ids(&v).expect_err("must refuse");
    let both = err(json!({"message_id": "a", "message_ids": ["b"]}));
    assert!(
        both.contains("not both"),
        "no precedence can drop one silently: {both}"
    );
    assert!(err(json!({"env": "x"})).contains("required"));
    assert!(err(json!({"message_ids": []})).contains("empty"));
    assert!(err(json!({"message_ids": "a"})).contains("array"));
    let typed = err(json!({"message_ids": ["a", 7]}));
    assert!(
        typed.contains("message_ids[1]") && typed.contains("not a string"),
        "it must say WHICH element, or the agent has to guess: {typed}"
    );

    let dup = err(json!({"message_ids": ["a", "b", "a"]}));
    assert!(
        dup.contains("named twice"),
        "deduplicating silently would show the operator a count that does not \
             match the list: {dup}"
    );

    let over: Vec<String> = (0..=DLQ_BATCH_CAP).map(|i| format!("id-{i}")).collect();
    let big = err(json!({"message_ids": over}));
    assert!(
        big.contains(&DLQ_BATCH_CAP.to_string()) && big.contains("dlq_purge"),
        "over the cap must name the cap and the alternative, not just refuse: {big}"
    );
    assert!(
        !big.contains("truncat"),
        "and must never offer to truncate — dispatching a subset while reporting \
             the whole is the worst outcome available: {big}"
    );
}

/// At the cap is allowed; one past it is not.
#[test]
fn the_cap_is_inclusive() {
    let at: Vec<String> = (0..DLQ_BATCH_CAP).map(|i| format!("id-{i}")).collect();
    assert_eq!(
        requested_message_ids(&json!({"message_ids": at}))
            .expect("the cap itself is allowed")
            .len(),
        DLQ_BATCH_CAP
    );
}

/// A failed item is PRESENT and says why.
///
/// Rule 6 across a set: an agent given four results for a
/// five-message plan cannot tell a dropped item from a truncated
/// list, so it has to infer — and it will infer success.
#[test]
fn a_failed_item_appears_in_the_report_with_its_reason() {
    let t = DlqTarget {
        id: "m-1".into(),
        task: "Nightly sweep".into(),
    };
    let ok = render_dlq_item(&t, &Ok(None));
    assert!(ok.contains("\"ok\":true") && ok.contains("m-1") && ok.contains("Nightly sweep"));
    assert!(
        !ok.contains("error"),
        "a success carries no error field: {ok}"
    );

    let bad = render_dlq_item(&t, &Err("queue vanished".into()));
    assert!(bad.contains("\"ok\":false"), "{bad}");
    assert!(
        bad.contains("m-1") && bad.contains("queue vanished"),
        "a failure names the message AND the reason, or the agent cannot report \
             which of five failed: {bad}"
    );
    // Both shapes must be parseable — these go into a JSON array.
    for line in [ok, bad] {
        serde_json::from_str::<Value>(&line).expect("each item is valid JSON");
    }
}

/// The operator sees the list, never a count.
#[test]
fn a_batch_ask_enumerates_the_messages() {
    let targets: Vec<DlqTarget> = (1..=3)
        .map(|i| DlqTarget {
            id: format!("m-{i}"),
            task: format!("Task {i}"),
        })
        .collect();
    let p = PendingWrite {
        token: "t".into(),
        verb: WriteVerb::DlqDelete,
        env: "poly-batch".into(),
        version: None,
        settings: Vec::new(),
        profile: None,
        region: None,
        expires_at: tokio::time::Instant::now(),
        dlq_visible: None,
        caller: CallerIdentity::Known {
            arn: "arn:aws:iam::123456789012:user/test".into(),
            account: "123456789012".into(),
        },
        name_retry_used: false,
        dlq_targets: targets,
        dlq_url: Some("https://sqs/q-dlq".into()),
        dlq_main_url: None,
    };
    let summary = ask_summary(&p);
    for i in 1..=3 {
        assert!(
            summary.contains(&format!("m-{i}")) && summary.contains(&format!("Task {i}")),
            "every message must be readable in the dialog — a summarised count is \
                 something to agree with, a list is something to read: {summary}"
        );
    }
    assert!(summary.contains("poly-batch"), "{summary}");
    assert!(
        summary.contains("3 messages"),
        "and the count, so a truncated dialog still shows the scale: {summary}"
    );
}

/// The foreclosure line scales with the batch.
///
/// "The message is destroyed" under a plan for nine of them
/// understates the cost in the one sentence written to stop
/// exactly that.
#[test]
fn the_foreclosure_line_counts_the_batch() {
    let one = forecloses(WriteVerb::DlqDelete, None, 1);
    let many = forecloses(WriteVerb::DlqDelete, None, 9);
    assert!(one.contains("The message is destroyed"), "{one}");
    assert!(
        many.contains("All 9 messages are destroyed"),
        "a batch must say how many it destroys: {many}"
    );
    assert!(
        many.contains("dlq_undo"),
        "and still name the one recovery path there is: {many}"
    );
    let resend = forecloses(WriteVerb::DlqResend, None, 4);
    assert!(resend.contains("All 4 messages"), "{resend}");
}

/// A batch writes one audit line per message, each naming its own.
///
/// The whole reason DLQ dispatch has its own audited path. One
/// line for five deletes records that five messages went from
/// `poly-batch` and leaves the log unable to say which — and for a
/// delete, the log is the only place that answer can still exist,
/// because the message does not.
#[test]
fn a_batch_audits_every_message_separately() {
    let targets: Vec<DlqTarget> = (1..=4)
        .map(|i| DlqTarget {
            id: format!("m-{i}"),
            task: format!("Task {i}"),
        })
        .collect();
    let lines = dlq_audit_lines("claude-code", true, &targets);
    assert_eq!(lines.len(), 4, "one line per message, not one per batch");

    for (i, line) in lines.iter().enumerate() {
        let get = |k: &str| {
            line.iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(
            get("message_id"),
            Some(format!("m-{}", i + 1)),
            "line {i} must name ITS message: {line:?}"
        );
        assert_eq!(
            get("task"),
            Some(format!("Task {}", i + 1)),
            "and its task, paired correctly — a filtered id list against an \
                 unfiltered task list is how every line ends up naming the wrong \
                 task: {line:?}"
        );
        assert_eq!(get("via"), Some("mcp".to_string()));
        assert_eq!(get("can_ask"), Some("true".to_string()));
    }

    // No two lines share an id, which a `clone()` of the first
    // target would produce and every per-line assertion above
    // would still pass.
    let ids: std::collections::HashSet<_> = lines
        .iter()
        .filter_map(|l| l.iter().find(|(k, _)| *k == "message_id").map(|(_, v)| v))
        .collect();
    assert_eq!(ids.len(), 4, "four distinct messages, four distinct lines");

    // An empty batch writes nothing — not one line with no id.
    assert!(dlq_audit_lines("c", true, &[]).is_empty());
}

/// An unknown identity is SAID, not omitted.
///
/// Rule 6 on the one field whose absence is most reassuring: a
/// plan that quietly dropped `identity` when STS was denied would
/// show nothing where there should be something, and nothing
/// reads as fine.
#[test]
fn an_unknown_caller_is_rendered_rather_than_dropped() {
    let known = CallerIdentity::Known {
        arn: "arn:aws:sts::123456789012:assumed-role/Admin/sess".into(),
        account: "123456789012".into(),
    };
    assert_eq!(
        known.line(),
        "as arn:aws:sts::123456789012:assumed-role/Admin/sess",
        "the FULL arn — assumed-role/Deploy and assumed-role/Admin differ by one \
             word, and the account number is only in the full form"
    );
    let j: Value =
        serde_json::from_str(&format!("{{\"identity\":{}}}", known.json())).expect("valid JSON");
    assert_eq!(j["identity"]["account"], json!("123456789012"));

    let unknown = CallerIdentity::Unknown {
        why: "AccessDenied".into(),
    };
    let line = unknown.line();
    assert!(
        line.contains("UNKNOWN") && line.contains("AccessDenied"),
        "the operator must be told they are approving a write whose identity \
             could not be established, and why: {line}"
    );
    // Null AND a reason — a bare null claims there is no identity,
    // which is never true of a call about to be made with one.
    let raw = format!("{{\"identity\":{}}}", unknown.json());
    let j: Value = serde_json::from_str(&raw).expect("valid JSON: {raw}");
    assert_eq!(j["identity"], Value::Null);
    assert_eq!(j["identity_error"], json!("AccessDenied"));
}

/// The confirmation names the identity.
#[test]
fn the_ask_says_whose_credentials_it_would_use() {
    let mut p = PendingWrite {
        token: "t".into(),
        verb: WriteVerb::Terminate,
        env: "poly-prod".into(),
        version: None,
        settings: Vec::new(),
        profile: None,
        region: None,
        dlq_visible: None,
        caller: CallerIdentity::Known {
            arn: "arn:aws:iam::999:role/Admin".into(),
            account: "999".into(),
        },
        expires_at: tokio::time::Instant::now(),
        name_retry_used: false,
        dlq_targets: Vec::new(),
        dlq_url: None,
        dlq_main_url: None,
    };
    let s = ask_summary(&p);
    assert!(
        s.contains("arn:aws:iam::999:role/Admin"),
        "approving a terminate without being shown whose credentials it goes \
             out under is the gap this closes: {s}"
    );
    assert!(s.contains("poly-prod"), "{s}");

    p.caller = CallerIdentity::Unknown {
        why: "sts denied".into(),
    };
    assert!(
        ask_summary(&p).contains("UNKNOWN"),
        "and an unresolved identity must be visible in the dialog too"
    );
}

/// Every policy refusal is a `Refused`; nothing else is.
///
/// The type's whole claim: holding a `Refused` means an audit line
/// exists. A path that refuses by policy and returns `Invalid`
/// silently records nothing — which is the 0.40 defect, and was
/// still live in `tool_confirm_action`'s scope check until
/// classifying these sites for this type turned it up.
#[tokio::test]
async fn a_policy_refusal_is_typed_as_one() {
    // Read-only server: confirm_action is not advertised, but a
    // client with a cached tool list reaches the body.
    let s = Server::with_scope(true, false, crate::cli::mcp::WriteScope::None);
    let err = s
        .tool_confirm_action(&json!({"confirm_token": "anything"}))
        .await
        .expect_err("a read-only server must refuse");
    assert!(
        matches!(err, WriteError::Refused(_)),
        "the scope check is a POLICY refusal and must be typed as one, or it \
             audits nothing: {err}"
    );
    assert!(err.to_string().contains("--allow-writes"), "{err}");

    // And a malformed request is NOT a refusal — otherwise
    // `is_refusal` is satisfied by calling everything one, and the
    // audit log fills with attempts nobody made.
    let s = Server::with_scope(true, false, crate::cli::mcp::WriteScope::All);
    for bad in [json!({}), json!({"confirm_token": "no-such-token"})] {
        let err = s
            .tool_confirm_action(&bad)
            .await
            .expect_err("malformed must fail");
        assert!(
            !matches!(err, WriteError::Refused(_)),
            "a missing or unknown token is not a policy refusal — nothing was \
                 blocked, so there is no attempt to record: {err}"
        );
    }
}

/// An out-of-scope verb refuses, and is typed as a refusal.
#[tokio::test]
async fn a_verb_outside_the_grant_is_a_typed_refusal() {
    let s = Server::with_scope(
        true,
        false,
        super::super::WriteScope::Only(vec!["dlq_delete".into()]),
    );
    let err = s
        .tool_write_plan(WriteVerb::Terminate, &json!({"env": "poly-prod"}))
        .await
        .expect_err("terminate is not granted");
    assert!(matches!(err, WriteError::Refused(_)), "{err}");
    assert!(
        err.to_string().contains("not in this server's write scope"),
        "{err}"
    );
}

/// `Audited` cannot be forged.
///
/// The privacy of its field is what makes `Refused` mean
/// something, and privacy is easy to widen by accident while
/// chasing a compile error. This fails if the field gains a
/// visibility keyword, or if a second constructor appears
/// alongside `record`.
#[test]
fn a_refusal_cannot_be_constructed_without_recording_it() {
    let body = crate::app::tests::scan::production_source("cli/mcp/writes.rs");
    let start = body
        .find("mod audited {")
        .expect("the audited module must exist");
    let end = body[start..].find("\n}\n").expect("its body ends") + start;
    let module = &body[start..end];

    assert!(
        module.contains("struct Audited(String);"),
        "the field must stay PRIVATE — a `pub` on it lets any code in this \
             module build a Refused without an audit line, which is the entire \
             property the type carries"
    );
    // Exactly one way to make one from nothing.
    let ctors = module.matches("-> Self {").count();
    assert_eq!(
        ctors, 2,
        "expected exactly two: `record`, which writes the line, and \
             `map_message`, which only rewrites one that exists. A third is a \
             way to hold a refusal that was never recorded — which is the hatch \
             this type exists to remove."
    );
    assert!(
        module.contains("append_action_refused"),
        "record must audit"
    );

    // Nothing outside the module may name the tuple constructor.
    let elsewhere = body.replace(module, "");
    assert!(
        !elsewhere.contains("Audited("),
        "`Audited(..)` outside its module means the field is reachable"
    );
}

/// The dialog names the CHANGES, not just the verb.
///
/// `set_option` can re-point an environment variable at an
/// attacker's endpoint. "SetOption on api-prod" asks the operator
/// to approve an unknown edit to an unknown key — the plan JSON
/// carried the detail all along, and the plan JSON is read by the
/// agent, which is the party the dialog exists to check.
#[test]
fn a_set_option_dialog_shows_what_changes() {
    let mut p = pending_for(WriteVerb::SetOption);
    p.settings = vec![
        (
            "aws:elasticbeanstalk:application:environment".into(),
            "WEBHOOK_URL".into(),
            "https://evil.example/collect".into(),
        ),
        ("aws:autoscaling:asg".into(), "MinSize".into(), "1".into()),
    ];
    let s = ask_summary(&p);
    for needle in [
        "WEBHOOK_URL",
        "https://evil.example/collect",
        "MinSize",
        "aws:autoscaling:asg",
    ] {
        assert!(
            s.contains(needle),
            "the operator must see the actual change, or the approval is blind: \
                 {needle:?} missing from {s:?}"
        );
    }
    assert!(
        !s.contains("shown above"),
        "the old foreclosure line pointed at the agent's transcript, which the \
             operator reading this dialog is not looking at: {s}"
    );
}

/// A purge dialog says how much it destroys.
#[test]
fn a_purge_dialog_carries_the_queue_depth() {
    let mut p = pending_for(WriteVerb::DlqPurge);
    p.dlq_visible = Some(12);
    let s = ask_summary(&p);
    assert!(
        s.contains("12"),
        "the plan JSON showed the agent `messages_now`; the operator got \
             \"every message in the queue\" with no number: {s}"
    );
}

/// Untrusted strings cannot reshape the operator's sentence.
///
/// A DLQ task name is whatever the application POSTed to the
/// queue. The dialog is one sentence a human reads before
/// destroying something, so a newline plus a bullet forges a row,
/// and a bidi override reverses the line around it. This is prompt
/// injection aimed at a person.
#[test]
fn a_hostile_task_name_cannot_forge_dialog_lines() {
    let evil = "Nightly sweep\n  • Something harmless (m-999)\u{202E}reversed\u{0007}";
    let clean = sanitize_for_ask(evil);
    assert!(!clean.contains('\n'), "no newline may survive: {clean:?}");
    assert!(!clean.contains('\u{202E}'), "no bidi override: {clean:?}");
    // Two the first version missed: an invisible bidi control that
    // `is_control()` does not report, and text that renders as
    // nothing at all.
    assert_eq!(
        sanitize_for_ask("a\u{061C}b"),
        "a\u{FFFD}b",
        "U+061C ARABIC LETTER MARK reorders visibly and is not a control char"
    );
    assert_eq!(
        sanitize_for_ask("a\u{E0041}b"),
        "a\u{FFFD}b",
        "the TAG block is invisible by design"
    );
    assert!(!clean.contains('\u{0007}'), "no control chars: {clean:?}");
    assert!(
        clean.starts_with("Nightly sweep"),
        "legible text survives: {clean:?}"
    );

    // And it is applied at EVERY call site — the function existing
    // proves nothing, and the single-message and batch arms
    // interpolate through different expressions. A first version
    // of this test covered only the single arm, and a mutation
    // removing the batch arm's sanitiser passed clean.
    //
    // One case per site: env, version, single task/id, batch
    // task/id, and each setting field.
    let mut single = pending_for(WriteVerb::DlqDelete);
    single.dlq_targets = vec![DlqTarget {
        id: evil.to_string(),
        task: evil.to_string(),
    }];

    let mut batch = pending_for(WriteVerb::DlqDelete);
    batch.dlq_targets = (0..2)
        .map(|i| DlqTarget {
            id: format!("{evil}-{i}"),
            task: evil.to_string(),
        })
        .collect();

    let mut opts = pending_for(WriteVerb::SetOption);
    opts.settings = vec![(evil.to_string(), evil.to_string(), evil.to_string())];

    let mut deploy = pending_for(WriteVerb::Deploy);
    deploy.version = Some(evil.to_string());

    let mut env = pending_for(WriteVerb::Restart);
    env.env = evil.to_string();

    // The identity line too: `CallerIdentity::Unknown` carries a
    // raw SDK error chain, which is remote prose, and it lands in
    // the same sentence. It was the one field the "every foreign
    // field" invariant did not actually cover.
    let mut ident = pending_for(WriteVerb::Restart);
    ident.caller = CallerIdentity::Unknown {
        why: evil.to_string(),
    };

    for (name, plan, allowed_bullets) in [
        ("identity error", ident, 0),
        ("single message", single, 0),
        ("batch", batch, 2),
        ("set_option", opts, 1),
        ("deploy version", deploy, 0),
        ("env name", env, 0),
    ] {
        let rendered = ask_summary(&plan);
        let bullets = rendered
            .lines()
            .filter(|l| l.trim_start().starts_with('•'))
            .count();
        assert_eq!(
            bullets,
            allowed_bullets,
            "{name}: the payload forged {} extra bullet rows — every line the \
                 operator reads must come from ebman, not from the queue: {rendered:?}",
            bullets.saturating_sub(allowed_bullets)
        );
        assert!(
            !rendered.contains('\u{202E}') && !rendered.contains('\u{0007}'),
            "{name}: formatting/control characters reached the dialog: {rendered:?}"
        );
    }
}

/// Long fields cannot push the rest of the sentence out of view.
#[test]
fn a_huge_task_name_is_capped() {
    let long = "A".repeat(5_000);
    let clean = sanitize_for_ask(&long);
    assert!(
        clean.chars().count() <= 121,
        "capped: {}",
        clean.chars().count()
    );
    assert!(clean.ends_with('…'), "and says it was truncated");
}

/// The delete plan a held message came from.
fn deleted_from(env: &str, dlq_url: Option<String>) -> PendingWrite {
    let mut p = pending_for(WriteVerb::DlqDelete);
    p.env = env.into();
    p.dlq_url = dlq_url;
    p
}

fn pending_for(verb: WriteVerb) -> PendingWrite {
    PendingWrite {
        token: "t".into(),
        verb,
        env: "api-prod".into(),
        version: None,
        settings: Vec::new(),
        profile: None,
        region: None,
        dlq_visible: None,
        caller: CallerIdentity::Known {
            arn: "arn:aws:iam::1:user/t".into(),
            account: "1".into(),
        },
        expires_at: tokio::time::Instant::now(),
        name_retry_used: false,
        dlq_targets: Vec::new(),
        dlq_url: None,
        dlq_main_url: None,
    }
}

/// A value too long to show whole is refused, not abbreviated.
///
/// The attack the cap invited:
/// `https://payments.example/callback/<90 filler>@evil.example/x`
/// renders as a benign-looking prefix and an ellipsis, because the
/// `@` that makes everything before it userinfo sits past the cut.
/// The operator reads one host and approves another.
#[tokio::test]
async fn an_unshowable_setting_value_is_refused_rather_than_truncated() {
    let s = Server::with_scope(true, false, crate::cli::mcp::WriteScope::All);
    let sneaky = format!(
        "https://payments.example/callback/{}@evil.example/steal",
        "x".repeat(SET_OPTION_FIELD_MAX)
    );
    let err = s
        .tool_write_plan(
            WriteVerb::SetOption,
            &json!({"env": "poly-batch", "settings": [{
                "namespace": "aws:elasticbeanstalk:application:environment",
                "name": "WEBHOOK_URL",
                "value": sneaky,
            }]}),
        )
        .await
        .expect_err("a value the dialog cannot show whole must be refused");
    let text = err.to_string();
    assert!(
        text.contains(&SET_OPTION_FIELD_MAX.to_string()),
        "the refusal must name the limit: {text}"
    );
    assert!(
        text.contains("whole rather than truncated"),
        "and say WHY, or this reads as arbitrary API fussiness to route \
             around: {text}"
    );

    // A value at the limit still plans, and renders untruncated.
    let ok_value = "y".repeat(SET_OPTION_FIELD_MAX);
    s.tool_write_plan(
        WriteVerb::SetOption,
        &json!({"env": "poly-batch", "settings": [{
            "namespace": "aws:elasticbeanstalk:application:environment",
            "name": "WEBHOOK_URL",
            "value": &ok_value,
        }]}),
    )
    .await
    .expect("at the limit is allowed");
    let st = s.writes.lock().await;
    let p = st.pending.as_ref().expect("a plan");
    let summary = ask_summary(p);
    assert!(
        summary.contains(&ok_value),
        "the dialog must carry the value WHOLE — a truncated one lets the \
             operator approve a prefix while the full value dispatches"
    );
    assert!(
        !summary.contains('…'),
        "and must not have abbreviated it: {summary}"
    );
}
