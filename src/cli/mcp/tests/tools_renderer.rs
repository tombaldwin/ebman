//! Renderer tests for `cli::mcp::tools` — how tool results are shaped.
//!
//! Moved verbatim out of `tools.rs`. Attached with `#[path]` so the
//! module keeps its parent — and with it `use super::*` and access to
//! `tools`'s private items — while the file lives beside the other
//! MCP tests. Same reason as `tests/writes.rs`: re-parenting would
//! have cost `pub(crate)` on items that have no business being
//! crate-visible.
//!
//! Was `mod renderer_tests` in `tools.rs`; kept a separate module
//! rather than merged, so the test paths do not change.

use super::*;

/// Every renderer below is reached ONLY through an AWS path, so
/// none of them had a test — mutating each one during a pre-release
/// review left the whole suite green. They are the JSON an agent
/// actually consumes, which makes them the last place that should
/// be unpinned.
///
/// Testing the SHAPE, not a golden string: these must stay parseable
/// and carry their fields, and rewording a key is a deliberate
/// wire-format change that should break something.
fn parse(json: &str) -> Value {
    serde_json::from_str(json)
        .unwrap_or_else(|e| panic!("renderer emitted invalid JSON: {e}\n{json}"))
}

#[test]
fn alarms_render_state_and_reason() {
    let alarms = vec![aws::CwAlarm {
        name: "prod-5xx".into(),
        state: "ALARM".into(),
        state_reason: "Threshold crossed: 3 datapoints".into(),
        metric_name: "HTTPCode_Target_5XX_Count".into(),
        namespace: "AWS/ApplicationELB".into(),
    }];
    let v = parse(&render_alarms_json(&alarms));
    assert_eq!(v[0]["name"], "prod-5xx");
    assert_eq!(
        v[0]["state"], "ALARM",
        "the state is the whole point of listing an alarm"
    );
    assert!(
        v[0]["reason"]
            .as_str()
            .is_some_and(|r| r.contains("Threshold")),
        "the reason is what makes an ALARM actionable: {v}"
    );
    assert_eq!(render_alarms_json(&[]), "[]", "no alarms is an empty array");
}

#[test]
fn instances_render_their_causes() {
    let instances = vec![aws::Instance {
        id: "i-0abc".into(),
        health: "Degraded".into(),
        color: "Yellow".into(),
        causes: vec!["ELB health failing".into(), "High CPU".into()],
        instance_type: "t3.medium".into(),
        availability_zone: "us-west-1a".into(),
        launched_at: None,
    }];
    let v = parse(&render_instances_json(&instances));
    assert_eq!(v[0]["id"], "i-0abc");
    assert_eq!(v[0]["health"], "Degraded");
    let causes = v[0]["causes"].as_array().expect("causes array");
    assert_eq!(
        causes.len(),
        2,
        "causes are the diagnosis — dropping them leaves only a colour: {v}"
    );
    assert_eq!(
        v[0]["launched_at"],
        Value::Null,
        "an absent launch time must be null, not a defaulted now"
    );
}

#[test]
fn versions_are_capped_and_ordered_as_given() {
    let many: Vec<aws::AppVersion> = (0..25)
        .map(|i| aws::AppVersion {
            label: format!("build-{i}"),
            description: String::new(),
            created: None,
        })
        .collect();
    let v = parse(&render_versions_json(&many));
    let arr = v.as_array().expect("array");
    assert_eq!(
        arr.len(),
        10,
        "the bundle caps versions — an unbounded list buries the rest \
             of the report"
    );
    assert_eq!(
        arr[0]["label"], "build-0",
        "and keeps the caller's order rather than re-sorting"
    );
}

#[test]
fn recent_logs_render_a_timestamp_per_line() {
    let events = vec![(
        "/aws/elasticbeanstalk/api-prod/var/log/web.stdout.log".to_string(),
        crate::aws::LogEvent {
            timestamp_ms: 1_789_625_040_068,
            stream: "i-0abc".into(),
            message: "task finished".into(),
        },
    )];
    let v = parse(&render_recent_logs_json(
        "api-prod",
        &[],
        true,
        false,
        &events,
    ));
    assert_eq!(v["env"], "api-prod");
    assert_eq!(v["complete"], true);
    assert_eq!(
        v["truncated_by_limit"], false,
        "a complete scan that returned everything says so on both axes"
    );
    let e = &v["events"][0];
    assert!(
        e["timestamp"]
            .as_str()
            .is_some_and(|t| t.starts_with("2026-")),
        "a log line without a timestamp cannot be correlated with anything: {v}"
    );
    assert_eq!(e["stream"], "i-0abc");
    assert_eq!(e["message"], "task finished");
}

/// A demo peek reports what it actually found.
///
/// The defect this replaces: `peeked: true, messages: []` beside a
/// dead-letter depth of 12 — an explicit all-clear on a queue the
/// server never opened, next to the number contradicting it. An
/// agent triaging that env reads "the DLQ is empty" and stops.
///
/// Fixed twice. First by refusing to claim a look that never
/// happened (`peeked: false`), then properly, by giving demo a
/// fixture with messages in it — so the answer is honest AND the
/// triage story is walkable without an AWS account.
///
/// The assertion is the INVARIANT, not the current value: a peek
/// that reports success must return something when the queue is
/// not empty. Flipping `peeked` to a constant passes neither half.
#[tokio::test]
async fn a_demo_peek_reports_what_it_found() {
    let s = Server::with_scope(true, false, crate::cli::mcp::WriteScope::None);
    let read = |v: &Value| -> (bool, usize, u64) {
        (
            v["peeked"].as_bool().expect("peeked"),
            v["messages"].as_array().map(Vec::len).expect("messages"),
            v["dead_letter_queue"]["stats"]["visible"]
                .as_u64()
                .unwrap_or(0),
        )
    };

    let asked: Value = serde_json::from_str(
        &s.tool_worker_queues(&json!({"env": "poly-batch", "peek": true}))
            .await
            .expect("demo worker_queues"),
    )
    .expect("json");
    let (peeked, msgs, visible) = read(&asked);
    assert!(visible > 0, "this test needs a non-empty demo DLQ: {asked}");
    assert!(
        peeked,
        "a peek was asked for and the fixture was read: {asked}"
    );
    assert!(
        msgs > 0,
        "reporting a successful peek of a queue holding {visible} while returning \
             nothing is the false all-clear this guards: {asked}"
    );
    assert!(
        msgs < visible as usize,
        "a peek samples; returning the whole depth teaches the shape wrong: {asked}"
    );

    // Not asked for: we did not look, and say so.
    let unasked: Value = serde_json::from_str(
        &s.tool_worker_queues(&json!({"env": "poly-batch"}))
            .await
            .expect("demo worker_queues"),
    )
    .expect("json");
    let (peeked, msgs, _) = read(&unasked);
    assert!(!peeked, "no peek was asked for: {unasked}");
    assert_eq!(msgs, 0, "{unasked}");

    // A WEB env in demo: no queue to look at, so the demo path must
    // apply the same `peekable` gate the live path does. It did
    // not — it passed the request flag through and answered
    // `peeked: true` beside a null queue, which is "we looked, it
    // was empty" about a queue that does not exist. The live fix
    // for that shipped three commits earlier; this is the same
    // defect on the path agents actually rehearse against, and the
    // test above cannot see it because `poly-batch` HAS a queue.
    let web: Value = serde_json::from_str(
        &s.tool_worker_queues(&json!({"env": "poly-prod-api", "peek": true}))
            .await
            .expect("demo worker_queues"),
    )
    .expect("json");
    assert_eq!(
        web["peeked"],
        json!(false),
        "there is no queue here, so no look happened however it was asked for: {web}"
    );
    assert!(web["dead_letter_queue"]["url"].is_null(), "{web}");
    assert!(
        web["reason"]
            .as_str()
            .is_some_and(|r| r.contains("web tier")),
        "and the reason must say why there is nothing: {web}"
    );

    // The fixture carries both shapes a consumer must handle: an EB
    // scheduled task, and a message that is not one at all.
    let tasks: Vec<&Value> = asked["messages"].as_array().expect("arr").iter().collect();
    assert!(
        tasks.iter().any(|m| m["task"]["name"].is_string()),
        "one message must be an EB worker task: {asked}"
    );
    assert!(
        tasks.iter().any(|m| m["task"].is_null()),
        "and one must not, so `task: null` is exercised: {asked}"
    );
}

#[test]
fn worker_queues_render_the_task_and_the_origin() {
    let queues = aws::WorkerQueues {
        main_url: Some("https://sqs/main".into()),
        dlq_url: Some("https://sqs/main-dlq".into()),
        main_stats: Some(crate::aws::QueueStats {
            visible: 0,
            in_flight: 2,
            delayed: 0,
        }),
        dlq_stats: Some(crate::aws::QueueStats {
            visible: 1,
            in_flight: 0,
            delayed: 0,
        }),
        dlq_origin: Some(aws::DlqOrigin::Derived),
    };
    let msgs = vec![crate::aws::QueueMessage {
        id: "m-1".into(),
        attributes: Vec::new(),
        receipt_handle: String::new(),
        body: "elasticbeanstalk scheduled job".into(),
        receive_count: 4,
        sent_at: None,
        task: Some(crate::aws::SqsdTask {
            name: Some("Remove unattended jobs".into()),
            path: Some("/STCleanupUnattendedJobs.do".into()),
            scheduled_time_raw: Some("2026-09-17 06:04:00 UTC".into()),
            scheduled_at: None,
        }),
    }];
    let v = parse(&render_worker_queues_json(&queues, &msgs, true, true, None));

    assert_eq!(v["dead_letter_queue"]["stats"]["visible"], 1);
    assert_eq!(
        v["dead_letter_queue"]["origin"], "derived",
        "a derived url returning nothing is ordinary; a reported one \
             that does is an anomaly — the consumer cannot tell without this"
    );
    assert_eq!(v["peeked"], true);
    let t = &v["messages"][0]["task"];
    assert_eq!(
        t["name"], "Remove unattended jobs",
        "the task name is the answer EB's health text does not give: {v}"
    );
    assert_eq!(t["scheduled_time"], "2026-09-17 06:04:00 UTC");
    assert_eq!(v["messages"][0]["receive_count"], 4);

    // A message that is not an EB task renders task: null, not an
    // empty object that reads as a task with no name.
    let plain = vec![crate::aws::QueueMessage {
        id: "m-2".into(),
        attributes: Vec::new(),
        receipt_handle: String::new(),
        body: "{}".into(),
        receive_count: 1,
        sent_at: None,
        task: None,
    }];
    let v = parse(&render_worker_queues_json(
        &queues, &plain, true, true, None,
    ));
    assert_eq!(v["messages"][0]["task"], Value::Null);
}

/// A web-tier env has no queues at all: nulls, not an error and not
/// zeroes that read as "the queue is empty".
#[test]
fn an_env_with_no_queues_renders_nulls() {
    let none = aws::WorkerQueues {
        main_url: None,
        dlq_url: None,
        main_stats: None,
        dlq_stats: None,
        dlq_origin: None,
    };
    let v = parse(&render_worker_queues_json(&none, &[], false, true, None));
    assert_eq!(v["main_queue"]["url"], Value::Null);
    assert_eq!(
        v["main_queue"]["stats"],
        Value::Null,
        "no queue is not a queue with zero messages"
    );
    assert_eq!(v["dead_letter_queue"]["origin"], Value::Null);
    assert_eq!(v["peeked"], false);
}

/// Per-group results must merge by TIME and cap to `limit`.
///
/// Groups are fetched and capped independently, so the union
/// arrives ordered by group. Without the sort, the tail of the
/// array is the last group's oldest lines rather than the fleet's
/// newest — the same wrong answer this whole tool exists to avoid,
/// arriving by a different route.
#[test]
fn groups_merge_by_time_and_cap_to_the_limit() {
    let ev = |group: &str, ts: i64| {
        (
            group.to_string(),
            crate::aws::LogEvent {
                timestamp_ms: ts,
                stream: "s".into(),
                message: format!("{group}@{ts}"),
            },
        )
    };
    // Arrives grouped: web's three, then worker's three, interleaved
    // in time.
    let mut events = vec![
        ev("web", 10),
        ev("web", 30),
        ev("web", 50),
        ev("worker", 20),
        ev("worker", 40),
        ev("worker", 60),
    ];
    merge_newest(&mut events, 3);
    assert_eq!(
        events
            .iter()
            .map(|(_, e)| e.timestamp_ms)
            .collect::<Vec<_>>(),
        vec![40, 50, 60],
        "the newest three across BOTH groups, in time order"
    );

    // Under the limit: everything survives, still time-ordered.
    let mut few = vec![ev("worker", 9), ev("web", 1)];
    merge_newest(&mut few, 50);
    assert_eq!(
        few.iter().map(|(_, e)| e.timestamp_ms).collect::<Vec<_>>(),
        vec![1, 9]
    );
}

/// The group cap must report truncation, not hide it.
///
/// The dropped groups might hold the newest lines, so a silently
/// truncated fan-out answers "here are the newest" with the newest
/// of an arbitrary subset — the same wrong answer `complete` exists
/// to prevent, arriving by a different route.
#[test]
fn capping_the_group_list_marks_the_answer_incomplete() {
    let many: Vec<String> = (0..12).map(|i| format!("group-{i}")).collect();
    let (kept, complete) = cap_log_groups(many, false);
    assert_eq!(kept.len(), 8, "the fan-out is bounded");
    assert!(
        !complete,
        "four groups were dropped and they might hold the newest lines"
    );

    // Under the cap: nothing dropped, nothing to report.
    let few: Vec<String> = (0..3).map(|i| format!("group-{i}")).collect();
    let (kept, complete) = cap_log_groups(few, false);
    assert_eq!(kept.len(), 3);
    assert!(complete, "nothing was dropped");

    // An explicitly-named group is never truncated — the caller
    // chose it, so there is nothing to drop and nothing to warn
    // about.
    let (kept, complete) = cap_log_groups(vec!["chosen".into()], true);
    assert_eq!(kept, vec!["chosen".to_string()]);
    assert!(complete);
}

/// `mcp.peek_bodies = false` withholds bodies and says so.
///
/// Requested by a field report: that fleet's worker payloads are
/// job dispatches for a live staffing platform, so a body can
/// carry seller and buyer identifiers, and the tool description
/// was the only thing between a peek and a Jira ticket.
#[tokio::test]
async fn peek_bodies_off_withholds_the_body_and_says_so() {
    let mut cfg = crate::config::Config::default();
    assert!(
        cfg.mcp_peek_bodies,
        "the default must be current behaviour — turning bodies off \
             silently would cost the operator the answer on app-posted \
             messages, which carry their identity nowhere else"
    );
    cfg.mcp_peek_bodies = false;
    let s = Server::with_config(true, false, crate::cli::mcp::WriteScope::None, cfg);

    let v: Value = serde_json::from_str(
        &s.tool_worker_queues(&json!({"env": "poly-batch", "peek": true}))
            .await
            .expect("worker_queues"),
    )
    .expect("json");

    let msgs = v["messages"].as_array().expect("messages");
    assert!(!msgs.is_empty(), "need messages to withhold: {v}");
    for m in msgs {
        // Replaced, not dropped. An absent key reads as "no body",
        // which is a different claim than "not shown to you".
        let body = m["body"]
            .as_str()
            .expect("body must still be present, as a marker");
        assert!(
            body.contains("mcp.peek_bodies"),
            "the marker must name the control that produced it: {body}"
        );
        // The task fields survive — separate fields on the same
        // message, which is what makes the switch usable at all.
        if m["task"].is_object() {
            assert!(
                m["task"]["name"].is_string(),
                "suppressing the body must not suppress the task: {m}"
            );
        }
    }

    // And the agent is told, in the only channel that reaches it.
    let listed = tool_table(&crate::cli::mcp::WriteScope::None, false);
    let desc = |name: &str| -> String {
        listed
            .as_array()
            .expect("arr")
            .iter()
            .find(|t| t["name"] == name)
            .and_then(|t| t["description"].as_str())
            .unwrap_or_default()
            .to_string()
    };
    for tool in ["worker_queues", "why"] {
        assert!(
            desc(tool).contains("BODIES ARE WITHHELD"),
            "`{tool}` can return a body and must declare the policy"
        );
    }
    // Default mode says nothing — the note is a deviation notice,
    // not boilerplate every server carries.
    let normal = tool_table(&crate::cli::mcp::WriteScope::None, true);
    assert!(
        !normal.to_string().contains("BODIES ARE WITHHELD"),
        "the note must not appear when bodies are on"
    );
}

/// `doctor` distinguishes the three things that look identical to
/// an agent: ebman can't, your client can't, the operator said no.
#[tokio::test]
async fn doctor_separates_cannot_from_was_not_allowed() {
    let mut cfg = crate::config::Config {
        safety_read_only: true,
        mcp_peek_bodies: false,
        ..crate::config::Config::default()
    };
    cfg.safety_envs.insert("poly-prod-api".into(), true);
    // An account pin as well as an env pin, so the count is a SUM
    // of two non-zero terms. With only one, `+` and `-` produce
    // the same answer and the arithmetic is untested.
    cfg.safety_accounts.insert("prod-admin".into(), true);
    let s = Server::with_config(true, false, crate::cli::mcp::WriteScope::All, cfg);

    let v: Value = serde_json::from_str(&s.tool_doctor()).expect("json");

    assert_eq!(v["ebman"], env!("CARGO_PKG_VERSION"), "names the build");
    assert_eq!(
        v["standing_restrictions"]["all_writes_refused"],
        json!(true),
        "an agent must be able to learn that every write will fail BEFORE \
             trying one and reporting it as broken: {v}"
    );
    assert_eq!(
        v["standing_restrictions"]["pinned_targets"],
        json!(2),
        "env pins and account pins both count, and the total is their SUM — \
             with only one pin set, `+` and `-` give the same answer and the \
             arithmetic is untested: {v}"
    );

    let notes = v["notes"]
        .as_array()
        .expect("notes")
        .iter()
        .filter_map(|n| n.as_str())
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(notes.contains("safety.read_only"), "{notes}");
    assert!(notes.contains("peek_bodies"), "{notes}");
    assert!(notes.contains("DEMO"), "{notes}");
    // No elicitation declared by this client, so say so — the
    // absence of an ask is otherwise indistinguishable from ebman
    // choosing not to ask.
    assert_eq!(v["client_declared"]["elicitation"], json!(false), "{v}");
    assert!(notes.contains("elicitation"), "{notes}");

    // Redaction on by default, and silent about it — a note for
    // every normal condition is noise, and noise is how the
    // abnormal ones stop being read.
    assert_eq!(v["redacting"], json!(true), "{v}");
    assert!(!notes.contains("Redaction is OFF"), "{notes}");

    // With it off, say so loudly. An agent receiving real
    // environment variables needs to know they are real: the
    // difference between `(redacted)` as a policy and a value that
    // happens to look like a secret is not visible from the value.
    let open_secrets = Server::with_config(
        true,
        true,
        crate::cli::mcp::WriteScope::None,
        crate::config::Config::default(),
    );
    let o: Value = serde_json::from_str(&open_secrets.tool_doctor()).expect("json");
    assert_eq!(o["redacting"], json!(false), "{o}");
    let on = o["notes"]
        .as_array()
        .expect("notes")
        .iter()
        .filter_map(|n| n.as_str())
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(
        on.contains("--no-redact"),
        "name the flag that did it: {on}"
    );
    assert!(
        on.contains("do not quote it"),
        "and say what to do about it, since the agent is the leak path: {on}"
    );

    // The control: a clean server volunteers no restriction notes,
    // so the notes mean something when they appear.
    let clean = Server::with_config(
        false,
        false,
        crate::cli::mcp::WriteScope::None,
        crate::config::Config::default(),
    );
    let c: Value = serde_json::from_str(&clean.tool_doctor()).expect("json");
    assert_eq!(
        c["standing_restrictions"]["all_writes_refused"],
        json!(false)
    );
    assert_eq!(c["standing_restrictions"]["pinned_targets"], json!(0));
    let cn = c["notes"]
        .as_array()
        .expect("notes")
        .iter()
        .filter_map(|n| n.as_str())
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(
        !cn.contains("DEMO"),
        "a live server must not claim to be demo: {cn}"
    );
    assert!(!cn.contains("safety.read_only"), "{cn}");
}

/// `doctor` answers when everything it describes is broken.
///
/// A diagnostic that needs AWS, or the config it is reporting on,
/// fails for the reasons it exists to explain. This one reads
/// fields already on the server and nothing else.
#[tokio::test]
async fn doctor_answers_with_an_unreadable_config_and_no_aws() {
    let cfg = crate::config::Config {
        safety_parse_errors: vec!["safety.envs.prod = true is missing .read_only".into()],
        ..crate::config::Config::default()
    };
    // Not demo: a real backend whose AWS calls would fail here.
    let s = Server::with_config(false, false, crate::cli::mcp::WriteScope::All, cfg);

    let v: Value = serde_json::from_str(&s.tool_doctor()).expect("json");
    // A server refusing EVERY write must say so in the field named
    // for that fact. `write_gate::decide` checks parse errors
    // first and unconditionally, so reporting only
    // `safety_read_only` here answered "writes are available" about
    // a server that refuses all of them.
    assert_eq!(
        v["standing_restrictions"]["all_writes_refused"],
        json!(true),
        "an unreadable safety config refuses every write and the summary field \
             must reflect it, not only the detail field: {v}"
    );
    let notes = v["notes"]
        .as_array()
        .expect("notes")
        .iter()
        .filter_map(|n| n.as_str())
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(
        notes.contains("fails CLOSED"),
        "and must say retrying will not help: {notes}"
    );
    assert_eq!(
        v["standing_restrictions"]["config_unreadable"],
        json!(true),
        "an unreadable safety config refuses every write, and the agent \
             should learn that here rather than from a refusal: {v}"
    );
}

/// An empty queue answer says WHY it is empty.
///
/// Field-reported against a live web-tier env: all-nulls with
/// `peeked: false`. The flag did its job — it correctly said "I did
/// not look" rather than implying an empty queue — but nothing
/// said why there was nothing to look at. All-nulls is consistent
/// with a web tier that has no queues, a failure reading queue
/// configuration, and EB not reporting queues for an env that has
/// them. The tool description names the first; a description is
/// read once and elsewhere, which is the argument already accepted
/// for `rules_not_checked`.
#[test]
fn an_empty_queue_answer_says_why_it_is_empty() {
    let none = aws::WorkerQueues::default();

    let web = empty_queue_reason("Web", &none).expect("a web env has a reason");
    assert!(web.contains("web tier"), "{web}");
    assert!(
        web.contains("nothing here to read"),
        "and must close the question rather than leaving it open: {web}"
    );

    // A worker env with no queues is NOT ordinary and must not read
    // like the web case.
    let worker = empty_queue_reason("Worker", &none).expect("a worker env has a reason");
    assert!(worker.contains("unexpected"), "{worker}");
    assert_ne!(web, worker, "the two cases mean different things");

    // THREE tiers, not two. `tier` is "Web" / "Worker" / "?" — EB
    // can omit the tier block, and an unrecognised name passes
    // through verbatim. A two-way branch claimed "web tier, nothing
    // here to read" about an env whose tier ebman does not know,
    // closing the triage question with a claim it cannot support.
    for unknown in ["?", "SomethingNew", ""] {
        let r = empty_queue_reason(unknown, &none)
            .unwrap_or_else(|| panic!("{unknown:?} must still get a reason"));
        assert!(
            r.contains("could not be determined"),
            "{unknown:?} must not be asserted as a web tier: {r}"
        );
        assert!(
            r.contains("unconfirmed"),
            "and must leave the question open rather than closing it: {r}"
        );
        assert_ne!(r, web, "{unknown:?} is not known to be a web env");
    }

    // With queues present there is nothing to explain, and a
    // reason beside real data is noise.
    let some = aws::WorkerQueues {
        main_url: Some("https://q/main".into()),
        ..Default::default()
    };
    assert_eq!(empty_queue_reason("Worker", &some), None);
    assert_eq!(empty_queue_reason("Web", &some), None);

    // And it reaches the rendered payload.
    let v: Value = serde_json::from_str(&render_worker_queues_json(
        &none,
        &[],
        false,
        true,
        empty_queue_reason("Web", &none),
    ))
    .expect("json");
    assert!(
        v["reason"].as_str().is_some_and(|r| r.contains("web tier")),
        "the reason must be in the RESULT, not only in the tool description: {v}"
    );
    assert_eq!(v["peeked"], json!(false), "{v}");

    // No reason key at all when there is data — absence is the
    // signal that nothing needed explaining.
    let ok: Value = serde_json::from_str(&render_worker_queues_json(&some, &[], false, true, None))
        .expect("json");
    assert!(ok["reason"].is_null(), "{ok}");
}

/// The peek gate, in both directions.
#[test]
fn a_dlq_is_peekable_only_when_it_answered() {
    use crate::aws::{QueueStats, WorkerQueues};
    let with = |stats: Option<QueueStats>, url: Option<&str>| WorkerQueues {
        main_url: None,
        dlq_url: url.map(str::to_string),
        main_stats: None,
        dlq_stats: stats,
        dlq_origin: None,
    };
    let real = with(Some(QueueStats::default()), Some("https://sqs/q-dlq"));
    assert_eq!(dlq_peek_target(&real, true), Some("https://sqs/q-dlq"));
    assert_eq!(
        dlq_peek_target(&real, false),
        None,
        "not asked for is not peeked — the default path must touch nothing, \
             because a peek increments every returned message's receive count"
    );

    // The case that cost three fixes: a DERIVED url naming a queue
    // that does not exist. `dlq_url` is Some and `dlq_stats` is
    // None, and this is the ORDINARY shape for an env with no
    // dead-letter queue.
    let guessed = with(None, Some("https://sqs/q-dlq"));
    assert_eq!(
        dlq_peek_target(&guessed, true),
        None,
        "a url ebman guessed, for a queue that never answered, must not be \
             peeked — doing so raises NonExistentQueue and failed the whole call, \
             discarding the depth answer already in hand"
    );

    // And stats without a url cannot be peeked either.
    assert_eq!(
        dlq_peek_target(&with(Some(QueueStats::default()), None), true),
        None
    );
    assert_eq!(dlq_peek_target(&with(None, None), true), None);
}

/// The policy is expressed ONCE.
///
/// This guard is about duplication, not correctness, because
/// duplication is how this specific bug kept coming back: the gate
/// was written into the live path, found missing from `why`, then
/// found missing from the demo path three commits after the live
/// fix — each copy carrying a comment claiming to be "the same
/// gate as" another one. Every copy was individually defensible
/// and the set of them was the defect.
#[test]
fn nothing_re_expresses_the_peek_gate() {
    // Decisions only. `stats(&queues.dlq_stats)` in the renderer
    // reads the field without judging it, which is fine; what must
    // not spread is the RULE that a queue with no stats is not a
    // queue. Matching on decision syntax rather than counting
    // mentions means the guard survives a refactor of the helper
    // itself — an earlier version broke the moment clippy asked
    // for `as_ref()?` instead of `is_none()`.
    const DECISIONS: [&str; 4] = [
        "dlq_stats.is_",
        "dlq_stats.as_ref()?",
        "dlq_stats.is_some()",
        "|_| queues.dlq_stats",
    ];

    // Comments stripped through the shared scanner: this guard's
    // own subject is described in prose on the helper it guards,
    // and a raw substring search reads that description as a
    // violation. `strip_line_comment` also handles a `//` inside a
    // string literal, which eight hand-rolled strippers here did
    // not.
    // Located from the crate root: relative `include_str!` re-points
    // silently when the test moves next to a file of the same name.
    let code = |path: &str| -> String {
        crate::app::tests::scan::production_source(path)
            .lines()
            .map(crate::app::tests::scan::strip_line_comment)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let tools = code("cli/mcp/tools.rs");
    // Everything outside the one function allowed to decide.
    let start = tools
        .find("pub(super) fn answered_dlq_url")
        .expect("the helper must exist");
    let end = tools[start..].find("\n}\n").expect("its body ends") + start;
    let mut elsewhere = tools.clone();
    elsewhere.replace_range(start..end, "");

    for probe in DECISIONS {
        assert!(
            !elsewhere.contains(probe),
            "tools.rs: `{probe}` outside `answered_dlq_url` is a second copy of \
                 the rule. That is how this defect returned three times — the live \
                 path had it, `why` did not, the demo path did not, and each copy \
                 carried a comment claiming to be the same gate as another one."
        );
        // writes.rs must not decide at all: it calls the helper.
        let writes = code("cli/mcp/writes.rs");
        assert!(
            !writes.contains(probe),
            "writes.rs: `{probe}` is the fourth copy of this rule, found while \
                 consolidating the first three — it asked the same question to \
                 decide whether a queue could be PLANNED against. Call \
                 `answered_dlq_url`."
        );
    }
}
