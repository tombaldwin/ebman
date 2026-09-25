//! Unit tests for the MCP server's protocol surface.
//!
//! Moved verbatim out of `mcp/mod.rs`, which had grown to 5,841 lines
//! of which 4,128 were this module — so the production half could not
//! be read without scrolling past it, and neither half could be
//! reviewed as a unit.
//!
//! Deliberately at `mcp::tests` rather than `mcp::tests::something`:
//! the old inline module sat at exactly that path, so every
//! `super::parse_write_scope` / `super::staleness` / `super::tools`
//! reference inside still resolves to `crate::cli::mcp` with nothing
//! rewritten. Code motion with a path rewrite is two changes, and only
//! one of them is checkable by the test names still being there.
//!
//! **Do not add `mod writes;` / `mod tools;` / `mod tools_renderer;`
//! here.** Those three files live at `tests/`, which is exactly where
//! rustc would auto-resolve such a declaration — but they are already
//! compiled, as CHILDREN of `writes` / `tools`, via `#[path]` in those
//! files. Declaring them here compiles each a second time under a
//! different parent, where `use super::*` resolves to this module and
//! the private-item access fails, producing an error a long way from
//! its cause.

/// The `ask` question, answered with data rather than a guess.
///
/// `docs/design/protection-levels.md` stage 3 turns on whether an
/// MCP client can be asked something mid-request. Elicitation is
/// the only mechanism, and it is a CLIENT capability — so the
/// server can know, per connection, whether `ask` is expressible or
/// must degrade to a refusal.
///
/// Pinned in both directions because a detector that always says
/// "no" would quietly make every client look unable, and the levels
/// design would then be built around a limitation that is not real.
#[tokio::test]
async fn the_elicitation_capability_is_detected_per_client() {
    use std::sync::atomic::Ordering;

    async fn declares(caps: serde_json::Value) -> bool {
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
        s.client_supports_elicitation.load(Ordering::Relaxed)
    }

    assert!(
        declares(json!({"elicitation": {}})).await,
        "a client declaring elicitation must be detected, or `ask` \
             degrades to a refusal for everyone"
    );
    for explicit_no in [json!({"elicitation": false}), json!({"elicitation": null})] {
        assert!(
            !declares(explicit_no.clone()).await,
            "{explicit_no} explicitly declines elicitation; treating \
                 mere presence as support would let `ask` believe a human \
                 is watching when none is"
        );
    }
    assert!(
        !declares(json!({})).await,
        "a client declaring nothing must NOT be treated as able to ask"
    );
    assert!(
        !declares(json!({"sampling": {}, "roots": {}})).await,
        "other capabilities are not elicitation"
    );
}
use super::*;

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn demo_server() -> Server {
    Server::with_scope(true, false, WriteScope::None)
}

fn demo_writes_server() -> Server {
    Server::with_scope(true, false, WriteScope::All)
}

async fn rpc(server: &Server, frame: Value) -> Option<Value> {
    server.handle_request(&frame).await
}

/// tools/call a write tool and return the parsed result text.
async fn call(server: &Server, name: &str, args: Value) -> (bool, Value) {
    let frame = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}});
    let resp = server.handle_request(&frame).await.expect("response");
    let result = &resp["result"];
    let is_error = result["isError"].as_bool().unwrap_or(false);
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    let parsed = serde_json::from_str(text).unwrap_or(Value::String(text.to_string()));
    (is_error, parsed)
}

#[tokio::test]
async fn write_tools_appear_only_under_allow_writes() {
    let list = |s: &Server| {
        let arr = tool_table(&s.write_scope, true);
        arr.as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    let reads = list(&demo_server());
    assert!(!reads.contains(&"deploy".to_string()));
    assert!(!reads.contains(&"confirm_action".to_string()));
    let writes = list(&demo_writes_server());
    for t in [
        "deploy",
        "restart",
        "rebuild",
        "terminate",
        "set_option",
        "confirm_action",
    ] {
        assert!(writes.contains(&t.to_string()), "missing {t}");
    }
}

#[tokio::test]
async fn two_phase_happy_path_demo() {
    let s = demo_writes_server();
    let envs = demo_fixture::envs();
    let env = &envs[0].name;
    let (err, plan) = call(&s, "restart", json!({"env": env})).await;
    assert!(!err);
    assert_eq!(plan["pending"], true);
    assert_eq!(plan["plan"]["action"], "RestartAppServer");
    let token = plan["confirm_token"].as_str().unwrap().to_string();
    let (err2, out) = call(&s, "confirm_action", json!({"confirm_token": token})).await;
    assert!(!err2);
    assert_eq!(out["dispatched"], true);
    assert_eq!(out["demo"], true);
}

#[tokio::test]
async fn confirm_token_single_use_and_unknown() {
    let s = demo_writes_server();
    let env = &demo_fixture::envs()[0].name;
    let (_, plan) = call(&s, "restart", json!({"env": env})).await;
    let token = plan["confirm_token"].as_str().unwrap().to_string();
    assert!(
        !call(
            &s,
            "confirm_action",
            json!({"confirm_token": token.clone()})
        )
        .await
        .0
    );
    // reused
    assert!(
        call(&s, "confirm_action", json!({"confirm_token": token}))
            .await
            .0
    );
    // unknown
    assert!(
        call(&s, "confirm_action", json!({"confirm_token": "deadbeef"}))
            .await
            .0
    );
    // no pending
    assert!(
        call(&s, "confirm_action", json!({"confirm_token": "x"}))
            .await
            .0
    );
}

#[tokio::test]
async fn terminate_requires_matching_confirm_name_with_one_retry() {
    let s = demo_writes_server();
    let env = demo_fixture::envs()[0].name.clone();
    let (_, plan) = call(&s, "terminate", json!({"env": env})).await;
    let token = plan["confirm_token"].as_str().unwrap().to_string();
    // wrong once — token survives
    assert!(
        call(
            &s,
            "confirm_action",
            json!({"confirm_token": token.clone(), "confirm_name": "wrong"})
        )
        .await
        .0
    );
    // correct — dispatches
    let (err, out) = call(
        &s,
        "confirm_action",
        json!({"confirm_token": token, "confirm_name": env}),
    )
    .await;
    assert!(!err);
    assert_eq!(out["dispatched"], true);
}

#[tokio::test]
async fn terminate_second_wrong_name_drops_the_plan() {
    let s = demo_writes_server();
    let env = demo_fixture::envs()[0].name.clone();
    let (_, plan) = call(&s, "terminate", json!({"env": env})).await;
    let token = plan["confirm_token"].as_str().unwrap().to_string();
    assert!(
        call(
            &s,
            "confirm_action",
            json!({"confirm_token": token.clone(), "confirm_name": "wrong"})
        )
        .await
        .0
    );
    assert!(
        call(
            &s,
            "confirm_action",
            json!({"confirm_token": token.clone(), "confirm_name": "wrong"})
        )
        .await
        .0
    );
    // even the RIGHT name now fails — plan is gone
    let env2 = demo_fixture::envs()[0].name.clone();
    assert!(
        call(
            &s,
            "confirm_action",
            json!({"confirm_token": token, "confirm_name": env2})
        )
        .await
        .0
    );
}

#[tokio::test]
async fn deploy_rejects_unknown_version_and_plan_carries_versions() {
    let s = demo_writes_server();
    let envs = demo_fixture::envs();
    let env = &envs[0];
    let known = &demo_fixture::deploys_for_app(&env.application)[0].label;
    let (err, plan) = call(&s, "deploy", json!({"env": env.name, "version": known})).await;
    assert!(!err);
    assert_eq!(plan["plan"]["target_version"], known.as_str());
    assert!(plan["plan"]["current_version"].is_string());
    let (err2, _) = call(
        &s,
        "deploy",
        json!({"env": env.name, "version": "no-such-999"}),
    )
    .await;
    assert!(err2, "unknown version must refuse");
}

#[tokio::test]
async fn set_option_caps_and_gates_namespaces_and_redacts_old() {
    let s = demo_writes_server();
    let env = &demo_fixture::envs()[0].name;
    // cap
    let big: Vec<Value> = (0..11)
        .map(|i| json!({"namespace": "aws:autoscaling:asg", "name": format!("n{i}"), "value": "1"}))
        .collect();
    assert!(
        call(&s, "set_option", json!({"env": env, "settings": big}))
            .await
            .0
    );
    // unknown namespace
    assert!(
        call(
            &s,
            "set_option",
            json!({"env": env, "settings": [{"namespace":"made:up","name":"X","value":"1"}]})
        )
        .await
        .0
    );
    // known namespace: plan present; if an env-var setting exists its OLD value is redacted
    let (err, plan) = call(
            &s,
            "set_option",
            json!({"env": env, "settings": [{"namespace":"aws:autoscaling:asg","name":"MinSize","value":"9"}]}),
        )
        .await;
    assert!(!err);
    assert_eq!(plan["plan"]["changes"][0]["new"], "9");
}

#[tokio::test]
async fn dispatching_flag_clears_after_dispatch() {
    // C1 (0.28 pre-tag): the RAII guard must reset `dispatching`
    // after every dispatch (incl. cancellation/panic), or the
    // write surface wedges. Here: a completed demo dispatch leaves
    // the flag false and a second write goes through.
    let s = demo_writes_server();
    let env = &demo_fixture::envs()[0].name;
    let (_, p1) = call(&s, "restart", json!({"env": env})).await;
    let t1 = p1["confirm_token"].as_str().unwrap().to_string();
    assert!(
        !call(&s, "confirm_action", json!({"confirm_token": t1}))
            .await
            .0
    );
    assert!(
        !s.dispatching.load(std::sync::atomic::Ordering::SeqCst),
        "guard must clear dispatching after dispatch"
    );
    // second write not wedged
    let (_, p2) = call(&s, "restart", json!({"env": env})).await;
    let t2 = p2["confirm_token"].as_str().unwrap().to_string();
    assert!(
        !call(&s, "confirm_action", json!({"confirm_token": t2}))
            .await
            .0
    );
}

#[tokio::test]
async fn write_serialization_blocks_second_dispatch_slot() {
    // A pending plan exists; injecting a dispatching=true state
    // makes confirm refuse. (Full concurrency is exercised live;
    // this pins the guard.)
    let s = demo_writes_server();
    let env = &demo_fixture::envs()[0].name;
    let (_, plan) = call(&s, "restart", json!({"env": env})).await;
    let token = plan["confirm_token"].as_str().unwrap().to_string();
    s.dispatching
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(
        call(&s, "confirm_action", json!({"confirm_token": token}))
            .await
            .0,
        "confirm must refuse while a dispatch is in flight"
    );
}

#[tokio::test]
async fn write_pin_refusal() {
    let mut cfg = crate::config::Config::default();
    cfg.safety_envs
        .insert(demo_fixture::envs()[0].name.clone(), true);
    let s = Server::with_config(true, false, WriteScope::All, cfg);
    let env = demo_fixture::envs()[0].name.clone();
    let (err, plan) = call(&s, "restart", json!({"env": env})).await;
    assert!(err, "pinned env must refuse");
    assert!(
        plan.as_str().unwrap_or("").contains("pinned"),
        "refusal names the pin: {plan:?}"
    );
}

#[test]
fn mcp_args_require_serve_and_reject_unknown_flags() {
    assert!(parse_mcp_args(&argv(&["mcp"])).is_err());
    assert!(parse_mcp_args(&argv(&["mcp", "listen"])).is_err());
    assert!(parse_mcp_args(&argv(&["mcp", "serve", "--port"])).is_err());
    let p = parse_mcp_args(&argv(&["mcp", "serve", "--demo", "--no-redact"])).unwrap();
    assert!(p.demo && p.no_redact);
    let p = parse_mcp_args(&argv(&["mcp", "serve"])).unwrap();
    assert!(!p.demo && !p.no_redact);
}

#[tokio::test]
async fn golden_initialize_frame() {
    // Pins protocolVersion + capabilities + serverInfo shape. A
    // failure here means the protocol surface changed — bump
    // consciously, then update docs/headless.md.
    let resp = rpc(
        &demo_server(),
        json!({"jsonrpc":"2.0","id":1,"method":"initialize",
                   "params":{"protocolVersion":"2025-06-18","capabilities":{}}}),
    )
    .await
    .expect("initialize answers");
    assert_eq!(resp["id"], 1);
    assert_eq!(resp["result"]["protocolVersion"], PROTOCOL_VERSION);
    assert_eq!(resp["result"]["serverInfo"]["name"], "ebman");
    assert!(resp["result"]["capabilities"]["tools"].is_object());
    // An older client revision gets ours offered back.
    let resp = rpc(
        &demo_server(),
        json!({"jsonrpc":"2.0","id":2,"method":"initialize",
                   "params":{"protocolVersion":"2024-11-05"}}),
    )
    .await
    .unwrap();
    assert_eq!(resp["result"]["protocolVersion"], PROTOCOL_VERSION);
}

#[tokio::test]
async fn golden_tools_list_frame() {
    let resp = rpc(
        &demo_server(),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}),
    )
    .await
    .expect("tools/list answers");
    let tools = resp["result"]["tools"].as_array().expect("array");
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        vec![
            "list_environments",
            "worker_queues",
            "recent_logs",
            "why",
            "lint",
            "get_option_settings",
            "drift",
            "doctor",
            "audit_log",
            "recent_events",
            "list_versions",
            "fleet_cost",
        ],
        "tool registry changed — update docs/headless.md's table"
    );
    // Coverage caveats are part of the contract, not prose fluff.
    //
    // Looked up by NAME, not position: this indexed `tools[1]` and
    // broke when a tool was inserted ahead of `lint` — a failure
    // about tool ORDER dressed up as one about lint's caveats,
    // which is the wrong thing to be told.
    let lint_desc = tools
        .iter()
        .find(|t| t["name"] == "lint")
        .and_then(|t| t["description"].as_str())
        .expect("lint is advertised");
    assert!(lint_desc.contains("EBL011") && lint_desc.contains("EBL016"));
    // And the caveat must point somewhere reachable: EBL011 not
    // firing here is only actionable if it names what does.
    assert!(
        lint_desc.contains("worker_queues"),
        "the EBL011 caveat must name the tool that DOES see queues: {lint_desc}"
    );
    for t in tools {
        assert!(t["inputSchema"]["type"] == "object", "schema shape");
        assert!(
            !t["description"].as_str().unwrap().is_empty(),
            "empty description"
        );
    }
}

#[tokio::test]
async fn notifications_and_unknown_methods_route_correctly() {
    let s = demo_server();
    assert!(rpc(
        &s,
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .await
    .is_none());
    let resp = rpc(&s, json!({"jsonrpc":"2.0","id":4,"method":"ping"}))
        .await
        .unwrap();
    assert!(resp["result"].is_object());
    let resp = rpc(
        &s,
        json!({"jsonrpc":"2.0","id":5,"method":"resources/list"}),
    )
    .await
    .unwrap();
    assert_eq!(resp["error"]["code"], -32601);
    // Unknown *notification* (no id) stays silent.
    assert!(
        rpc(&s, json!({"jsonrpc":"2.0","method":"resources/changed"}))
            .await
            .is_none()
    );
    // Unknown tool → -32602 at the JSON-RPC layer.
    let resp = rpc(
        &s,
        json!({"jsonrpc":"2.0","id":6,"method":"tools/call",
                   "params":{"name":"terminate_env","arguments":{}}}),
    )
    .await
    .unwrap();
    assert_eq!(resp["error"]["code"], -32602);
}

#[tokio::test]
async fn demo_e2e_list_environments_and_lint() {
    // The zero-AWS e2e: real frames through the real tool layer
    // over the synthetic fleet.
    let s = demo_server();
    let resp = rpc(
        &s,
        json!({"jsonrpc":"2.0","id":7,"method":"tools/call",
                   "params":{"name":"list_environments","arguments":{}}}),
    )
    .await
    .unwrap();
    assert_eq!(resp["result"]["isError"], false);
    let body = resp["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(body).expect("tool body is valid JSON");
    assert!(
        !parsed.as_array().unwrap().is_empty(),
        "demo fleet non-empty"
    );
    assert!(parsed[0]["name"].is_string() && parsed[0]["health"].is_string());

    // Demo lint finds the planted EBL014 (NetworkOut trigger on a
    // scaling ASG) — a demo lint that finds nothing demonstrates
    // nothing.
    let resp = rpc(
        &s,
        json!({"jsonrpc":"2.0","id":8,"method":"tools/call",
                   "params":{"name":"lint","arguments":{"rules":"EBL014"}}}),
    )
    .await
    .unwrap();
    assert_eq!(resp["result"]["isError"], false);
    let body = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(body.contains("EBL014"), "planted finding surfaced: {body}");
}

#[tokio::test]
async fn demo_e2e_option_settings_redacts_env_vars_by_default() {
    let s = demo_server();
    let env_name = demo_fixture::envs()[0].name.clone();
    let resp = rpc(
        &s,
        json!({"jsonrpc":"2.0","id":9,"method":"tools/call",
                   "params":{"name":"get_option_settings","arguments":{"env": env_name}}}),
    )
    .await
    .unwrap();
    let body = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        !body.contains("hunter2"),
        "secret env-var value must not leak: {body}"
    );
    assert!(body.contains("DATABASE_URL"), "keys stay visible");
    assert!(body.contains("(redacted)"));
    assert!(body.contains("\"redacted\":true"));
    // --no-redact opt-out passes values through.
    let open = Server::with_scope(true, true, WriteScope::None);
    let resp = rpc(
        &open,
        json!({"jsonrpc":"2.0","id":10,"method":"tools/call",
                   "params":{"name":"get_option_settings",
                             "arguments":{"env": demo_fixture::envs()[0].name.clone()}}}),
    )
    .await
    .unwrap();
    let body = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(body.contains("hunter2") && body.contains("\"redacted\":false"));
}

#[tokio::test]
async fn tool_errors_come_back_as_is_error_results_not_rpc_errors() {
    let s = demo_server();
    let resp = rpc(
        &s,
        json!({"jsonrpc":"2.0","id":11,"method":"tools/call",
                   "params":{"name":"get_option_settings","arguments":{"env":"no-such-env"}}}),
    )
    .await
    .unwrap();
    assert_eq!(resp["result"]["isError"], true);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("not found"), "got: {text}");
    // Missing required arg — same shape.
    let resp = rpc(
        &s,
        json!({"jsonrpc":"2.0","id":12,"method":"tools/call",
                   "params":{"name":"list_versions","arguments":{}}}),
    )
    .await
    .unwrap();
    assert_eq!(resp["result"]["isError"], true);
}

#[test]
fn drift_reports_redact_env_var_secrets() {
    // C1 (0.26 pre-tag review): the drift tool leaked the exact
    // values get_option_settings redacts.
    let mut reports = vec![(
        "prod".to_string(),
        true,
        vec![
            terraform::DriftField {
                kind: "option_setting".into(),
                namespace: Some("aws:elasticbeanstalk:application:environment".into()),
                name: Some("DATABASE_URL".into()),
                tf_value: "postgres://u:hunter2@old".into(),
                live_value: "postgres://u:hunter2@new".into(),
            },
            terraform::DriftField {
                kind: "option_setting".into(),
                namespace: Some("aws:autoscaling:asg".into()),
                name: Some("MaxSize".into()),
                tf_value: "4".into(),
                live_value: "6".into(),
            },
            terraform::DriftField {
                kind: "version_label".into(),
                namespace: None,
                name: None,
                tf_value: "v1".into(),
                live_value: "v2".into(),
            },
        ],
    )];
    redact_drift_reports(&mut reports);
    let fields = &reports[0].2;
    assert_eq!(fields[0].tf_value, "(redacted)");
    assert_eq!(fields[0].live_value, "(redacted)");
    assert_eq!(fields[1].live_value, "6", "non-secret options untouched");
    assert_eq!(fields[2].tf_value, "v1", "non-option kinds untouched");
    let rendered = terraform::render_drift_json(None, None, &reports);
    assert!(!rendered.contains("hunter2"), "no secret in the payload");
}

#[test]
fn skipped_envs_spliced_only_when_present() {
    let clean = append_skipped_envs("{\"issues\":[]}".to_string(), &[]);
    assert_eq!(clean, "{\"issues\":[]}", "common case byte-identical");
    let degraded = append_skipped_envs(
        "{\"issues\":[]}".to_string(),
        &["prod: fetch failed".to_string()],
    );
    assert_eq!(
        degraded,
        "{\"issues\":[],\"skipped_envs\":[\"prod: fetch failed\"]}"
    );
    serde_json::from_str::<Value>(&degraded).expect("valid JSON");
}

#[test]
fn audit_jsonl_wraps_into_an_array() {
    assert_eq!(jsonl_to_array(""), "[]");
    assert_eq!(
        jsonl_to_array("{\"a\":1}\n{\"b\":2}\n"),
        "[{\"a\":1},{\"b\":2}]"
    );
    serde_json::from_str::<Value>(&jsonl_to_array("{\"a\":1}")).expect("valid JSON");
}

#[tokio::test]
async fn id_less_requests_are_notifications_and_get_no_response() {
    let server = Server::with_scope(true, false, WriteScope::None);
    let req: Value =
        serde_json::from_str(r#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"nope"}}"#)
            .unwrap();
    assert!(server.handle_request(&req).await.is_none());
    let ping: Value = serde_json::from_str(r#"{"jsonrpc":"2.0","method":"ping"}"#).unwrap();
    assert!(server.handle_request(&ping).await.is_none());
    // Explicit null id = notification too — answering it would
    // collide with the -32700 parse-error convention.
    let null_id: Value =
        serde_json::from_str(r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#).unwrap();
    assert!(server.handle_request(&null_id).await.is_none());
}

#[test]
fn non_object_frames_get_invalid_request() {
    // Batch arrays and scalars answer -32600 (id null); a real
    // object passes through untouched.
    let arr: Value = serde_json::from_str(r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#).unwrap();
    let resp = invalid_request_response(&arr).expect("array is invalid");
    let parsed: Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(parsed["error"]["code"], -32600);
    assert!(parsed["id"].is_null());
    let scalar: Value = serde_json::from_str("42").unwrap();
    assert!(invalid_request_response(&scalar).is_some());
    let obj: Value = serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).unwrap();
    assert!(invalid_request_response(&obj).is_none());
}

#[test]
fn empty_cost_cache_total_formats_positive_zero() {
    // f64's Sum impl folds from -0.0; without the `+ 0.0` normalise
    // an empty cost cache renders total_usd_month as "-0.00".
    let costs: std::collections::HashMap<String, f64> = Default::default();
    let total: f64 = costs.values().sum::<f64>() + 0.0;
    assert_eq!(format!("{total:.2}"), "0.00");
}

#[test]
fn redaction_covers_env_vars_and_db_password_only() {
    let r = |ns, n, v| redact_option_value(ns, n, v, true);
    assert_eq!(
        r(
            "aws:elasticbeanstalk:application:environment",
            "API_KEY",
            "sk-123"
        ),
        "(redacted)"
    );
    assert_eq!(r("aws:rds:dbinstance", "DBPassword", "pw"), "(redacted)");
    assert_eq!(r("aws:autoscaling:asg", "MaxSize", "6"), "6");
    assert_eq!(
        redact_option_value(
            "aws:elasticbeanstalk:application:environment",
            "API_KEY",
            "sk-123",
            false
        ),
        "sk-123"
    );
}

#[tokio::test]
async fn credential_errors_are_rewritten_actionably() {
    // Pure check on the shared rewrite path the tool errors use.
    let msg = tool_error(
        &Some("prod-admin".into()),
        "list_environments",
        "The security token included in the request is expired",
    );
    assert!(
        msg.contains("aws sso login --profile prod-admin"),
        "got: {msg}"
    );
    let msg = tool_error(&None, "op", "some unrelated failure");
    assert!(msg.contains("op failed"), "got: {msg}");
}

/// Every `skipped_envs` entry carries the credential fix when the
/// failure in it was an expired session: the whole-pass EBL015 skip,
/// and — through `with_credential_fix` — the per-env coverage warnings
/// and EBL015 per-branch warnings, which carried the raw SDK error.
#[test]
fn a_skipped_entry_carries_the_credential_fix() {
    let expired =
        "ListPlatformVersions failed: The security token included in the request is expired";
    let msg = super::tools::rule_skipped(&Some("prod-admin".into()), "EBL015", expired);
    assert!(
        msg.starts_with("EBL015 skipped — ListPlatformVersions failed: "),
        "got: {msg}"
    );
    assert_eq!(
        msg.matches("ListPlatformVersions").count(),
        1,
        "op doubled: {msg}"
    );
    assert!(
        msg.contains("aws sso login --profile prod-admin"),
        "got: {msg}"
    );
    let plain =
        super::tools::rule_skipped(&None, "EBL015", "ListPlatformVersions failed: Throttling");
    assert_eq!(
        plain,
        "EBL015 skipped — ListPlatformVersions failed: Throttling"
    );

    let warning = format!("EBL010 could not be evaluated for api: {expired}");
    let fixed = super::tools::with_credential_fix(&Some("prod-admin".into()), &warning);
    assert!(
        fixed.starts_with(&warning),
        "keeps the rule and env: {fixed}"
    );
    assert!(
        fixed.contains("aws sso login --profile prod-admin"),
        "{fixed}"
    );
    let other = "EBL010 could not be evaluated for api: Throttling";
    assert_eq!(super::tools::with_credential_fix(&None, other), other);
    // The wiring — every entry kind, through the live backend — is
    // `orchestration::every_skipped_entry_carries_the_credential_fix`.
}

/// `initialize` must tell a client what ebman can do that this
/// surface does not expose.
///
/// An agent sees only the tool list, so a TUI-only capability is
/// indistinguishable from one ebman lacks — and the reasonable
/// conclusion is that the gap is absolute. On a real incident that
/// sent the diagnosis out into raw `aws sqs` calls while ebman had
/// had a DLQ peek all along.
///
/// Pinned to the capabilities rather than the prose: this must fail
/// when a gap CLOSES and the note goes stale, not merely when
/// someone rewords it.
#[tokio::test]
async fn initialize_names_what_this_surface_cannot_do() {
    let s = Server::with_scope(true, false, WriteScope::None);
    let resp = s
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": PROTOCOL_VERSION, "capabilities": {},
                       "clientInfo": {"name": "probe", "version": "1"}}
        }))
        .await
        .expect("initialize responds");
    let instructions = resp["result"]["instructions"]
        .as_str()
        .expect("initialize must carry instructions")
        .to_string();

    // What is TUI-only TODAY. This list has shrunk three times as
    // tools shipped — queues, then `:why` and point-in-time logs,
    // now dead-letter management — and each time the guard failed
    // first and the block was corrected, which is the behaviour
    // wanted from it.
    // One entry today; it was three. Kept as a list because the
    // shape is "what is TUI-only", and the next capability to ship
    // should shorten this rather than restructure it.
    const TUI_ONLY: &[&str] = &["LIVE log tail"];
    for needle in TUI_ONLY {
        assert!(
            instructions.to_lowercase().contains(&needle.to_lowercase()),
            "the instructions must name `{needle}` as available elsewhere: {instructions}"
        );
    }
    assert!(
        instructions.contains("TUI"),
        "and must say where: {instructions}"
    );

    // The build version, IN the text. It is in `serverInfo` as well,
    // but a client need not surface that field and the agent reads
    // this — and the capability list above is a claim about a
    // specific build. A reader on a three-week-old binary spent two
    // days reporting capability gaps as facts, with no way to tell
    // from where it sat that it was two releases behind.
    assert!(
        instructions.contains(env!("CARGO_PKG_VERSION")),
        "the instructions must name the build they describe: {instructions}"
    );
    assert_eq!(
        resp["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION"),
        "and must agree with serverInfo"
    );

    // If a queue TOOL ever ships, this note becomes a lie. Fail
    // here so it is updated with the tool rather than left behind.
    let tools = s
        .handle_request(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
        .await
        .expect("tools/list responds");
    let names: Vec<String> = tools["result"]["tools"]
        .as_array()
        .expect("a tool array")
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_string))
        .collect();
    // The staleness half, re-aimed. It fired for real when
    // `worker_queues` shipped — the block still said queues were
    // TUI-only — which is the whole point of pinning capabilities
    // rather than prose. Now it guards the claim that MANAGEMENT
    // (resend / delete / purge) stays TUI-only.
    assert!(
        !names
            .iter()
            .any(|n| n.contains("resend") || n.contains("purge")),
        "a dead-letter management tool exists now, so the instructions \
             claiming that is TUI-only are stale: {names:?}"
    );
}

/// The queue tool answers the question EB's health text does not.
///
/// "1 message in Dead Letter Queue" names no task; the whole
/// diagnosis of a real incident hinged on which one, and that took
/// three raw `aws sqs` calls because this surface had no tool for
/// it.
#[tokio::test]
async fn worker_queues_reports_depth_and_whether_it_looked() {
    let s = demo_server();
    let envs = rpc(
        &s,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                   "params":{"name":"list_environments","arguments":{}}}),
    )
    .await
    .expect("envs");
    let listing = envs["result"]["content"][0]["text"].as_str().expect("text");
    let env_name = listing
        .split("\"name\":\"")
        .nth(1)
        .and_then(|r| r.split('"').next())
        .expect("an env in the demo fleet")
        .to_string();

    let resp = rpc(
        &s,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
                   "params":{"name":"worker_queues","arguments":{"env": env_name}}}),
    )
    .await
    .expect("worker_queues answers");
    let body = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("text payload");
    let parsed: Value = serde_json::from_str(body).expect("valid JSON");

    assert!(parsed["main_queue"].is_object(), "{body}");
    assert!(parsed["dead_letter_queue"].is_object(), "{body}");
    // "we did not look" must be distinguishable from "nothing
    // there" — the same empty array otherwise, and they mean
    // opposite things during triage.
    assert_eq!(
        parsed["peeked"], false,
        "peek defaults off, and the response must say so: {body}"
    );
    assert!(parsed["messages"].is_array(), "{body}");
}

/// `env` is required — without it the tool would have to pick one.
#[tokio::test]
async fn worker_queues_requires_an_env() {
    let resp = rpc(
        &demo_server(),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                   "params":{"name":"worker_queues","arguments":{}}}),
    )
    .await
    .expect("a response");
    let text = serde_json::to_string(&resp).expect("serialisable");
    assert!(
        text.contains("'env' is required"),
        "must refuse rather than guess an env: {text}"
    );
}

/// The counter caveat must be stated where an agent will read it.
///
/// A peek increments `receive_count`, which counts every receive —
/// an operator watching it climb across calls would conclude the
/// task is still failing. That was measured on a live message
/// (2 → 4 from three peeks), so the warning is fact, not caution.
#[tokio::test]
async fn the_queue_tool_warns_that_a_peek_inflates_the_receive_count() {
    let resp = rpc(
        &demo_server(),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
    )
    .await
    .expect("tools/list");
    let desc = resp["result"]["tools"]
        .as_array()
        .expect("array")
        .iter()
        .find(|t| t["name"] == "worker_queues")
        .and_then(|t| t["description"].as_str())
        .expect("worker_queues is advertised")
        .to_string();
    assert!(
        desc.contains("receive_count"),
        "the caveat must name the field it is about: {desc}"
    );
    assert!(
        desc.contains("not a retry count") || desc.contains("NOT a retry count"),
        "and must say what it is not: {desc}"
    );
    assert!(
        // The field as EMITTED (`dead_letter_queue.origin`), not
        // the internal Rust field name — the description is what an
        // agent reads and then looks for in the JSON, and those two
        // disagreed.
        desc.contains("dead_letter_queue.origin"),
        "a derived DLQ url that returns nothing is ordinary; a reported \
             one that does is an anomaly — the consumer cannot tell without \
             this: {desc}"
    );
}

/// `recent_logs` must report whether it reached the newest lines.
///
/// The failure this guards is a plausible wrong answer, not an
/// error: `FilterLogEvents` returns matches oldest-first, so a
/// truncated window hands back the OLDEST lines and answers "is
/// this still running?" with evidence from hours ago. `complete`
/// is the only thing that tells a reader which they are holding.
#[tokio::test]
async fn recent_logs_says_whether_it_reached_the_newest() {
    let resp = rpc(
        &demo_server(),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                   "params":{"name":"recent_logs","arguments":{"env":"any"}}}),
    )
    .await
    .expect("recent_logs answers");
    let body = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("text payload");
    let parsed: Value = serde_json::from_str(body).expect("valid JSON");
    assert!(
        parsed["complete"].is_boolean(),
        "every answer must say whether the window was fully read: {body}"
    );
    assert!(parsed["events"].is_array(), "{body}");
}

/// And the caveat must be where an agent reads it.
#[tokio::test]
async fn recent_logs_warns_about_oldest_first() {
    let resp = rpc(
        &demo_server(),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
    )
    .await
    .expect("tools/list");
    let desc = resp["result"]["tools"]
        .as_array()
        .expect("array")
        .iter()
        .find(|t| t["name"] == "recent_logs")
        .and_then(|t| t["description"].as_str())
        .expect("recent_logs is advertised")
        .to_string();
    assert!(
        desc.contains("oldest-first") || desc.contains("oldest first"),
        "the trap must be named: {desc}"
    );
    assert!(
        desc.contains("complete"),
        "and the field that tells you which you have: {desc}"
    );
    // Redaction is namespace-and-key based and cannot reach free
    // text, so this tool hands over whatever the application
    // logged. "Redaction-by-default" is a property a reader
    // attributes to the whole surface; the exception has to be
    // stated where it is acted on.
    assert!(
        desc.contains("NOT REDACTED"),
        "the one read tool that cannot be redacted must say so: {desc}"
    );
}

/// The `why` bundle puts the facts side by side in one call.
///
/// Five sections assembled by hand across five calls during a real
/// incident. The bundle is deliberately not a narrative: adjacent
/// facts let a reader be wrong in their own name, where a confident
/// generated sentence does not.
#[tokio::test]
async fn why_returns_every_section_in_one_call() {
    let s = demo_server();
    let envs = rpc(
        &s,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                   "params":{"name":"list_environments","arguments":{}}}),
    )
    .await
    .expect("envs");
    let env_name = envs["result"]["content"][0]["text"]
        .as_str()
        .and_then(|t| t.split("\"name\":\"").nth(1))
        .and_then(|r| r.split('"').next())
        .expect("an env")
        .to_string();

    let resp = rpc(
        &s,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
                   "params":{"name":"why","arguments":{"env": env_name}}}),
    )
    .await
    .expect("why answers");
    let body = resp["result"]["content"][0]["text"].as_str().expect("text");
    let parsed: Value = serde_json::from_str(body).expect("valid JSON");

    for section in ["events", "alarms", "instances", "queues", "recent_versions"] {
        assert!(
            parsed.get(section).is_some(),
            "`{section}` must be present even when null — a missing key \
                 and an empty one read differently: {body}"
        );
    }
    assert!(
        parsed["errors"].is_array(),
        "a partial bundle must be visibly partial: {body}"
    );
}

/// A failed section is null WITH a reason, never an empty array.
///
/// "We could not look" and "there is nothing there" are opposite
/// conclusions during triage, and an empty array says the second
/// while meaning the first.
#[test]
fn a_failed_why_section_is_null_with_its_reason() {
    let bundle = super::tools::render_why_json(
        "api-prod",
        "[]",
        "null",
        "[]",
        "null",
        "[]",
        &[
            ("alarms".to_string(), "AccessDenied".to_string()),
            ("queues".to_string(), "throttled".to_string()),
        ],
    );
    let parsed: Value = serde_json::from_str(&bundle).expect("valid JSON");
    assert!(parsed["alarms"].is_null(), "{bundle}");
    assert!(parsed["queues"].is_null(), "{bundle}");
    assert!(
        parsed["events"].is_array() && parsed["instances"].is_array(),
        "sections that succeeded must still be there: {bundle}"
    );
    let errs = parsed["errors"].as_array().expect("errors array");
    assert_eq!(errs.len(), 2, "{bundle}");
    assert!(
        errs.iter()
            .any(|e| e["section"] == "alarms" && e["error"] == "AccessDenied"),
        "the reason must survive, not just the fact of failure: {bundle}"
    );
}

/// A failed section must be `null` with its reason, not an empty
/// array and not a silent success.
#[test]
fn a_failed_section_is_null_and_recorded() {
    let mut errors: Vec<(String, String)> = Vec::new();
    let ok = super::tools::section_or_error("events", Ok("[1,2]".into()), &mut errors);
    assert_eq!(ok, "[1,2]", "a good section passes through untouched");
    assert!(errors.is_empty(), "and records nothing");

    let bad = super::tools::section_or_error("alarms", Err("AccessDenied".into()), &mut errors);
    assert_eq!(
        bad, "null",
        "a failed section must be null — `[]` would read as \"no alarms\", \
             which is the opposite of \"we could not check\""
    );
    assert_eq!(
        errors,
        vec![("alarms".to_string(), "AccessDenied".to_string())],
        "and the reason must be kept, not just the fact of failure"
    );
}

/// A FAILED dead-letter peek must not report as an empty queue.
///
/// Reading messages needs `sqs:ReceiveMessage`, a different
/// permission from the attributes call that produced the depth — so
/// "depth says 1, peek denied" is an ordinary IAM shape. Reported as
/// `peeked: true, messages: []` it reads as "we looked, the queue is
/// clean", which is the opposite of what happened and the exact
/// distinction `peeked` exists to preserve.
#[test]
fn a_denied_dlq_peek_is_not_an_empty_queue() {
    use crate::aws::QueueMessage;
    let msg = || QueueMessage {
        id: "m-1".into(),
        attributes: Vec::new(),
        receipt_handle: String::new(),
        body: "b".into(),
        receive_count: 1,
        sent_at: None,
        task: None,
    };

    // Success: messages, and we looked.
    let mut errors = Vec::new();
    let (msgs, peeked) = super::tools::dlq_peek_outcome(Some(Ok(vec![msg()])), &mut errors);
    assert_eq!(msgs.len(), 1);
    assert!(peeked);
    assert!(errors.is_empty());

    // Failure: no messages, we did NOT look, and the reason survives.
    let mut errors = Vec::new();
    let (msgs, peeked) = super::tools::dlq_peek_outcome(
        Some(Err("AccessDenied: sqs:ReceiveMessage".into())),
        &mut errors,
    );
    assert!(msgs.is_empty());
    assert!(
        !peeked,
        "a denied peek must not claim we looked — that turns \
             'permission missing' into 'queue is clean'"
    );
    assert_eq!(
        errors,
        vec![(
            "dlq_peek".to_string(),
            "AccessDenied: sqs:ReceiveMessage".to_string()
        )],
        "and the reason must reach the caller, not just the fact"
    );

    // No dead-letter queue at all: nothing to look at, did not look,
    // and NOT an error — a web-tier env is not a failure.
    let mut errors = Vec::new();
    let (msgs, peeked) = super::tools::dlq_peek_outcome(None, &mut errors);
    assert!(msgs.is_empty() && !peeked);
    assert!(
        errors.is_empty(),
        "an env with no dead-letter queue is ordinary, not an error"
    );
}

/// Orchestration tests: the tool BODIES, driven against a mocked
/// SDK.
///
/// Everything under these was already covered — renderers, extracted
/// decisions, the AWS-layer calls. What was not is which calls a
/// tool makes and with what, and that is the layer the `tool_why`
/// dead-letter peek bug lived in: it survived a full review because
/// nothing could reach it.
mod orchestration {
    use super::*;
    use aws_sdk_elasticbeanstalk::Client as EbClient;
    use aws_sdk_sqs::Client as SqsClient;

    fn client_with(eb: EbClient, sqs: SqsClient) -> crate::aws::AwsClient {
        let cfg = aws_config::SdkConfig::builder()
            .region(aws_config::Region::new("us-west-1"))
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build();
        crate::aws::AwsClient::for_tests(
            eb,
            sqs,
            aws_sdk_cloudwatch::Client::new(&cfg),
            aws_sdk_cloudwatchlogs::Client::new(&cfg),
            aws_sdk_s3::Client::new(&cfg),
            aws_sdk_ec2::Client::new(&cfg),
        )
    }

    fn env_listing() -> aws_smithy_mocks::Rule {
        use aws_sdk_elasticbeanstalk::operation::describe_environments::DescribeEnvironmentsOutput;
        use aws_sdk_elasticbeanstalk::types::EnvironmentDescription;
        aws_smithy_mocks::mock!(EbClient::describe_environments).then_output(|| {
            DescribeEnvironmentsOutput::builder()
                .environments(
                    EnvironmentDescription::builder()
                        .environment_name("poly-prod-wk")
                        .application_name("poly")
                        .status("Ready".into())
                        .health("Yellow".into())
                        .tier(
                            aws_sdk_elasticbeanstalk::types::EnvironmentTier::builder()
                                .name("Worker")
                                .build(),
                        )
                        .build(),
                )
                .build()
        })
    }

    /// Which lint input the mock fleet refuses.
    #[derive(Default, Clone, Copy)]
    struct LintFaults {
        stacks: bool,
        platform_list: bool,
        platform_date: bool,
        health: bool,
        /// The refusals are an expired session, not AccessDenied.
        expired: bool,
        /// Two versioned envs instead of one, so a per-env line and a
        /// per-run line differ.
        two_envs: bool,
        /// The env is on a custom platform: no version family, so EBL008
        /// cannot apply to it.
        unversioned: bool,
        /// The per-env option-settings fetch fails: lint cannot run for
        /// the env at all.
        settings: bool,
    }

    /// One Ready/Green web env, every lint input answering except the
    /// faults asked for — so anything in `skipped_envs` came from them.
    fn lint_server(f: LintFaults) -> Server {
        use aws_sdk_elasticbeanstalk::operation as op;
        use aws_sdk_elasticbeanstalk::types::{EnvironmentDescription, PlatformSummary};
        let expired = f.expired;
        let denied = move |what: &str| {
            if expired {
                aws_smithy_types::error::ErrorMetadata::builder()
                    .code("ExpiredTokenException")
                    .message("The security token included in the request is expired")
                    .build()
            } else {
                aws_smithy_types::error::ErrorMetadata::builder()
                    .code("AccessDeniedException")
                    .message(format!("User is not authorized to perform {what}"))
                    .build()
            }
        };
        let names: &'static [&'static str] = if f.two_envs {
            &["poly-web", "poly-api"]
        } else {
            &["poly-web"]
        };
        let stack = if f.unversioned {
            ""
        } else {
            // Versioned, so EBL008 applies to it.
            "64bit Amazon Linux 2023 v4.1.0 running Corretto 17"
        };
        let listing =
            aws_smithy_mocks::mock!(EbClient::describe_environments).then_output(move || {
                let mut b = op::describe_environments::DescribeEnvironmentsOutput::builder();
                for name in names {
                    b = b.environments(
                        EnvironmentDescription::builder()
                            .environment_name(*name)
                            .application_name("poly")
                            .solution_stack_name(stack)
                            .status("Ready".into())
                            .health("Green".into())
                            .build(),
                    );
                }
                b.build()
            });
        let stacks = if f.stacks {
            aws_smithy_mocks::mock!(EbClient::list_available_solution_stacks).then_error(
                move || {
                    op::list_available_solution_stacks::ListAvailableSolutionStacksError::generic(
                        denied("elasticbeanstalk:ListAvailableSolutionStacks"),
                    )
                },
            )
        } else {
            aws_smithy_mocks::mock!(EbClient::list_available_solution_stacks).then_output(|| {
                op::list_available_solution_stacks::ListAvailableSolutionStacksOutput::builder()
                    .build()
            })
        };
        let settings = if f.settings {
            aws_smithy_mocks::mock!(EbClient::describe_configuration_settings).then_error(
                move || {
                    op::describe_configuration_settings::DescribeConfigurationSettingsError::generic(
                        denied("elasticbeanstalk:DescribeConfigurationSettings"),
                    )
                },
            )
        } else {
            aws_smithy_mocks::mock!(EbClient::describe_configuration_settings).then_output(|| {
                op::describe_configuration_settings::DescribeConfigurationSettingsOutput::builder()
                    .build()
            })
        };
        let health = if f.health {
            aws_smithy_mocks::mock!(EbClient::describe_environment_health).then_error(move || {
                op::describe_environment_health::DescribeEnvironmentHealthError::generic(denied(
                    "elasticbeanstalk:DescribeEnvironmentHealth",
                ))
            })
        } else {
            aws_smithy_mocks::mock!(EbClient::describe_environment_health).then_output(|| {
                op::describe_environment_health::DescribeEnvironmentHealthOutput::builder().build()
            })
        };
        let platforms = if f.platform_list {
            aws_smithy_mocks::mock!(EbClient::list_platform_versions).then_error(move || {
                op::list_platform_versions::ListPlatformVersionsError::generic(denied(
                    "elasticbeanstalk:ListPlatformVersions",
                ))
            })
        } else {
            let with_one = f.platform_date;
            aws_smithy_mocks::mock!(EbClient::list_platform_versions).then_output(move || {
                let mut b = op::list_platform_versions::ListPlatformVersionsOutput::builder();
                if with_one {
                    b = b.platform_summary_list(
                        PlatformSummary::builder()
                            .platform_arn("arn:aws:elasticbeanstalk:us-west-1:123456789012:platform/custom-node/1.0.0")
                            .platform_branch_name("custom-node")
                            .build(),
                    );
                }
                b.build()
            })
        };
        let platform_date = aws_smithy_mocks::mock!(EbClient::describe_platform_version)
            .then_error(move || {
                op::describe_platform_version::DescribePlatformVersionError::generic(denied(
                    "elasticbeanstalk:DescribePlatformVersion",
                ))
            });
        let eb = aws_smithy_mocks::mock_client!(
            aws_sdk_elasticbeanstalk,
            aws_smithy_mocks::RuleMode::MatchAny,
            [
                &listing,
                &stacks,
                &settings,
                &health,
                &platforms,
                &platform_date
            ]
        );
        let sqs =
            aws_smithy_mocks::mock_client!(aws_sdk_sqs, aws_smithy_mocks::RuleMode::MatchAny, []);
        Server::with_injected_client(
            WriteScope::None,
            crate::config::Config::default(),
            client_with(eb, sqs),
        )
    }

    async fn lint_skipped(f: LintFaults) -> Vec<String> {
        let out = lint_server(f)
            .call_tool("lint", &json!({}))
            .await
            .expect("lint answers");
        let v: Value = serde_json::from_str(&out).expect("valid JSON");
        v["skipped_envs"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The discriminating case: a fleet that answered everything
    /// reports nothing skipped. Without it, every assertion below
    /// would pass on a tool that reported skips unconditionally.
    #[tokio::test]
    async fn lint_that_saw_everything_reports_nothing_skipped() {
        let skipped = lint_skipped(LintFaults::default()).await;
        assert!(skipped.is_empty(), "{skipped:?}");
    }

    /// A failed stack listing is lost EBL008 coverage, reported.
    ///
    /// It was an empty map under a comment reading "same tolerance as
    /// the CLI path" — false since 0.44, when the CLI started degrading
    /// on it. An agent got a clean result for a check that never ran.
    #[tokio::test]
    async fn lint_reports_a_failed_stack_listing() {
        let skipped = lint_skipped(LintFaults {
            stacks: true,
            ..LintFaults::default()
        })
        .await;
        assert!(
            skipped
                .iter()
                .any(|s| s.contains("EBL008") && s.contains("ListAvailableSolutionStacks")),
            "{skipped:?}"
        );
    }

    /// ...once for the run, not once per env: one failed call is one
    /// entry, where the per-env form repeated the same cause N times.
    #[tokio::test]
    async fn a_failed_stack_listing_is_one_entry_however_many_envs() {
        let skipped = lint_skipped(LintFaults {
            stacks: true,
            two_envs: true,
            ..LintFaults::default()
        })
        .await;
        let ebl008: Vec<_> = skipped.iter().filter(|s| s.contains("EBL008")).collect();
        assert_eq!(ebl008.len(), 1, "{skipped:?}");
        assert_eq!(
            ebl008[0]
                .matches("ListAvailableSolutionStacks failed:")
                .count(),
            1,
            "the op is not doubled: {}",
            ebl008[0]
        );
    }

    /// ...and not at all when no env in scope could have had EBL008 fire:
    /// a fleet of custom platforms lost nothing.
    #[tokio::test]
    async fn a_failed_stack_listing_costs_nothing_on_custom_platforms() {
        let skipped = lint_skipped(LintFaults {
            stacks: true,
            unversioned: true,
            ..LintFaults::default()
        })
        .await;
        assert!(!skipped.iter().any(|s| s.contains("EBL008")), "{skipped:?}");
    }

    /// An expired session reaches the agent as the fix, in every kind
    /// of entry: the whole-pass skip and a per-env coverage warning.
    /// Both carried the raw SDK error before.
    #[tokio::test]
    async fn every_skipped_entry_carries_the_credential_fix() {
        let skipped = lint_skipped(LintFaults {
            stacks: true,
            health: true,
            expired: true,
            ..LintFaults::default()
        })
        .await;
        for rule in ["EBL008", "EBL012"] {
            let entry = skipped
                .iter()
                .find(|s| s.starts_with(rule))
                .unwrap_or_else(|| panic!("no {rule} entry: {skipped:?}"));
            assert!(entry.contains("aws sso login"), "{rule}: {entry}");
            assert_eq!(
                entry.matches("aws sso login").count(),
                1,
                "one hint: {entry}"
            );
        }
    }

    /// An env whose settings could not be fetched is skipped whole, and
    /// the entry carries the fix — without the op prefixed twice, which
    /// `tool_error` did ("fetch_env_lint_inputs failed:
    /// DescribeConfigurationSettings failed: …").
    #[tokio::test]
    async fn a_skipped_env_carries_the_credential_fix_once() {
        let skipped = lint_skipped(LintFaults {
            settings: true,
            expired: true,
            ..LintFaults::default()
        })
        .await;
        let entry = skipped
            .iter()
            .find(|s| s.starts_with("poly-web: "))
            .unwrap_or_else(|| panic!("no entry for the env: {skipped:?}"));
        assert!(entry.contains("aws sso login"), "{entry}");
        assert!(!entry.contains("fetch_env_lint_inputs"), "{entry}");
        assert_eq!(entry.matches(" failed:").count(), 1, "{entry}");
    }

    /// A whole failed EBL015 pass is reported. `if let Ok` dropped it
    /// with nothing in the result at all.
    #[tokio::test]
    async fn lint_reports_a_failed_platform_pass() {
        let skipped = lint_skipped(LintFaults {
            platform_list: true,
            ..LintFaults::default()
        })
        .await;
        assert!(
            skipped
                .iter()
                .any(|s| s.contains("EBL015") && s.contains("ListPlatformVersions")),
            "{skipped:?}"
        );
    }

    /// And a partly-failed one, where the CLI now degrades too.
    #[tokio::test]
    async fn lint_reports_a_partly_failed_platform_pass() {
        let skipped = lint_skipped(LintFaults {
            platform_date: true,
            ..LintFaults::default()
        })
        .await;
        assert!(
            skipped
                .iter()
                .any(|s| s.contains("EBL015 skipped for 'custom-node'")),
            "{skipped:?}"
        );
    }

    /// The shared input fetch's new health warning reaches the tool.
    #[tokio::test]
    async fn lint_reports_a_failed_health_fetch() {
        let skipped = lint_skipped(LintFaults {
            health: true,
            ..LintFaults::default()
        })
        .await;
        assert!(
            skipped
                .iter()
                .any(|s| s.contains("EBL012") && s.contains("DescribeEnvironmentHealth")),
            "{skipped:?}"
        );
    }

    /// EB's own names for an auto-created worker queue pair. Neither
    /// ends in `-dlq`, which is what made the old derivation wrong.
    const EB_MAIN: &str =
        "https://sqs.us-west-1.amazonaws.com/123456789012/awseb-e-abc-stack-AWSEBWorkerQueue-XYZ";
    const EB_DLQ: &str =
        "https://sqs.us-west-1.amazonaws.com/123456789012/awseb-e-abc-stack-AWSEBWorkerDeadLetterQueue-XYZ";

    /// The worker env's queues as EB reports them: the DLQ always,
    /// the main queue only when `with_main`.
    fn reported_queues(with_main: bool) -> aws_smithy_mocks::Rule {
        use aws_sdk_elasticbeanstalk::operation::describe_environment_resources::DescribeEnvironmentResourcesOutput;
        use aws_sdk_elasticbeanstalk::types::{EnvironmentResourceDescription, Queue};
        aws_smithy_mocks::mock!(EbClient::describe_environment_resources).then_output(move || {
            let mut res = EnvironmentResourceDescription::builder().queues(
                Queue::builder()
                    .name("WorkerDeadLetterQueue")
                    .url(EB_DLQ)
                    .build(),
            );
            if with_main {
                res = res.queues(Queue::builder().name("WorkerQueue").url(EB_MAIN).build());
            }
            DescribeEnvironmentResourcesOutput::builder()
                .environment_resources(res.build())
                .build()
        })
    }

    /// No `aws:elasticbeanstalk:sqsd` overrides, so a queue EB did not
    /// report stays unresolved rather than being invented.
    fn no_sqsd_settings() -> aws_smithy_mocks::Rule {
        use aws_sdk_elasticbeanstalk::operation::describe_configuration_settings::DescribeConfigurationSettingsOutput;
        aws_smithy_mocks::mock!(EbClient::describe_configuration_settings)
            .then_output(|| DescribeConfigurationSettingsOutput::builder().build())
    }

    /// The confirm path reads recent events for its dispatch baseline.
    fn no_events() -> aws_smithy_mocks::Rule {
        aws_smithy_mocks::mock!(EbClient::describe_events).then_output(|| {
            aws_sdk_elasticbeanstalk::operation::describe_events::DescribeEventsOutput::builder()
                .build()
        })
    }

    fn one_visible() -> aws_smithy_mocks::Rule {
        use aws_sdk_sqs::operation::get_queue_attributes::GetQueueAttributesOutput;
        use aws_sdk_sqs::types::QueueAttributeName;
        aws_smithy_mocks::mock!(SqsClient::get_queue_attributes).then_output(|| {
            GetQueueAttributesOutput::builder()
                .attributes(QueueAttributeName::ApproximateNumberOfMessages, "1")
                .attributes(
                    QueueAttributeName::ApproximateNumberOfMessagesNotVisible,
                    "0",
                )
                .attributes(QueueAttributeName::ApproximateNumberOfMessagesDelayed, "0")
                .build()
        })
    }

    fn dead_lettered_message() -> aws_smithy_mocks::Rule {
        use aws_sdk_sqs::operation::receive_message::ReceiveMessageOutput;
        use aws_sdk_sqs::types::Message;
        aws_smithy_mocks::mock!(SqsClient::receive_message)
            .match_requests(|req| req.queue_url() == Some(EB_DLQ))
            .then_output(|| {
                ReceiveMessageOutput::builder()
                    .messages(
                        Message::builder()
                            .message_id("m-1")
                            .receipt_handle("rh-1")
                            .body("job")
                            .build(),
                    )
                    .build()
            })
    }

    /// A resend puts the message on the main queue EB REPORTED.
    ///
    /// It used to derive the main queue by stripping `-dlq` from the
    /// DLQ url and, when there was no such suffix, fall back to the DLQ
    /// url itself. EB's auto-created pair is named
    /// `…-AWSEBWorkerQueue-…` / `…-AWSEBWorkerDeadLetterQueue-…`, so the
    /// "resend" went straight back into the dead-letter queue, the
    /// original was deleted, and the result said `ok: true`. The work
    /// never reached the worker. Every fixture used `-dlq` names, which
    /// is the one shape the derivation got right.
    ///
    /// The send rule matches ONLY the reported main queue: a send
    /// anywhere else finds no rule and the message reports a failure.
    #[tokio::test]
    async fn a_resend_goes_to_the_main_queue_eb_reported() {
        use aws_sdk_sqs::operation::delete_message::DeleteMessageOutput;
        use aws_sdk_sqs::operation::send_message::SendMessageOutput;

        let send = aws_smithy_mocks::mock!(SqsClient::send_message)
            .match_requests(|req| req.queue_url() == Some(EB_MAIN))
            .then_output(|| SendMessageOutput::builder().message_id("new-1").build());
        let delete = aws_smithy_mocks::mock!(SqsClient::delete_message)
            .match_requests(|req| req.queue_url() == Some(EB_DLQ))
            .then_output(|| DeleteMessageOutput::builder().build());
        let eb = aws_smithy_mocks::mock_client!(
            aws_sdk_elasticbeanstalk,
            aws_smithy_mocks::RuleMode::MatchAny,
            [
                &env_listing(),
                &reported_queues(true),
                &no_sqsd_settings(),
                &no_events()
            ]
        );
        let sqs = aws_smithy_mocks::mock_client!(
            aws_sdk_sqs,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&one_visible(), &dead_lettered_message(), &send, &delete]
        );
        let s = Server::with_injected_client(
            WriteScope::All,
            crate::config::Config::default(),
            client_with(eb, sqs),
        );

        let plan = s
            .call_tool(
                "dlq_resend",
                &json!({"env": "poly-prod-wk", "message_id": "m-1"}),
            )
            .await
            .expect("the resend plans");
        let plan: Value = serde_json::from_str(&plan).expect("valid JSON");
        let token = plan["confirm_token"].as_str().expect("a token").to_string();

        let out = s
            .call_tool("confirm_action", &json!({"confirm_token": token}))
            .await
            .expect("the confirm dispatches");
        let v: Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(
            v["succeeded"], 1,
            "the resend must reach the reported main queue: {out}"
        );
    }

    /// With no main queue resolved, a resend is refused at PLAN time:
    /// there is nowhere legitimate to send it, and guessing one is
    /// exactly the defect above.
    #[tokio::test]
    async fn a_resend_with_no_main_queue_is_refused_before_anything_is_planned() {
        let eb = aws_smithy_mocks::mock_client!(
            aws_sdk_elasticbeanstalk,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&env_listing(), &reported_queues(false), &no_sqsd_settings()]
        );
        let sqs = aws_smithy_mocks::mock_client!(
            aws_sdk_sqs,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&one_visible(), &dead_lettered_message()]
        );
        let s = Server::with_injected_client(
            WriteScope::All,
            crate::config::Config::default(),
            client_with(eb, sqs),
        );
        let err = s
            .call_tool(
                "dlq_resend",
                &json!({"env": "poly-prod-wk", "message_id": "m-1"}),
            )
            .await
            .expect_err("no main queue means no plan");
        assert!(
            err.contains("main queue could not be resolved"),
            "the refusal must say why: {err}"
        );
    }

    /// `worker_queues` must resolve the env's queues and, with
    /// `peek`, read messages from the DEAD-LETTER url — not the
    /// main one.
    #[tokio::test]
    async fn worker_queues_peeks_the_dead_letter_queue() {
        use aws_sdk_elasticbeanstalk::operation::describe_environment_resources::DescribeEnvironmentResourcesOutput;
        use aws_sdk_elasticbeanstalk::types::{EnvironmentResourceDescription, Queue};
        use aws_sdk_sqs::operation::get_queue_attributes::GetQueueAttributesOutput;
        use aws_sdk_sqs::operation::receive_message::ReceiveMessageOutput;
        use aws_sdk_sqs::types::{Message, MessageAttributeValue, QueueAttributeName};

        let resources = aws_smithy_mocks::mock!(EbClient::describe_environment_resources)
            .then_output(|| {
                DescribeEnvironmentResourcesOutput::builder()
                    .environment_resources(
                        EnvironmentResourceDescription::builder()
                            .queues(
                                Queue::builder()
                                    .name("WorkerQueue")
                                    .url("https://sqs/main")
                                    .build(),
                            )
                            .queues(
                                Queue::builder()
                                    .name("WorkerDeadLetterQueue")
                                    .url("https://sqs/main-dlq")
                                    .build(),
                            )
                            .build(),
                    )
                    .build()
            });
        let attrs = aws_smithy_mocks::mock!(SqsClient::get_queue_attributes).then_output(|| {
            GetQueueAttributesOutput::builder()
                .attributes(QueueAttributeName::ApproximateNumberOfMessages, "1")
                .attributes(
                    QueueAttributeName::ApproximateNumberOfMessagesNotVisible,
                    "0",
                )
                .attributes(QueueAttributeName::ApproximateNumberOfMessagesDelayed, "0")
                .build()
        });
        // Matches ONLY the dead-letter url. A body that peeked the
        // main queue would get no match and no task.
        let peek = aws_smithy_mocks::mock!(SqsClient::receive_message)
            .match_requests(|req| req.queue_url() == Some("https://sqs/main-dlq"))
            .then_output(|| {
                let attr = |v: &str| {
                    MessageAttributeValue::builder()
                        .data_type("String")
                        .string_value(v)
                        .build()
                        .expect("valid")
                };
                ReceiveMessageOutput::builder()
                    .messages(
                        Message::builder()
                            .message_id("m-1")
                            .receipt_handle("rh-1")
                            .body("elasticbeanstalk scheduled job")
                            .message_attributes(
                                "beanstalk.sqsd.task_name",
                                attr("ORCHCANARY sweep"),
                            )
                            .build(),
                    )
                    .build()
            });

        let eb = aws_smithy_mocks::mock_client!(
            aws_sdk_elasticbeanstalk,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&env_listing(), &resources]
        );
        let sqs = aws_smithy_mocks::mock_client!(
            aws_sdk_sqs,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&attrs, &peek]
        );
        let s = Server::with_injected_client(
            WriteScope::None,
            crate::config::Config::default(),
            client_with(eb, sqs),
        );

        let out = s
            .call_tool(
                "worker_queues",
                &json!({"env": "poly-prod-wk", "peek": true}),
            )
            .await
            .expect("worker_queues answers");
        let v: Value = serde_json::from_str(&out).expect("valid JSON");

        assert_eq!(v["dead_letter_queue"]["stats"]["visible"], 1);
        assert_eq!(v["peeked"], true);
        assert_eq!(
            v["messages"][0]["task"]["name"], "ORCHCANARY sweep",
            "the peek must read the DEAD-LETTER queue and carry the \
                 task through: {out}"
        );
    }

    /// Without `peek` the tool must not call ReceiveMessage at all.
    ///
    /// The default path is documented as touching nothing, and a
    /// peek increments a counter an operator reads. The mock has no
    /// receive_message rule, so a body that peeked anyway fails.
    #[tokio::test]
    async fn worker_queues_does_not_peek_unless_asked() {
        use aws_sdk_elasticbeanstalk::operation::describe_environment_resources::DescribeEnvironmentResourcesOutput;
        use aws_sdk_elasticbeanstalk::types::{EnvironmentResourceDescription, Queue};
        use aws_sdk_sqs::operation::get_queue_attributes::GetQueueAttributesOutput;
        use aws_sdk_sqs::types::QueueAttributeName;

        let resources = aws_smithy_mocks::mock!(EbClient::describe_environment_resources)
            .then_output(|| {
                DescribeEnvironmentResourcesOutput::builder()
                    .environment_resources(
                        EnvironmentResourceDescription::builder()
                            .queues(
                                Queue::builder()
                                    .name("WorkerQueue")
                                    .url("https://sqs/main")
                                    .build(),
                            )
                            .build(),
                    )
                    .build()
            });
        let attrs = aws_smithy_mocks::mock!(SqsClient::get_queue_attributes).then_output(|| {
            GetQueueAttributesOutput::builder()
                .attributes(QueueAttributeName::ApproximateNumberOfMessages, "0")
                .build()
        });
        // EB named no dead-letter queue, so `describe_worker_queues`
        // falls back to `aws:elasticbeanstalk:sqsd` option settings
        // looking for an explicit override. Discovered by this test
        // failing on an UNMATCHED call — which is exactly the
        // orchestration detail these tests exist to pin, and which
        // nothing below this layer could have shown.
        let settings = aws_smithy_mocks::mock!(EbClient::describe_configuration_settings)
                .then_output(|| {
                    aws_sdk_elasticbeanstalk::operation::describe_configuration_settings::DescribeConfigurationSettingsOutput::builder().build()
                });
        let eb = aws_smithy_mocks::mock_client!(
            aws_sdk_elasticbeanstalk,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&env_listing(), &resources, &settings]
        );
        let sqs = aws_smithy_mocks::mock_client!(
            aws_sdk_sqs,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&attrs]
        );
        let s = Server::with_injected_client(
            WriteScope::None,
            crate::config::Config::default(),
            client_with(eb, sqs),
        );

        let out = s
            .call_tool("worker_queues", &json!({"env": "poly-prod-wk"}))
            .await
            .expect("depth-only must not need a receive_message rule");
        let v: Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(v["peeked"], false, "{out}");
        assert!(
            v["messages"].as_array().is_some_and(|m| m.is_empty()),
            "{out}"
        );
    }

    /// An env the fleet does not contain must be refused, not
    /// silently queried.
    #[tokio::test]
    async fn worker_queues_refuses_an_unknown_env() {
        let eb = aws_smithy_mocks::mock_client!(
            aws_sdk_elasticbeanstalk,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&env_listing()]
        );
        let cfg = aws_config::SdkConfig::builder()
            .region(aws_config::Region::new("us-west-1"))
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build();
        let s = Server::with_injected_client(
            WriteScope::None,
            crate::config::Config::default(),
            client_with(eb, SqsClient::new(&cfg)),
        );
        let err = s
            .call_tool("worker_queues", &json!({"env": "no-such-env"}))
            .await
            .expect_err("an unknown env must be an error");
        assert!(err.contains("not found"), "{err}");
    }

    /// A derived dead-letter URL that names no real queue must not
    /// fail the call.
    ///
    /// `describe_worker_queues` treats NonExistentQueue on a DERIVED
    /// url as THE "this env has no dead-letter queue" shape — it
    /// swallows the error and leaves `dlq_stats: None` with
    /// `dlq_url: Some`. Peeking that url raised it again, and the
    /// `?` failed the whole tool call, discarding the depth answer
    /// we already had. On the case the tool's own description calls
    /// ordinary.
    ///
    /// The mock has NO receive_message rule: a body that still
    /// peeks gets an unmatched call and fails.
    #[tokio::test]
    async fn a_derived_dlq_that_does_not_exist_still_answers() {
        use aws_sdk_elasticbeanstalk::operation::describe_environment_resources::DescribeEnvironmentResourcesOutput;
        use aws_sdk_elasticbeanstalk::types::{EnvironmentResourceDescription, Queue};
        use aws_sdk_sqs::operation::get_queue_attributes::GetQueueAttributesOutput;
        use aws_sdk_sqs::types::QueueAttributeName;

        // EB names only the main queue, so the DLQ url is derived.
        let resources = aws_smithy_mocks::mock!(EbClient::describe_environment_resources)
            .then_output(|| {
                DescribeEnvironmentResourcesOutput::builder()
                    .environment_resources(
                        EnvironmentResourceDescription::builder()
                            .queues(
                                Queue::builder()
                                    .name("WorkerQueue")
                                    .url("https://sqs/main")
                                    .build(),
                            )
                            .build(),
                    )
                    .build()
            });
        let settings = aws_smithy_mocks::mock!(EbClient::describe_configuration_settings)
                .then_output(|| {
                    aws_sdk_elasticbeanstalk::operation::describe_configuration_settings::DescribeConfigurationSettingsOutput::builder().build()
                });
        // The main queue answers; the derived `-dlq` does not exist.
        let main_attrs = aws_smithy_mocks::mock!(SqsClient::get_queue_attributes)
            .match_requests(|req| req.queue_url() == Some("https://sqs/main"))
            .then_output(|| {
                GetQueueAttributesOutput::builder()
                    .attributes(QueueAttributeName::ApproximateNumberOfMessages, "3")
                    .build()
            });
        let dlq_missing = aws_smithy_mocks::mock!(SqsClient::get_queue_attributes)
            .match_requests(|req| req.queue_url() == Some("https://sqs/main-dlq"))
            .then_error(|| {
                aws_sdk_sqs::operation::get_queue_attributes::GetQueueAttributesError::generic(
                    aws_smithy_types::error::ErrorMetadata::builder()
                        .code("AWS.SimpleQueueService.NonExistentQueue")
                        .message("The specified queue does not exist")
                        .build(),
                )
            });

        let eb = aws_smithy_mocks::mock_client!(
            aws_sdk_elasticbeanstalk,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&env_listing(), &resources, &settings]
        );
        let sqs = aws_smithy_mocks::mock_client!(
            aws_sdk_sqs,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&main_attrs, &dlq_missing]
        );
        let s = Server::with_injected_client(
            WriteScope::None,
            crate::config::Config::default(),
            client_with(eb, sqs),
        );

        let out = s
            .call_tool(
                "worker_queues",
                &json!({"env": "poly-prod-wk", "peek": true}),
            )
            .await
            .expect("a missing derived DLQ must not fail the call");
        let v: Value = serde_json::from_str(&out).expect("valid JSON");

        assert_eq!(
            v["main_queue"]["stats"]["visible"], 3,
            "the depth answer we already had must survive: {out}"
        );
        assert_eq!(
            v["peeked"], false,
            "there was nothing to peek, so we did not look — saying \
                 `true` here claims an empty queue that does not exist: {out}"
        );
    }

    /// `why` must not record a spurious "we could not look" when the
    /// env simply has no dead-letter queue.
    ///
    /// Sibling of `a_derived_dlq_that_does_not_exist_still_answers`,
    /// and it exists because that fix was applied to `why` as well
    /// and left UNTESTED — mutating the gate away came back green.
    /// `why` is the body the injected-client seam was built for, so
    /// an untested fix here is the gap the seam exists to close.
    #[tokio::test]
    async fn why_records_no_dlq_error_when_there_is_no_dlq() {
        use aws_sdk_elasticbeanstalk::operation::describe_environment_resources::DescribeEnvironmentResourcesOutput;
        use aws_sdk_elasticbeanstalk::types::{EnvironmentResourceDescription, Queue};
        use aws_sdk_sqs::operation::get_queue_attributes::GetQueueAttributesOutput;
        use aws_sdk_sqs::types::QueueAttributeName;

        let resources = aws_smithy_mocks::mock!(EbClient::describe_environment_resources)
            .then_output(|| {
                DescribeEnvironmentResourcesOutput::builder()
                    .environment_resources(
                        EnvironmentResourceDescription::builder()
                            .queues(
                                Queue::builder()
                                    .name("WorkerQueue")
                                    .url("https://sqs/main")
                                    .build(),
                            )
                            .build(),
                    )
                    .build()
            });
        let settings = aws_smithy_mocks::mock!(EbClient::describe_configuration_settings)
                .then_output(|| {
                    aws_sdk_elasticbeanstalk::operation::describe_configuration_settings::DescribeConfigurationSettingsOutput::builder().build()
                });
        // `why` fans out to five more calls; every one errors here so
        // the bundle records them — which is fine, because the
        // assertion is specifically about `dlq_peek` NOT being among
        // them.
        let main_attrs = aws_smithy_mocks::mock!(SqsClient::get_queue_attributes)
            .match_requests(|req| req.queue_url() == Some("https://sqs/main"))
            .then_output(|| {
                GetQueueAttributesOutput::builder()
                    .attributes(QueueAttributeName::ApproximateNumberOfMessages, "0")
                    .build()
            });
        let dlq_missing = aws_smithy_mocks::mock!(SqsClient::get_queue_attributes)
            .match_requests(|req| req.queue_url() == Some("https://sqs/main-dlq"))
            .then_error(|| {
                aws_sdk_sqs::operation::get_queue_attributes::GetQueueAttributesError::generic(
                    aws_smithy_types::error::ErrorMetadata::builder()
                        .code("AWS.SimpleQueueService.NonExistentQueue")
                        .message("The specified queue does not exist")
                        .build(),
                )
            });

        // `why` also fans out to events, instances and versions.
        // The mock HARD-FAILS on an unmatched call rather than
        // erroring the section, so each needs a rule even though
        // this test asserts about none of them.
        let events = aws_smithy_mocks::mock!(EbClient::describe_events).then_output(|| {
            aws_sdk_elasticbeanstalk::operation::describe_events::DescribeEventsOutput::builder()
                .build()
        });
        let health = aws_smithy_mocks::mock!(EbClient::describe_instances_health)
                .then_output(|| {
                    aws_sdk_elasticbeanstalk::operation::describe_instances_health::DescribeInstancesHealthOutput::builder().build()
                });
        let versions = aws_smithy_mocks::mock!(EbClient::describe_application_versions)
                .then_output(|| {
                    aws_sdk_elasticbeanstalk::operation::describe_application_versions::DescribeApplicationVersionsOutput::builder().build()
                });
        let eb = aws_smithy_mocks::mock_client!(
            aws_sdk_elasticbeanstalk,
            aws_smithy_mocks::RuleMode::MatchAny,
            [
                &env_listing(),
                &resources,
                &settings,
                &events,
                &health,
                &versions
            ]
        );
        let sqs = aws_smithy_mocks::mock_client!(
            aws_sdk_sqs,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&main_attrs, &dlq_missing]
        );
        let s = Server::with_injected_client(
            WriteScope::None,
            crate::config::Config::default(),
            client_with(eb, sqs),
        );

        let out = s
            .call_tool("why", &json!({"env": "poly-prod-wk"}))
            .await
            .expect("why answers");
        let v: Value = serde_json::from_str(&out).expect("valid JSON");

        let errors = v["errors"].as_array().expect("errors array");
        assert!(
            !errors.iter().any(|e| e["section"] == "dlq_peek"),
            "an env with no dead-letter queue is ordinary — recording \
                 `dlq_peek` as a failure is triage noise in the array that \
                 exists to make partial answers load-bearing: {out}"
        );
        assert_eq!(
            v["queues"]["peeked"], false,
            "and we did not look, because there was nothing to look at: {out}"
        );
    }

    /// Confirm must act on the message the PLAN named, or refuse.
    ///
    /// The dangerous implementation re-receives at confirm and acts
    /// on whatever comes back — deleting the head of the queue,
    /// which may not be what the plan described, with nothing in
    /// the output saying they differed. A silent target swap.
    ///
    /// Here the message is gone by confirm time, which is ordinary:
    /// something else consumed, redrove or removed it in the token
    /// The LIVE batch counters, which demo does not exercise.
    ///
    /// Found by the scheduled mutation sweep: `succeeded += 1` in
    /// `dispatch_dlq_and_audit` survived being changed to `-=` and
    /// `*=`. The batch test that asserts `"succeeded":2` runs in
    /// demo, and the demo branch renders that field from
    /// `dlq_targets.len()` rather than from the counter — so the
    /// counter itself was never incremented by any test.
    ///
    /// Demo and live computing the same field two ways, with only
    /// the demo one covered, is the exact divergence this cycle
    /// kept finding elsewhere.
    #[tokio::test]
    async fn a_live_batch_counts_what_it_actually_deleted() {
        use aws_sdk_elasticbeanstalk::operation::describe_environment_resources::DescribeEnvironmentResourcesOutput;
        use aws_sdk_elasticbeanstalk::types::{EnvironmentResourceDescription, Queue};
        use aws_sdk_sqs::operation::get_queue_attributes::GetQueueAttributesOutput;
        use aws_sdk_sqs::operation::receive_message::ReceiveMessageOutput;
        use aws_sdk_sqs::types::{Message, QueueAttributeName};

        let resources = aws_smithy_mocks::mock!(EbClient::describe_environment_resources)
            .then_output(|| {
                DescribeEnvironmentResourcesOutput::builder()
                    .environment_resources(
                        EnvironmentResourceDescription::builder()
                            .queues(
                                Queue::builder()
                                    .name("WorkerQueue")
                                    .url("https://sqs/main")
                                    .build(),
                            )
                            .queues(
                                Queue::builder()
                                    .name("WorkerDeadLetterQueue")
                                    .url("https://sqs/main-dlq")
                                    .build(),
                            )
                            .build(),
                    )
                    .build()
            });
        let attrs = aws_smithy_mocks::mock!(SqsClient::get_queue_attributes).then_output(|| {
            GetQueueAttributesOutput::builder()
                .attributes(QueueAttributeName::ApproximateNumberOfMessages, "2")
                .build()
        });
        // The confirm-time re-read must ASK for enough depth to
        // find a whole batch. `dispatch_dlq_batch` requests
        // `DLQ_BATCH_CAP * 3`, and `peek_messages` pages that in
        // SQS's per-call maximum of 10 — so the first request asks
        // for 10. Shrinking the multiplier (the sweep mutated
        // `* 3` to `/ 3`) makes it ask for 3, and a 10-message
        // batch would then report seven of the messages the
        // operator approved as "not among those returned" while
        // dispatching the other three.
        //
        // Asserted on the REQUEST, because simulating SQS's
        // sampling would be testing the mock.
        let asked_for = std::sync::Arc::new(std::sync::atomic::AtomicI32::new(0));
        let seen = std::sync::Arc::clone(&asked_for);
        let peek = aws_smithy_mocks::mock!(SqsClient::receive_message)
            .match_requests(move |req| {
                seen.fetch_max(
                    req.max_number_of_messages().unwrap_or(0),
                    std::sync::atomic::Ordering::SeqCst,
                );
                true
            })
            .then_output(|| {
                ReceiveMessageOutput::builder()
                    .messages(
                        Message::builder()
                            .message_id("m-1")
                            .receipt_handle("rh-1")
                            .body("a")
                            .build(),
                    )
                    .messages(
                        Message::builder()
                            .message_id("m-2")
                            .receipt_handle("rh-2")
                            .body("b")
                            .build(),
                    )
                    .build()
            });
        let del = aws_smithy_mocks::mock!(SqsClient::delete_message).then_output(|| {
            aws_sdk_sqs::operation::delete_message::DeleteMessageOutput::builder().build()
        });
        let events = aws_smithy_mocks::mock!(EbClient::describe_events).then_output(|| {
            aws_sdk_elasticbeanstalk::operation::describe_events::DescribeEventsOutput::builder()
                .build()
        });
        let eb = aws_smithy_mocks::mock_client!(
            aws_sdk_elasticbeanstalk,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&env_listing(), &resources, &events]
        );
        let sqs = aws_smithy_mocks::mock_client!(
            aws_sdk_sqs,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&attrs, &peek, &del]
        );
        let s = Server::with_injected_client(
            WriteScope::All,
            crate::config::Config::default(),
            client_with(eb, sqs),
        );

        let plan = s
            .call_tool(
                "dlq_delete",
                &json!({"env": "poly-prod-wk", "message_ids": ["m-1", "m-2"]}),
            )
            .await
            .expect("both are present at plan time");
        let token = plan
            .split("\"confirm_token\":\"")
            .nth(1)
            .and_then(|r| r.split('"').next())
            .expect("a token")
            .to_string();

        let out = s
            .call_tool("confirm_action", &json!({"confirm_token": token}))
            .await
            .expect("both deletes succeed");
        let body: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{out}: {e}"));

        assert_eq!(
            body["succeeded"],
            json!(2),
            "the live counter must count what was actually deleted: {out}"
        );
        assert_eq!(body["failed"], json!(0), "{out}");
        assert_eq!(body["dispatched"], json!(true), "{out}");
        assert_eq!(
            body["results"].as_array().map(Vec::len),
            Some(2),
            "one result per message: {out}"
        );
        assert_eq!(
            asked_for.load(std::sync::atomic::Ordering::SeqCst),
            10,
            "the re-read must ask SQS for its per-call maximum, or a full batch \
                 cannot be found and approved messages report as missing"
        );
    }

    /// A resend whose delete half fails says a duplicate exists.
    ///
    /// Send-before-delete is deliberate: the other order can lose
    /// the message outright, while this one can at worst duplicate
    /// it. But when the delete fails the copy IS on the main queue
    /// and the original is still dead-lettered, and the batch
    /// report calls that `ok: false` — which invites the retry
    /// that mints another copy per attempt. The bare error said
    /// none of that. Found twice by the same reviewer, in two
    /// separate reviews.
    #[tokio::test]
    async fn a_resend_that_could_not_delete_says_a_duplicate_now_exists() {
        use aws_sdk_elasticbeanstalk::operation::describe_environment_resources::DescribeEnvironmentResourcesOutput;
        use aws_sdk_elasticbeanstalk::types::{EnvironmentResourceDescription, Queue};
        use aws_sdk_sqs::operation::get_queue_attributes::GetQueueAttributesOutput;
        use aws_sdk_sqs::operation::receive_message::ReceiveMessageOutput;
        use aws_sdk_sqs::types::{Message, QueueAttributeName};

        let resources = aws_smithy_mocks::mock!(EbClient::describe_environment_resources)
            .then_output(|| {
                DescribeEnvironmentResourcesOutput::builder()
                    .environment_resources(
                        EnvironmentResourceDescription::builder()
                            .queues(
                                Queue::builder()
                                    .name("WorkerQueue")
                                    .url("https://sqs/main")
                                    .build(),
                            )
                            .queues(
                                Queue::builder()
                                    .name("WorkerDeadLetterQueue")
                                    .url("https://sqs/main-dlq")
                                    .build(),
                            )
                            .build(),
                    )
                    .build()
            });
        let attrs = aws_smithy_mocks::mock!(SqsClient::get_queue_attributes).then_output(|| {
            GetQueueAttributesOutput::builder()
                .attributes(QueueAttributeName::ApproximateNumberOfMessages, "1")
                .build()
        });
        let peek = aws_smithy_mocks::mock!(SqsClient::receive_message).then_output(|| {
            ReceiveMessageOutput::builder()
                .messages(
                    Message::builder()
                        .message_id("m-1")
                        .receipt_handle("rh-m-1")
                        .body("payload")
                        .build(),
                )
                .build()
        });
        // The send SUCCEEDS...
        let send = aws_smithy_mocks::mock!(SqsClient::send_message).then_output(|| {
            aws_sdk_sqs::operation::send_message::SendMessageOutput::builder().build()
        });
        // ...and the delete does not. This is the half-failure.
        let del = aws_smithy_mocks::mock!(SqsClient::delete_message).then_error(|| {
            aws_sdk_sqs::operation::delete_message::DeleteMessageError::generic(
                aws_smithy_types::error::ErrorMetadata::builder()
                    .code("ReceiptHandleIsInvalid")
                    .message("handle expired")
                    .build(),
            )
        });
        let events = aws_smithy_mocks::mock!(EbClient::describe_events).then_output(|| {
            aws_sdk_elasticbeanstalk::operation::describe_events::DescribeEventsOutput::builder()
                .build()
        });
        let eb = aws_smithy_mocks::mock_client!(
            aws_sdk_elasticbeanstalk,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&env_listing(), &resources, &events]
        );
        let sqs = aws_smithy_mocks::mock_client!(
            aws_sdk_sqs,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&attrs, &peek, &send, &del]
        );
        let s = Server::with_injected_client(
            WriteScope::All,
            crate::config::Config::default(),
            client_with(eb, sqs),
        );

        let plan = s
            .call_tool(
                "dlq_resend",
                &json!({"env": "poly-prod-wk", "message_id": "m-1"}),
            )
            .await
            .expect("m-1 is present at plan time");
        let token = plan
            .split("\"confirm_token\":\"")
            .nth(1)
            .and_then(|r| r.split('"').next())
            .expect("a token")
            .to_string();

        let err = s
            .call_tool("confirm_action", &json!({"confirm_token": token}))
            .await
            .expect_err("nothing succeeded, so the call is an error");

        assert!(
            err.contains("RESENT BUT NOT REMOVED"),
            "the agent must be told the send half worked: {err}"
        );
        assert!(
            err.contains("duplicate now exists"),
            "and that a duplicate exists, or `ok: false` reads as nothing \
                 happened: {err}"
        );
        assert!(
            err.contains("DO NOT resend this id again"),
            "and must block the retry the batch report otherwise invites — each \
                 attempt adds another copy: {err}"
        );

        // The OTHER branch, or the condition is untested: a
        // dlq_delete whose delete fails has sent nothing, so
        // claiming a duplicate exists would be a false statement
        // about the main queue. One case per branch — a mutation
        // flipping the verb check to `true` passed until this
        // existed.
        let plan = s
            .call_tool(
                "dlq_delete",
                &json!({"env": "poly-prod-wk", "message_id": "m-1"}),
            )
            .await
            .expect("m-1 is still present");
        let token = plan
            .split("\"confirm_token\":\"")
            .nth(1)
            .and_then(|r| r.split('"').next())
            .expect("a token")
            .to_string();
        let err = s
            .call_tool("confirm_action", &json!({"confirm_token": token}))
            .await
            .expect_err("the delete fails");
        assert!(
            !err.contains("RESENT BUT NOT REMOVED") && !err.contains("duplicate"),
            "a delete sent nothing — claiming a duplicate is on the main queue \
                 would be a false statement about the fleet: {err}"
        );
        assert!(
            err.contains("\"ok\":false") && err.contains("failed\":1"),
            "it must still report the item as failed: {err}"
        );
        // Recorded rather than asserted: the cause renders as the
        // SDK's bare "service error", which tells an operator
        // nothing. That is `DeleteMessageError`'s Display, not
        // something this path adds, and wrapping every SQS error
        // is its own item — noted in PLAN.md rather than widened
        // into this one.
    }

    /// window. The confirm must refuse and say nothing changed.
    #[tokio::test]
    async fn a_dlq_delete_refuses_when_the_planned_message_is_gone() {
        use aws_sdk_elasticbeanstalk::operation::describe_environment_resources::DescribeEnvironmentResourcesOutput;
        use aws_sdk_elasticbeanstalk::types::{EnvironmentResourceDescription, Queue};
        use aws_sdk_sqs::operation::get_queue_attributes::GetQueueAttributesOutput;
        use aws_sdk_sqs::operation::receive_message::ReceiveMessageOutput;
        use aws_sdk_sqs::types::{Message, QueueAttributeName};

        let resources = aws_smithy_mocks::mock!(EbClient::describe_environment_resources)
            .then_output(|| {
                DescribeEnvironmentResourcesOutput::builder()
                    .environment_resources(
                        EnvironmentResourceDescription::builder()
                            .queues(
                                Queue::builder()
                                    .name("WorkerQueue")
                                    .url("https://sqs/main")
                                    .build(),
                            )
                            .queues(
                                Queue::builder()
                                    .name("WorkerDeadLetterQueue")
                                    .url("https://sqs/main-dlq")
                                    .build(),
                            )
                            .build(),
                    )
                    .build()
            });
        let attrs = aws_smithy_mocks::mock!(SqsClient::get_queue_attributes).then_output(|| {
            GetQueueAttributesOutput::builder()
                .attributes(QueueAttributeName::ApproximateNumberOfMessages, "2")
                .build()
        });
        // Plan time: m-1 is present. Confirm time: only m-2 is —
        // the head of the queue is now a DIFFERENT message, which
        // is exactly the swap this must not perform.
        let seq = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seq2 = std::sync::Arc::clone(&seq);
        let peek = aws_smithy_mocks::mock!(SqsClient::receive_message).then_output(move || {
            let first = seq2.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
            let id = if first { "m-1" } else { "m-2" };
            ReceiveMessageOutput::builder()
                .messages(
                    Message::builder()
                        .message_id(id)
                        .receipt_handle(format!("rh-{id}"))
                        .body("payload")
                        .build(),
                )
                .build()
        });

        // The plan path also pulls recent events for its summary.
        let events = aws_smithy_mocks::mock!(EbClient::describe_events).then_output(|| {
            aws_sdk_elasticbeanstalk::operation::describe_events::DescribeEventsOutput::builder()
                .build()
        });
        let eb = aws_smithy_mocks::mock_client!(
            aws_sdk_elasticbeanstalk,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&env_listing(), &resources, &events]
        );
        let sqs = aws_smithy_mocks::mock_client!(
            aws_sdk_sqs,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&attrs, &peek]
        );
        let s = Server::with_injected_client(
            WriteScope::All,
            crate::config::Config::default(),
            client_with(eb, sqs),
        );

        let plan = s
            .call_tool(
                "dlq_delete",
                &json!({"env": "poly-prod-wk", "message_id": "m-1"}),
            )
            .await
            .expect("m-1 is present at plan time");
        let token = plan
            .split("\"confirm_token\":\"")
            .nth(1)
            .and_then(|r| r.split('"').next())
            .expect("a token")
            .to_string();
        assert!(plan.contains("m-1"), "the plan names the message: {plan}");

        let err = s
            .call_tool("confirm_action", &json!({"confirm_token": token}))
            .await
            .expect_err("m-1 is gone by confirm time — this must refuse");
        assert!(
            err.contains("not among the messages returned when the queue was re-read"),
            "it must say the planned message was not found, not delete m-2 \
                 quietly: {err}"
        );
        // Scoped to what was observed. This said "is no longer in
        // the dead-letter queue", which is a firmer claim than a
        // receive can support: SQS returns a sample, so absence
        // from one read is evidence, not proof. Same rule as
        // `empty_queue_reason` and the `peeked` flag — say what was
        // looked at, not what is the case.
        assert!(
            !err.contains("dispatched\":true"),
            "a batch where nothing succeeded must not report a dispatch: {err}"
        );
        assert!(
            !err.contains("m-2"),
            "and must not have touched the message that IS there: {err}"
        );
    }
}

/// The MCP drift tool must canonicalise before discovery.
///
/// Source-scanned: the discovery path needs a real filesystem walk
/// and an AWS client to reach. `Path::new(".")` there means
/// discovery checks the server's own directory and nothing above
/// it, which contradicts the tool's own description.
#[test]
fn mcp_drift_discovery_starts_from_an_absolute_path() {
    // Was a raw read plus a hand-rolled split on
    // `"\n#[cfg(test)]\nmod "`. That split went decorative the moment
    // `tools.rs` started spelling its test modules with `#[path]` — it
    // matched nothing and returned the whole file, tests included.
    let prod = crate::app::tests::scan::production_source("cli/mcp/tools.rs");
    let prod = prod.as_str();
    assert!(
        prod.contains("std::env::current_dir()"),
        "drift discovery must start from the absolute cwd, or it \
             cannot walk up at all"
    );
    // Canary: prove the slice found real code.
    assert!(
        prod.contains("resolve_state_path("),
        "the production slice is not finding the resolution"
    );
}

/// Staleness is "the file changed", and unknown is never "changed".
///
/// Verified empirically before this was built: replacing a binary
/// underneath a running process changes its inode while
/// `current_exe()` still resolves the path. That is the whole
/// mechanism — a `brew upgrade` leaves the server on the old inode.
#[test]
fn the_stale_notice_says_whose_job_the_reconnect_is() {
    let n = stale_binary_notice("/usr/local/bin/ebman");
    assert!(
        n.contains("ASK YOUR OPERATOR"),
        "an agent reading `reconnect` imperatively will try it and fail — the \
             panel is a client surface it cannot reach: {n}"
    );
    assert!(
        n.contains("cannot do it yourself"),
        "and must be told why, or it will look for another way round: {n}"
    );
    assert!(
        n.contains("no MCP message a server can send"),
        "naming the missing primitive stops an agent hunting for a tool that \
             does not exist: {n}"
    );
    assert!(n.contains(env!("CARGO_PKG_VERSION")), "{n}");
    assert!(!n.contains("  "), "renders into one line: {n:?}");
}

#[test]
fn a_replaced_binary_is_stale_and_an_unreadable_one_is_not() {
    assert!(
        super::staleness(Some(100), Some(200)),
        "a different inode is a different file — that is an upgrade"
    );
    assert!(
        !super::staleness(Some(100), Some(100)),
        "the same file is not stale, however often it is checked"
    );
    // Unknown on either side must NOT report stale. A server crying
    // staleness whenever it cannot stat itself is noise, and noise
    // is how a real warning gets ignored.
    assert!(!super::staleness(None, Some(200)), "unknown start");
    assert!(!super::staleness(Some(100), None), "unknown now");
    assert!(!super::staleness(None, None), "unknown both");
}

/// The notice must be actionable, not merely alarming.
#[test]
fn the_stale_notice_says_what_to_do_about_it() {
    let n = super::stale_binary_notice("/opt/homebrew/bin/ebman");
    assert!(
        n.contains(env!("CARGO_PKG_VERSION")),
        "it must name the version actually RUNNING — the cached \
             instructions block cannot be refreshed mid-connection, so \
             this is the only truthful version an agent sees: {n}"
    );
    assert!(n.contains("/opt/homebrew/bin/ebman"), "and where: {n}");
    assert!(
        n.contains("reconnect") || n.contains("Reconnect"),
        "an agent cannot act on \"you are stale\" — it can act on \
             \"reconnect\": {n}"
    );
}

/// A current server must say nothing.
///
/// Without this the feature could prepend on every call and both
/// tests above would still pass — a warning on every response is
/// indistinguishable from no warning at all.
#[tokio::test]
async fn a_current_binary_adds_no_notice() {
    let s = demo_server();
    let resp = rpc(
        &s,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                   "params":{"name":"list_environments","arguments":{}}}),
    )
    .await
    .expect("answers");
    let text = resp["result"]["content"][0]["text"].as_str().expect("text");
    assert!(
        !text.contains("is running, but a different build"),
        "this binary has not been replaced, so there is nothing to \
             report: {text}"
    );
    // And the payload is still the payload — the prepend must not
    // have eaten it.
    assert!(text.starts_with('['), "still JSON: {text}");
}

/// A stale server must SAY so, on a real response.
///
/// The pure halves are tested above; this is the wiring. It points
/// the check at a file the test controls, replaces it — which is
/// what `brew upgrade` does — and drives a real tool call.
#[tokio::test]
async fn a_stale_server_prepends_the_notice_to_its_answer() {
    let dir = std::env::temp_dir().join(format!("ebman-exe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let fake = dir.join("ebman");
    std::fs::write(&fake, b"v1").expect("write");

    let s = Server::with_scope(true, false, WriteScope::None).watching_exe(&fake);

    // Unchanged: no notice.
    let quiet = rpc(
        &s,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                   "params":{"name":"list_environments","arguments":{}}}),
    )
    .await
    .expect("answers");
    assert!(
        !quiet["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("a different build"),
        "nothing has changed yet"
    );

    // Replace the file — a new inode, as an upgrade produces.
    let replacement = dir.join("ebman.new");
    std::fs::write(&replacement, b"v2").expect("write");
    std::fs::rename(&replacement, &fake).expect("rename");

    let loud = rpc(
        &s,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
                   "params":{"name":"list_environments","arguments":{}}}),
    )
    .await
    .expect("answers");
    let text = loud["result"]["content"][0]["text"].as_str().expect("text");
    assert!(
        text.contains("a different build is now installed"),
        "the server must say the binary changed underneath it: {text}"
    );
    assert!(
        text.contains(env!("CARGO_PKG_VERSION")),
        "naming the version actually running, which is the only \
             truthful one an agent sees once instructions are cached: {text}"
    );
    assert!(
        text.contains('['),
        "and the payload must survive the prepend: {text}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A lint RESULT must name the rules that cannot fire.
///
/// The description said so and that was not enough. Linting an
/// environment that was Yellow because of a dead-lettered message
/// returned two unrelated findings and nothing about the queue — so
/// the reader sees findings, concludes lint has looked, and the
/// actual cause of the health state is invisible. Observed against a
/// live fleet.
#[tokio::test]
async fn a_lint_result_names_the_rules_it_could_not_check() {
    let resp = rpc(
        &demo_server(),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                   "params":{"name":"lint","arguments":{}}}),
    )
    .await
    .expect("lint answers");
    let body = resp["result"]["content"][0]["text"].as_str().expect("text");
    let v: Value = serde_json::from_str(body).expect("valid JSON");

    let not_checked = v["rules_not_checked"]
        .as_array()
        .unwrap_or_else(|| panic!("every lint result must say what it could not check: {body}"));
    let joined = not_checked
        .iter()
        .filter_map(|x| x.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        joined.contains("EBL011"),
        "the worker-DLQ rule never fires here and the result must say \
             so, not only the description: {body}"
    );
    assert!(
        joined.contains("worker_queues"),
        "and must point at the tool that DOES see queues — a caveat \
             that points nowhere leaves the reader with \"lint says it is \
             fine\": {body}"
    );
    // The findings themselves must survive alongside it.
    assert!(v["issues"].is_array(), "{body}");
}

/// Rule 6: a result must carry its own negative space.
///
/// Not a sweep — what "negative space" looks like differs per tool,
/// so there is nothing generic to walk. This pins the five places
/// the remedy already exists, each added after someone was misled
/// rather than before, so removing one is a deliberate act rather
/// than an oversight.
///
/// The list is the point. Five separate features saying the same
/// thing — *here is the edge of what I looked at* — and every one
/// arrived as a bug report. ARCHITECTURE.md states the rule so the
/// sixth is designed in.
#[test]
fn every_result_that_can_be_partial_says_so() {
    // PRODUCTION halves only. Reading whole files let this test's
    // own field names satisfy it — the guard was its own evidence,
    // and removing a field came back green. Caught by mutation, and
    // it is the second time in this file that a source scan has
    // needed protecting from itself.
    let prod = crate::app::tests::scan::production_source;
    let src = format!(
        "{}{}",
        prod("src/cli/mcp/tools.rs"),
        prod("src/cli/mcp/mod.rs")
    );
    // Canary: the slices must be finding real code.
    assert!(
        src.contains("pub(super) fn append_cannot_fire"),
        "the production slice is not finding tools.rs"
    );

    for (field, what_it_bounds) in [
        ("rules_not_checked", "lint rules that cannot fire over MCP"),
        ("skipped_envs", "environments whose input fetch failed"),
        ("peeked", "whether a dead-letter queue was actually read"),
        ("complete", "whether a log window was fully consumed"),
        ("errors", "which sections of a `why` bundle failed"),
    ] {
        assert!(
            src.contains(field),
            "`{field}` is gone — it bounded {what_it_bounds}, and \
                 without it that absence reads as an answer. See rule 6 \
                 in ARCHITECTURE.md before removing it."
        );
    }
}

/// A narrowed grant advertises only what it named, and refuses the
/// rest at dispatch too.
///
/// The forcing case: a client wanted to delete one dead-lettered
/// message, and the only grant available also handed it terminate
/// across every environment the credentials reach.
#[tokio::test]
async fn a_scoped_grant_advertises_and_dispatches_only_its_verbs() {
    let scope = WriteScope::Only(vec!["dlq_delete".into(), "dlq_resend".into()]);
    let s = Server::with_config(true, false, scope, crate::config::Config::default());

    let resp = rpc(&s, json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .await
        .expect("tools/list");
    let names: Vec<String> = resp["result"]["tools"]
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_string))
        .collect();

    assert!(names.iter().any(|n| n == "dlq_delete"), "{names:?}");
    assert!(names.iter().any(|n| n == "dlq_resend"), "{names:?}");
    for ungranted in [
        "terminate",
        "deploy",
        "restart",
        "rebuild",
        "set_option",
        "dlq_purge",
    ] {
        assert!(
            !names.iter().any(|n| n == ungranted),
            "`{ungranted}` was not granted and must not be advertised — a \
                 client that cannot see a tool does not plan around it: {names:?}"
        );
    }
    // Reads are unaffected by the scope.
    assert!(names.iter().any(|n| n == "list_environments"), "{names:?}");
    // And the confirm half of the two-phase protocol rides along:
    // without it the grant could plan a delete and never dispatch
    // one, which is a broken server rather than a narrow one.
    assert!(
        names.iter().any(|n| n == "confirm_action"),
        "a narrow grant must still be able to confirm what it planned: {names:?}"
    );

    // The agent must be able to tell "not granted" from "ebman
    // can't do this" — a withheld tool is simply absent from
    // tools/list, and absent alone is ambiguous. tools/list reaches
    // the agent; the REASON only does if the server authors it.
    let init = rpc(
        &s,
        json!({"jsonrpc":"2.0","id":0,"method":"initialize",
                   "params":{"protocolVersion":"2025-06-18","capabilities":{},
                             "clientInfo":{"name":"t","version":"0"}}}),
    )
    .await
    .expect("initialize");
    let instructions = init["result"]["instructions"]
        .as_str()
        .expect("instructions are the only version/scope signal an agent is guaranteed");
    assert!(
        instructions.contains("dlq_delete") && instructions.contains("dlq_resend"),
        "the instructions must name what WAS granted: {instructions}"
    );
    assert!(
        instructions.contains("NOT GRANTED"),
        "and say that the rest is withheld rather than missing: {instructions}"
    );

    // Calling an ungranted verb must say SO, not "unknown tool".
    // "Unknown" is a lie in the shape that matters: it teaches the
    // agent ebman lacks terminate, which directly contradicts the
    // scope line in `instructions` and is the reading that sends it
    // off to report a capability gap instead of asking.
    let call = rpc(
        &s,
        json!({"jsonrpc":"2.0","id":9,"method":"tools/call",
                   "params":{"name":"terminate","arguments":{"env":"demo-prod"}}}),
    )
    .await
    .expect("tools/call");
    assert!(
        call.get("error").is_none(),
        "an ungranted verb is a tool refusal, not a protocol error: {call}"
    );
    let text = call["result"]["content"][0]["text"]
        .as_str()
        .expect("text content");
    assert!(
        text.contains("not in this server's write scope"),
        "the refusal must name the scope: {text}"
    );
    assert!(
        !text.contains("unknown tool"),
        "and must not claim the tool does not exist: {text}"
    );
    assert_eq!(call["result"]["isError"], json!(true));

    // A genuinely unknown name is still a protocol error.
    let bogus = rpc(
        &s,
        json!({"jsonrpc":"2.0","id":10,"method":"tools/call",
                   "params":{"name":"no_such_tool","arguments":{}}}),
    )
    .await
    .expect("tools/call");
    assert_eq!(bogus["error"]["code"], json!(-32602), "{bogus}");

    // And the dispatch gate refuses it even if a client held a
    // cached list from a wider grant.
    let err = s
        .tool_write_plan_for_tests(writes::WriteVerb::Terminate, &json!({"env": "x"}))
        .await
        .expect_err("terminate is outside the scope");
    assert!(
        err.contains("not in this server's write scope"),
        "the refusal must name the scope, not read as a missing tool: {err}"
    );
    assert!(
        err.contains("--allow-writes=terminate"),
        "and say how to grant it: {err}"
    );

    // The positive half: a granted verb gets PAST the scope gate.
    // Without this the test would still pass if `allows` returned
    // false for everything, which is a grant that grants nothing.
    let granted = s
        .tool_write_plan_for_tests(writes::WriteVerb::DlqDelete, &json!({}))
        .await;
    if let Err(e) = &granted {
        assert!(
            !e.contains("not in this server's write scope"),
            "`dlq_delete` was granted and must clear the scope gate \
                 (any later complaint about arguments is fine): {e}"
        );
    }
}

/// An unknown verb in the flag is an error, never a silent skip.
///
/// A typo'd `--allow-writes=dlq_delte` that quietly granted nothing
/// looks exactly like a working narrow grant until the first write
/// is refused; one that quietly granted everything would be worse.
#[test]
fn a_mistyped_write_verb_is_refused_at_startup() {
    let known = ["dlq_delete", "terminate"];
    let err = super::parse_write_scope(Some("dlq_delte"), &known)
        .expect_err("a typo must not be silently ignored");
    assert!(err.contains("dlq_delte"), "name the typo: {err}");
    assert!(err.contains("dlq_delete"), "and list what IS known: {err}");

    // Bare `--allow-writes` keeps meaning everything, so an
    // existing .mcp.json is unaffected.
    assert_eq!(
        super::parse_write_scope(None, &known).expect("bare is valid"),
        WriteScope::All
    );
    // `--allow-writes=` with nothing after it is a mistake, not an
    // empty grant that silently disables writes.
    assert!(super::parse_write_scope(Some(""), &known).is_err());
    assert!(super::parse_write_scope(Some("  , ,"), &known).is_err());

    assert_eq!(
        super::parse_write_scope(Some("terminate, dlq_delete ,terminate"), &known).expect("valid"),
        WriteScope::Only(vec!["terminate".into(), "dlq_delete".into()]),
        "whitespace tolerated, duplicates collapsed, order kept"
    );
}

/// Every verb round-trips: the name `--allow-writes` accepts is
/// the name the tool is advertised as is the name the dispatch
/// gate checks.
///
/// Three spellings of one thing, and two of them are hand-written.
/// Drift between them does not fail loudly — it makes
/// `--allow-writes=dlq_purge` advertise the tool and then refuse it
/// at dispatch, which reads as a broken server rather than a typo.
/// A mutation changing one arm of `tool_name` to `dlq-purge` walked
/// straight past the first version of this guard.
#[test]
fn every_write_verb_round_trips_through_the_tool_table() {
    let nameable = writes::write_verb_names();
    assert_eq!(
        nameable.len(),
        writes::WriteVerb::ALL.len(),
        "a write verb exists in one table and not the other: nameable={nameable:?}"
    );

    let advertised: Vec<String> = tool_table(&WriteScope::All, true)
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_string))
        .collect();

    for verb in writes::WriteVerb::ALL {
        let name = verb.tool_name();
        assert!(
            nameable.iter().any(|n| n == name),
            "`{name}` is what the dispatch gate checks but not a name \
                 --allow-writes accepts: {nameable:?}"
        );
        assert!(
            advertised.contains(&name.to_string()),
            "`{name}` is checkable but never advertised: {advertised:?}"
        );

        // One discriminating case per verb: a grant of exactly this
        // verb admits it and nothing else. Sampling a single verb
        // would leave the other seven asserted by analogy.
        let only = WriteScope::Only(vec![name.to_string()]);
        assert!(only.allows(name), "a grant of `{name}` must allow it");
        for other in writes::WriteVerb::ALL {
            if other.tool_name() != name {
                assert!(
                    !only.allows(other.tool_name()),
                    "a grant of `{name}` must not allow `{}`",
                    other.tool_name()
                );
            }
        }
    }
}

/// A second `--allow-writes` is an error, not last-wins.
///
/// `--allow-writes=dlq_delete --allow-writes` under last-wins
/// silently widens a deliberately narrow grant to every verb —
/// the fail-open this flag exists to remove, reachable by a stray
/// duplicate in a hand-edited `.mcp.json` args array.
#[test]
fn a_repeated_write_flag_cannot_silently_widen_the_grant() {
    let args = |v: &[&str]| -> Vec<String> {
        std::iter::once("mcp")
            .chain(std::iter::once("serve"))
            .chain(v.iter().copied())
            .map(str::to_string)
            .collect()
    };

    let err = parse_mcp_args(&args(&["--allow-writes=dlq_delete", "--allow-writes"]))
        .expect_err("a second flag must not widen the first");
    assert!(
        err.contains("more than once"),
        "the error must name the duplication: {err}"
    );
    assert!(
        err.contains("--allow-writes=a,b"),
        "and say what to do instead: {err}"
    );
    // The rendered message, not the literal: a wrapped string
    // without a continuation embeds the newline and the next
    // line's indent, and this one goes to a one-line status bar.
    assert!(
        !err.contains("  "),
        "a wrapped literal left a gap in the rendered message: {err:?}"
    );

    // Narrowing order does not rescue it either.
    assert!(parse_mcp_args(&args(&["--allow-writes", "--allow-writes=dlq_delete"])).is_err());

    // One flag is still fine.
    let ok = parse_mcp_args(&args(&["--allow-writes=dlq_delete"])).expect("one flag is valid");
    assert_eq!(ok.write_scope, WriteScope::Only(vec!["dlq_delete".into()]));
}

/// A grant of nothing must read as nothing.
///
/// The parser cannot build an empty `Only`, but a scope answering
/// `any() == true` while `allows()` refused every verb would
/// advertise `confirm_action` beside no verb to confirm — a server
/// that looks write-capable and is not.
#[test]
fn an_empty_grant_is_not_a_grant() {
    let empty = WriteScope::Only(Vec::new());
    assert!(
        !empty.any(),
        "an empty grant must not read as write-capable"
    );
    let names: Vec<String> = tool_table(&empty, true)
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_string))
        .collect();
    assert!(
        !names.iter().any(|n| n == "confirm_action"),
        "nothing to confirm, so nothing should offer to confirm it: {names:?}"
    );
}

/// The audit-init decision, pinned outside `run`.
///
/// A reads-only server must not read the config disk, and a demo
/// server must not acquire a webhook. Both are decided on one line
/// inside the process entry, where no lib test reaches — the
/// mutation sweep found `&&` → `||` and a dropped `!` both survive
/// there, which is why the decision moved out.
#[test]
fn only_a_live_write_capable_server_reads_the_audit_config() {
    let narrow = WriteScope::Only(vec!["dlq_delete".into()]);

    assert!(should_init_audit(&WriteScope::All, false));
    assert!(
        should_init_audit(&narrow, false),
        "a narrow grant still dispatches writes, so it still audits"
    );

    assert!(
        !should_init_audit(&WriteScope::None, false),
        "a reads-only server must not touch the config disk"
    );
    assert!(
        !should_init_audit(&WriteScope::All, true),
        "demo must not acquire a webhook — `||` here would give it one"
    );
    assert!(
        !should_init_audit(&WriteScope::None, true),
        "and neither half alone is enough — dropping the `!` inverts this"
    );
}

/// `serve` logs to a file; `setup` writes nothing.
///
/// Both directions are load-bearing: `setup`'s module doc promises
/// it writes no files, and `serve` is a daemon whose only voice is
/// the log — it had none, so the elicitation measurement stage 5
/// waits on recorded nothing at all.
#[test]
fn only_the_mcp_daemon_opens_a_log_file() {
    let a = |v: &[&str]| -> Vec<String> { v.iter().map(|s| s.to_string()).collect() };

    assert!(wants_file_logging(&a(&["mcp", "serve"])));
    assert!(wants_file_logging(&a(&[
        "mcp",
        "serve",
        "--demo",
        "--allow-writes"
    ])));

    assert!(
        !wants_file_logging(&a(&["mcp", "setup"])),
        "setup is a pure printer that promises it writes no files"
    );
    assert!(!wants_file_logging(&a(&["mcp", "setup", "--allow-writes"])));
    assert!(!wants_file_logging(&a(&["mcp"])), "a bare usage error");
}

/// The instructions tell the agent that a grant is not its to make.
///
/// A peer session on 0.40.0 went to scope its own server to
/// `--allow-writes=dlq_delete` — the narrowest grant, the exact
/// case the feature was built for — and its client refused the
/// edit as self-modification. The refusal was correct; the wasted
/// attempt was avoidable. Nothing on this surface said whose job
/// the edit was, and `tools/list` showing no write tools reads as
/// "go and enable them".
#[test]
fn the_instructions_say_a_grant_is_not_the_agents_to_make() {
    let read_only = WriteScope::None.agent_summary(None, false, false);
    assert!(
        read_only.contains("Do not edit the MCP config yourself"),
        "a read-only server must say whose job the grant is: {read_only}"
    );
    assert!(
        read_only.contains("self-modification"),
        "and warn that trying costs a denial: {read_only}"
    );

    // A narrow grant needs the other half: the verb it is missing
    // may have been granted already and not picked up.
    let narrow = WriteScope::Only(vec!["dlq_delete".into()]).agent_summary(None, false, false);
    assert!(
        narrow.contains("restart the client"),
        "a client that reconnects without re-reading its config shows the \
             verb as absent, which reads as a broken feature: {narrow}"
    );
    assert!(narrow.contains("Asking is your part"), "{narrow}");
}

/// A standing refusal outranks the grant in the instructions.
///
/// The block is the one channel an agent is guaranteed to read. On
/// a server with `safety.read_only` set it said "Writes are
/// ENABLED for every verb" — true about the flag, false about the
/// server, and the sentence an agent would plan an afternoon
/// around. `doctor` reported it correctly, which is not a defence:
/// a tool the agent has to think to call is not the same as the
/// text it is handed.
#[tokio::test]
async fn a_standing_refusal_outranks_the_grant_in_the_instructions() {
    let cfg = crate::config::Config {
        safety_read_only: true,
        ..crate::config::Config::default()
    };
    let s = Server::with_config(true, false, WriteScope::All, cfg);
    let init = rpc(
        &s,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize",
                   "params":{"protocolVersion":"2025-06-18","capabilities":{},
                             "clientInfo":{"name":"t","version":"0"}}}),
    )
    .await
    .expect("initialize");
    let ins = init["result"]["instructions"]
        .as_str()
        .expect("instructions");

    assert!(ins.contains("Writes are REFUSED"), "{ins}");
    assert!(
        ins.contains("safety.read_only"),
        "and name the control: {ins}"
    );
    assert!(
        !ins.contains("Writes are ENABLED"),
        "the grant must not be described on a server that refuses every write: {ins}"
    );

    // An unreadable safety config fails closed the same way, and
    // says so rather than describing a grant it will not honour.
    let broken = crate::config::Config {
        safety_parse_errors: vec!["safety.envs.prod = true is missing .read_only".into()],
        ..crate::config::Config::default()
    };
    let b = Server::with_config(true, false, WriteScope::All, broken);
    assert!(
        b.write_scope
            .agent_summary(Some("the safety config could not be parsed."), false, false)
            .contains("REFUSED"),
        "a fail-closed parse refuses every write and the block must say so"
    );

    // The control: an unrestricted server still describes its grant.
    let open = Server::with_config(
        true,
        false,
        WriteScope::All,
        crate::config::Config::default(),
    );
    assert!(
        open.write_scope
            .agent_summary(None, false, false)
            .contains("ENABLED"),
        "without a standing refusal the grant is the right thing to describe"
    );
}

/// A call that can ask a human gets a human's budget.
///
/// `TOOL_TIMEOUT_SECS` is 30 and bounds a hung AWS call, which is
/// what it was written for. Applied to a person deciding whether
/// to delete production data it turns the gate into a tool that
/// times out under ordinary use — the failure a reviewer flagged
/// as "the difference between a gate and a thing that breaks when
/// used".
#[test]
fn only_the_call_that_asks_a_human_gets_a_humans_budget() {
    // The one tool every write dispatches through, on a connection
    // that can be asked.
    assert_eq!(
        call_timeout_secs(writes::CONFIRM_TOOL, true),
        ASK_TIMEOUT_SECS
    );
    // A connection that cannot be asked keeps the AWS bound —
    // nothing will stop to ask, so a long budget would only make a
    // hung dispatch take longer to fail.
    assert_eq!(
        call_timeout_secs(writes::CONFIRM_TOOL, false),
        TOOL_TIMEOUT_SECS
    );

    // Every other tool is AWS-bound whatever the client declared.
    for t in [
        "list_environments",
        "worker_queues",
        "deploy",
        "terminate",
        "doctor",
    ] {
        assert_eq!(
            call_timeout_secs(t, true),
            TOOL_TIMEOUT_SECS,
            "`{t}` cannot block on a person and must keep the AWS bound"
        );
    }
}

/// The frame loop uses the budget rather than the raw constant.
///
/// `call_timeout_secs` is pure and tested, and that proves
/// nothing about the one place it matters: a mutation swapping
/// `budget` back for `TOOL_TIMEOUT_SECS` at the call site left the
/// whole suite green. The loop is reachable only through stdio, so
/// this is anchored in source — the same shape as
/// `the_extracted_gates_are_wired_into_run`.
#[test]
fn the_frame_loop_times_calls_by_the_computed_budget() {
    let src = crate::app::tests::scan::production_source("cli/mcp/mod.rs");
    let src = src.as_str();
    let prod = src;
    let call = prod
        .split("let outcome = tokio::time::timeout(")
        .nth(1)
        .and_then(|r| r.split(".await").next())
        .expect("the frame loop times the tool call here");

    assert!(
        call.contains("from_secs(budget)"),
        "the tool call must be bounded by the COMPUTED budget: a person deciding \
             whether to delete production data gets the AWS bound otherwise, and the \
             gate times out under ordinary use:\n{call}"
    );
    assert!(
        !call.contains("TOOL_TIMEOUT_SECS"),
        "and not by the raw constant, which is what it was before:\n{call}"
    );

    // Canary: the anchor must still find the loop. A scan that has
    // stopped seeing its subject passes silently, which is worse
    // than finding a defect.
    assert!(
        prod.contains("let budget = call_timeout_secs("),
        "the budget is no longer computed in this file — this guard has lost \
             its subject rather than being satisfied"
    );
}

/// A reply that is not a clear "accept" is not an approval.
///
/// This is the one place where being generous to a malformed
/// client converts a broken reply into a standing yes. Every
/// shape that is not exactly `accept` denies.
#[test]
fn only_an_explicit_accept_approves() {
    let accept = json!({"jsonrpc":"2.0","id":1,"result":{"action":"accept"}});
    assert_eq!(ask_outcome_from(&accept), AskOutcome::Approved);

    for reply in [
        json!({"jsonrpc":"2.0","id":1,"result":{"action":"decline"}}),
        json!({"jsonrpc":"2.0","id":1,"result":{"action":"cancel"}}),
        // Malformed, missing, wrong type, an error response, a
        // result that is not an object — none of these is consent.
        json!({"jsonrpc":"2.0","id":1,"result":{"action":"ACCEPT"}}),
        json!({"jsonrpc":"2.0","id":1,"result":{"action":true}}),
        json!({"jsonrpc":"2.0","id":1,"result":{}}),
        json!({"jsonrpc":"2.0","id":1,"result":"accept"}),
        json!({"jsonrpc":"2.0","id":1}),
    ] {
        assert_eq!(
            ask_outcome_from(&reply),
            AskOutcome::Declined,
            "not an explicit accept, so not an approval: {reply}"
        );
    }

    // An ERROR reply is not a decline. The client could not put
    // the question at all; nobody refused. It still denies — only
    // the label changes, and the label is what an agent repeats to
    // its user.
    let errored = json!({"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"no"}});
    assert_eq!(
        ask_outcome_from(&errored),
        AskOutcome::Unsupported,
        "a client that could not present the question has not declined it"
    );
    assert!(
        ask_outcome_from(&errored).refuses(),
        "and it must still deny"
    );
}

/// Not-asked and unanswered have opposite consequences.
#[test]
fn not_asked_is_not_the_same_as_unanswered() {
    assert!(
        AskOutcome::Unanswered.refuses(),
        "an ask nobody answered denies"
    );
    assert!(AskOutcome::Declined.refuses());
    assert!(
        !AskOutcome::NotAsked.refuses(),
        "no question was put, so there is no answer to respect — conflating \
             these denied every write on every client that cannot elicit, including \
             ones the operator had granted with the flag"
    );
    assert_ne!(
        AskOutcome::NotAsked,
        AskOutcome::Approved,
        "nor is it an approval"
    );
}

/// A client that cannot elicit is not asked, and is not refused.
#[tokio::test]
async fn a_client_without_elicitation_is_not_asked() {
    let s = Server::with_scope(true, false, WriteScope::All);
    assert_eq!(
        s.ask_operator("delete something", None).await,
        AskOutcome::NotAsked
    );
}

/// An ask with nobody listening denies rather than hanging.
#[tokio::test(start_paused = true)]
async fn an_unanswered_ask_denies_when_the_budget_expires() {
    let s = Server::with_scope(true, false, WriteScope::All);
    s.client_supports_elicitation
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(4);
    if let Ok(mut slot) = s.outbound.lock() {
        *slot = Some(tx);
    }

    let asked = tokio::spawn(async move { s.ask_operator("delete something", None).await });
    // The question goes out...
    let frame: Value = serde_json::from_str(&rx.recv().await.expect("a frame")).expect("json");
    assert_eq!(frame["method"], json!("elicitation/create"), "{frame}");
    assert!(
        frame["params"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("delete something")),
        "the question must carry what is being asked: {frame}"
    );

    // ...and nobody ever replies.
    tokio::time::advance(std::time::Duration::from_secs(ASK_TIMEOUT_SECS + 1)).await;
    // Past ASK_WAIT_SECS, which is the one that fires.
    let outcome = asked.await.expect("join");
    assert_eq!(
        outcome,
        AskOutcome::Unanswered,
        "an operator who walked away must produce a deny, not a call that \
             never returns"
    );
}

/// The answer reaches the waiting ask.
///
/// A reply has an id and no method, which the frame loop would
/// otherwise answer `-32601` while the ask sat waiting out its
/// budget — denying a write the operator had just approved.
#[tokio::test]
async fn an_answer_is_routed_back_to_the_ask_that_is_waiting() {
    let s = std::sync::Arc::new(Server::with_scope(true, false, WriteScope::All));
    s.client_supports_elicitation
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(4);
    if let Ok(mut slot) = s.outbound.lock() {
        *slot = Some(tx);
    }

    let asking = {
        let s = std::sync::Arc::clone(&s);
        tokio::spawn(async move { s.ask_operator("terminate prod", None).await })
    };
    let frame: Value = serde_json::from_str(&rx.recv().await.expect("frame")).expect("json");
    let id = frame["id"].clone();

    let reply = json!({"jsonrpc": "2.0", "id": id, "result": {"action": "accept"}});
    assert!(
        s.take_ask_reply(&reply),
        "a frame carrying an id we issued, with no method, is ours"
    );
    assert_eq!(asking.await.expect("join"), AskOutcome::Approved);

    // Someone else's frame is not ours, and must fall through to
    // the normal dispatch rather than being swallowed.
    assert!(!s.take_ask_reply(&json!({"jsonrpc":"2.0","id":7,"result":{}})));
    assert!(!s.take_ask_reply(&json!({"jsonrpc":"2.0","id":1,"method":"ping"})));
}

/// A demo server whose client can elicit, with a stand-in for the
/// frame loop answering every ask with `action` and recording what
/// it was asked.
fn demo_answering(
    action: &'static str,
) -> (
    std::sync::Arc<Server>,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) {
    let s = std::sync::Arc::new(demo_writes_server());
    s.client_supports_elicitation
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    if let Ok(mut slot) = s.outbound.lock() {
        *slot = Some(tx);
    }
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = std::sync::Arc::clone(&asked);
    let srv = std::sync::Arc::clone(&s);
    tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            let frame: Value = serde_json::from_str(&line).expect("json");
            if let Some(m) = frame["params"]["message"].as_str() {
                seen.lock().expect("lock").push(m.to_string());
            }
            // Exactly what the real frame loop does with a reply.
            let reply = json!({
                "jsonrpc": "2.0",
                "id": frame["id"].clone(),
                "result": {"action": action},
            });
            assert!(srv.take_ask_reply(&reply), "the loop must route it back");
        }
    });
    (s, asked)
}

/// The gate is *wired*, not merely present.
///
/// `ask_operator` having its own passing tests says nothing about
/// whether `confirm_action` consults it — four separate defects
/// this cycle were a tested function nothing called. So: decline,
/// and assert the dispatch did not happen.
#[tokio::test]
async fn a_declined_ask_stops_the_write() {
    let (s, asked) = demo_answering("decline");
    let env = demo_fixture::envs()[0].name.clone();
    let (err, plan) = call(&s, "restart", json!({"env": &env})).await;
    assert!(!err, "planning is not gated: {plan}");
    let token = plan["confirm_token"].as_str().expect("token").to_string();

    let (err, out) = call(&s, "confirm_action", json!({"confirm_token": token})).await;
    assert!(err, "a declined ask must refuse the write: {out}");
    let text = out.to_string();
    assert!(
        text.contains("declined"),
        "the refusal must say the operator declined, not blame a missing \
             flag or an expired token: {text}"
    );
    assert!(
        !text.contains("--allow-writes"),
        "and must not send the agent off to widen permissions it already \
             has — the answer was no: {text}"
    );

    let asked = asked.lock().expect("lock");
    assert_eq!(asked.len(), 1, "exactly one ask: {asked:?}");
    assert!(
        asked[0].contains(&env) && asked[0].to_lowercase().contains("restart"),
        "the operator must be told which verb on which environment, or the \
             question is unanswerable: {:?}",
        asked[0]
    );
}

/// And an approval lets it through — otherwise "it refuses" is
/// satisfied by a gate that refuses everything.
#[tokio::test]
async fn an_approved_ask_lets_the_write_through() {
    let (s, asked) = demo_answering("accept");
    let env = demo_fixture::envs()[0].name.clone();
    let (_, plan) = call(&s, "restart", json!({"env": env})).await;
    let token = plan["confirm_token"].as_str().expect("token").to_string();

    let (err, out) = call(&s, "confirm_action", json!({"confirm_token": token})).await;
    assert!(!err, "an approved write must dispatch: {out}");
    assert_eq!(asked.lock().expect("lock").len(), 1, "and must still ask");
}

/// The ask happens once per dispatch, not once per plan.
///
/// A spent token must not re-ask: an agent replaying a used token
/// would otherwise put the same question to the operator again,
/// and repeated identical prompts are how consent gets clicked
/// through.
#[tokio::test]
async fn a_spent_token_does_not_ask_again() {
    let (s, asked) = demo_answering("accept");
    let env = demo_fixture::envs()[0].name.clone();
    let (_, plan) = call(&s, "restart", json!({"env": env})).await;
    let token = plan["confirm_token"].as_str().expect("token").to_string();

    assert!(
        !call(&s, "confirm_action", json!({"confirm_token": &token}))
            .await
            .0
    );
    assert!(
        call(&s, "confirm_action", json!({"confirm_token": &token}))
            .await
            .0,
        "the token is single-use"
    );
    assert_eq!(
        asked.lock().expect("lock").len(),
        1,
        "the replay must be rejected before anyone is asked"
    );
}

/// The frame loop must claim an ask reply *before* it dispatches.
///
/// The loop reads stdin and can't be called here, so this pins the
/// two halves separately: that interception is load-bearing
/// (below), and that the source has it in the right place
/// (`take_ask_reply_precedes_the_dispatch`). Without it a reply is
/// answered `-32601`, the ask waits out its full budget, and a
/// write the operator *approved* is denied — indistinguishable
/// from a timeout, so it would be diagnosed as a slow operator
/// rather than a routing bug.
///
/// Note it is `handle_request`, not `invalid_request_response`,
/// that does the damage: that one only rejects non-objects, so a
/// reply sails straight past it. Written the other way round first,
/// and the test failed — the guard was aimed at a function that
/// would never have fired.
///
/// Since 0.42.0 the frame loop also DROPS an unclaimed response
/// before dispatch, so this describes what `handle_request` would
/// do rather than what the loop now does. Both guards are needed:
/// this one says why claiming matters, and
/// `a_reply_to_a_forgotten_ask_is_dropped_not_answered` says why
/// the drop sits between claim and dispatch.
#[tokio::test]
async fn an_unclaimed_reply_would_be_answered_as_a_bad_method() {
    let s = demo_writes_server();
    let reply = json!({"jsonrpc": "2.0", "id": 1_000_000, "result": {"action": "accept"}});
    assert!(
        invalid_request_response(&reply).is_none(),
        "the validity check passes it through — it only rejects non-objects"
    );
    let resp = s.handle_request(&reply).await.expect("a response");
    assert_eq!(
        resp["error"]["code"], -32601,
        "so an unclaimed reply gets method-not-found, and the approval is \
             lost: {resp}"
    );
}

/// ...and that the source actually intercepts before dispatching.
#[test]
fn take_ask_reply_precedes_the_dispatch() {
    let src = crate::app::tests::scan::production_source("cli/mcp/mod.rs");
    let src = src.as_str();
    let body = src;
    let claim = body
        .find("if server.take_ask_reply(&req)")
        .expect("the frame loop must route ask replies");
    let dispatch = body
        .find("server.handle_request(&req).await")
        .expect("the frame loop must dispatch requests");
    assert!(
        claim < dispatch,
        "take_ask_reply must come first: a reply has an id and no method, \
             so the dispatch below answers it -32601 and the ask denies a write \
             the operator had approved"
    );
}

/// The agent is told the ask exists — and only when it does.
///
/// Elicitation is negotiated in protocol metadata the *client*
/// consumes; none of it reaches the agent reading `instructions`.
/// So an agent on an ask-capable connection, not told, reads the
/// operator's decline as a permission error and retries — which is
/// the one response a decline must not produce.
#[test]
fn the_summary_says_whether_confirmations_are_put_to_a_person() {
    for scope in [
        WriteScope::All,
        WriteScope::Only(vec!["restart".into(), "dlq_delete".into()]),
    ] {
        let asked = scope.agent_summary(None, true, false);
        let silent = scope.agent_summary(None, false, false);
        assert!(
            asked.contains("put to the operator") || asked.contains("put to the"),
            "a granted scope on an ask-capable client must say the confirmation \
                 reaches a person: {asked}"
        );
        assert!(
            !asked.contains("a final answer from a human"),
            "and must NOT pre-load the agent with an attribution ebman cannot \
                 make. That sentence sat in the instructions block — read at connect, \
                 before any refusal text — and taught the exact false record the \
                 decline wording was rewritten to stop: {asked}"
        );
        assert!(
            asked.contains("decline"),
            "and must say what a decline means, or it reads as an error: {asked}"
        );
        assert!(
            !silent.contains("put to the"),
            "but must NOT promise an ask that cannot happen — on a client that \
                 can't elicit, nobody is reachable and the agent would wait for a \
                 human who is never shown anything: {silent}"
        );
    }
}

/// A standing refusal still outranks the ask note.
///
/// Otherwise a server that refuses every write tells the agent to
/// expect the operator to be asked — inviting it to plan a write
/// and wait on a question that will never be put.
#[test]
fn a_standing_refusal_outranks_the_ask_note() {
    let t = WriteScope::All.agent_summary(Some("safety.read_only is set."), true, true);
    assert!(t.contains("REFUSED"), "{t}");
    assert!(
        !t.contains("OPERATOR"),
        "nothing will be put to anyone on a server that refuses every write: {t}"
    );
}

/// Parity by default: a client that can be asked gets the write
/// surface with no flag at all.
///
/// This is the design's whole usability claim. Without it the
/// operator must stop, edit a config file and restart their client
/// the first time they want a write — which is the point at which
/// people stop bothering.
#[tokio::test]
async fn an_ask_capable_client_gets_writes_without_a_flag() {
    let s = Server::with_scope(true, false, WriteScope::None);
    assert_eq!(
        s.effective_scope(),
        WriteScope::None,
        "no flag and no ask is still read-only"
    );
    s.client_supports_elicitation
        .store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        s.effective_scope(),
        WriteScope::All,
        "but a client that can be asked gets parity with the TUI"
    );
}

/// An explicitly narrowed grant is NOT widened by elicitation.
///
/// `--allow-writes=dlq_delete` is an operator saying "only this".
/// Opening it to every verb because the client supports a dialog
/// would override a restriction they typed deliberately — the one
/// direction this design must never move on its own.
#[tokio::test]
async fn elicitation_does_not_widen_a_narrowed_grant() {
    let narrow = WriteScope::Only(vec!["dlq_delete".into()]);
    let s = Server::with_scope(true, false, narrow.clone());
    s.client_supports_elicitation
        .store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        s.effective_scope(),
        narrow,
        "the operator said only dlq_delete, and a dialog capability is not \
             their permission to widen it"
    );
    assert!(!s.effective_scope().allows("terminate"));
}

/// The advertised surface follows the connection, not argv.
///
/// `tools/list` is where an agent learns what it may do. If the
/// scope opened but the table did not, the write tools would exist
/// and be invisible — and an agent that cannot see a tool will
/// tell the operator ebman does not support it.
#[tokio::test]
async fn the_advertised_tools_follow_the_connection() {
    let s = demo_server(); // no flag
    let names = |s: &Server| -> Vec<String> {
        tool_table(&s.effective_scope(), true)
            .as_array()
            .expect("array")
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect()
    };
    let before = names(&s);
    assert!(
        !before.iter().any(|n| n == "confirm_action"),
        "read-only: {before:?}"
    );
    s.client_supports_elicitation
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let after = names(&s);
    assert!(
        after.iter().any(|n| n == "confirm_action"),
        "an ask-capable client must be able to SEE the write surface, not \
             just be permitted it: {after:?}"
    );
    assert!(
        after.len() > before.len(),
        "and the read tools must not have been swapped out for them"
    );
}

/// Doctor reports the surface this connection actually has.
///
/// It is the tool an agent runs when something is missing, so a
/// doctor still reading argv would say "read-only" on a connection
/// that had just been granted everything.
#[tokio::test]
async fn doctor_reports_the_effective_surface() {
    let s = demo_server();
    // Through the tool interface, not the private method: that is
    // how an agent reaches it, and it pins the wiring too.
    async fn doctor(s: &Server) -> String {
        call(s, "doctor", json!({})).await.1.to_string()
    }
    assert!(doctor(&s).await.contains("read-only"));
    s.client_supports_elicitation
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let d = doctor(&s).await;
    assert!(
        !d.contains("read-only"),
        "doctor must not report a restriction this connection does not have: {d}"
    );
    assert!(d.contains("every verb"), "{d}");
}

/// The whole feature, end to end: no flag, no restart, one human
/// answer, and the write goes.
///
/// Each piece is tested above in isolation, and every one of them
/// can pass while the path as a whole does not — which is the
/// failure this cycle kept producing. So this drives the actual
/// tools an agent calls, on a server started with no write flag.
#[tokio::test]
async fn no_flag_one_answer_and_the_write_dispatches() {
    let s = std::sync::Arc::new(demo_server());
    s.client_supports_elicitation
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    if let Ok(mut slot) = s.outbound.lock() {
        *slot = Some(tx);
    }
    let srv = std::sync::Arc::clone(&s);
    tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            let f: Value = serde_json::from_str(&line).expect("json");
            let reply = json!({"jsonrpc":"2.0","id":f["id"].clone(),
                                   "result":{"action":"accept"}});
            assert!(srv.take_ask_reply(&reply));
        }
    });

    let env = demo_fixture::envs()[0].name.clone();
    let (err, plan) = call(&s, "restart", json!({"env": env})).await;
    assert!(
        !err,
        "a write tool must be callable with no flag when the operator can be \
             asked — this is the restart-your-client problem the design exists to \
             remove: {plan}"
    );
    let token = plan["confirm_token"].as_str().expect("a token").to_string();
    let (err, out) = call(&s, "confirm_action", json!({"confirm_token": token})).await;
    assert!(!err, "and it must dispatch once approved: {out}");
}

/// The ask must lose to nothing, and beat its own container.
///
/// Found in self-review: both were `ASK_TIMEOUT_SECS`, so the outer
/// call bound — whose clock starts earlier — would fire first, and
/// the deny plus its `rule=not_approved` audit line would be lost
/// with the dropped future. The suite was green: every ask test
/// either answers or waits past both.
///
/// The constants are compared at compile time above. What this adds
/// is the WIRING: that `call_timeout_secs` actually hands
/// `confirm_action` the larger budget on an ask-capable connection.
/// A margin between two constants is worth nothing if the call that
/// carries the ask is still bounded at 30 seconds.
#[test]
fn the_ask_resolves_inside_the_call_that_carries_it() {
    assert!(
        ASK_WAIT_SECS < call_timeout_secs(writes::CONFIRM_TOOL, true),
        "an ask given its container's whole budget never returns its own \
             answer — the outer timeout wins and nothing is audited"
    );
}

/// Two messages, ONE confirmation.
///
/// The constraint this feature exists to satisfy, in the
/// maintainer's words: *"if I ask to delete certain messages 1
/// confirmation is fine, more than that and it's easier to do it
/// myself"*. Every write asks, so without batching a ten-message
/// clean-up is ten dialogs and the tool is worse than the console.
///
/// Asserting the ask COUNT is the point. Everything else here
/// could pass while the server quietly asked once per message.
#[tokio::test]
async fn a_batch_of_two_asks_once_and_reports_per_message() {
    let (s, asked) = demo_answering("accept");
    let ids: Vec<String> = crate::demo_fixture::dlq_messages_for_env("poly-batch")
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(ids.len(), 2, "the fixture must hold a real batch");

    let (err, plan) = call(
        &s,
        "dlq_delete",
        json!({"env": "poly-batch", "message_ids": ids.clone()}),
    )
    .await;
    assert!(!err, "a batch plan must be accepted: {plan}");
    let token = plan["confirm_token"].as_str().expect("token").to_string();
    let plan_text = plan.to_string();
    for id in &ids {
        assert!(
            plan_text.contains(id.as_str()),
            "the plan must name every message it covers: {plan_text}"
        );
    }

    let (err, out) = call(&s, "confirm_action", json!({"confirm_token": token})).await;
    assert!(!err, "the batch must dispatch: {out}");

    let asked = asked.lock().expect("lock");
    assert_eq!(
        asked.len(),
        1,
        "ONE dialog for the whole batch — asking per message is the failure \
             this feature exists to remove: {asked:?}"
    );
    for id in &ids {
        assert!(
            asked[0].contains(id.as_str()),
            "and that one dialog must enumerate what it covers, or the operator \
                 approves a set they were not shown: {:?}",
            asked[0]
        );
    }

    let text = out.to_string();
    assert!(
        text.contains("\"succeeded\":2"),
        "the result must account for both: {text}"
    );
    assert!(
        text.contains("\"failed\":0"),
        "including the failures it did NOT have — an absent count reads as \
             unknown: {text}"
    );
}

/// A batch plan over the cap is refused before anyone is asked.
#[tokio::test]
async fn an_oversized_batch_never_reaches_the_operator() {
    let (s, asked) = demo_answering("accept");
    let too_many: Vec<String> = (0..=super::writes::DLQ_BATCH_CAP)
        .map(|i| format!("id-{i}"))
        .collect();
    let (err, out) = call(
        &s,
        "dlq_delete",
        json!({"env": "poly-batch", "message_ids": too_many}),
    )
    .await;
    assert!(err, "over the cap must refuse: {out}");
    assert_eq!(
        asked.lock().expect("lock").len(),
        0,
        "and must refuse at PLAN time — an unreadable list must never reach a \
             dialog, because the cap exists to keep the dialog readable"
    );
}

/// A dead ask channel DENIES; it does not fall through.
///
/// Found by a release panel and confirmed: `NotAsked` meant three
/// different things — no capability, no channel, failed send — and
/// only the first has a flag to fall back to. On a connection
/// widened purely by elicitation the ask IS the gate, so a client
/// that crashed mid-confirm dispatched an unapproved terminate.
#[tokio::test]
async fn a_dead_ask_channel_denies_rather_than_falling_through() {
    let s = Server::with_scope(true, false, WriteScope::None);
    s.client_supports_elicitation
        .store(true, std::sync::atomic::Ordering::Relaxed);
    // Capability declared, but no channel — the shutdown path
    // clears it, and a confirm already in flight sees this.
    assert_eq!(
        s.ask_operator("terminate poly-prod", None).await,
        AskOutcome::Undeliverable,
        "a client that can be asked but cannot be reached must DENY — \
             `NotAsked` would fall back to a flag that was never given"
    );

    // A closed receiver is the same hazard by another route.
    let (tx, rx) = tokio::sync::mpsc::channel::<String>(1);
    drop(rx);
    if let Ok(mut slot) = s.outbound.lock() {
        *slot = Some(tx);
    }
    assert_eq!(
        s.ask_operator("terminate poly-prod", None).await,
        AskOutcome::Undeliverable,
        "a send that cannot be delivered is an unanswered ask, not an absent one"
    );

    // And the one case that legitimately falls back is untouched.
    let flagged = Server::with_scope(true, false, WriteScope::All);
    assert_eq!(
        flagged.ask_operator("x", None).await,
        AskOutcome::NotAsked,
        "no capability is still NotAsked, or every flag-granted client is denied"
    );
}

/// The whole point, end to end: no flag, no channel, no dispatch.
#[tokio::test]
async fn a_write_that_only_the_ask_gated_cannot_dispatch_unasked() {
    let s = demo_server(); // no --allow-writes
    s.client_supports_elicitation
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let env = demo_fixture::envs()[0].name.clone();
    let (err, plan) = call(&s, "restart", json!({"env": env})).await;
    assert!(!err, "planning is open: {plan}");
    let token = plan["confirm_token"].as_str().expect("token").to_string();

    // No outbound channel was ever installed — the client is gone.
    let (err, out) = call(&s, "confirm_action", json!({"confirm_token": token})).await;
    assert!(
        err,
        "with no flag and no reachable operator, nothing may dispatch: {out}"
    );
}

/// `--read-only` outranks the parity default.
#[tokio::test]
async fn read_only_outranks_the_elicitation_default() {
    let s = Server::with_scope(true, false, WriteScope::None);
    s.mcp_read_only
        .store(true, std::sync::atomic::Ordering::Relaxed);
    s.client_supports_elicitation
        .store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        s.effective_scope(),
        WriteScope::None,
        "the operator said no to writes on this server; a client that supports \
             dialogs is not their permission to change that"
    );
    assert!(
        !tool_table(&s.effective_scope(), true)
            .as_array()
            .expect("array")
            .iter()
            .any(|t| t["name"] == "confirm_action"),
        "and the write surface must not be advertised"
    );
}

/// The flag parses, refuses contradictions, and refuses repeats.
#[test]
fn read_only_is_parsed_and_cannot_contradict_a_grant() {
    let args = |v: &[&str]| -> Vec<String> {
        std::iter::once("mcp".to_string())
            .chain(std::iter::once("serve".to_string()))
            .chain(v.iter().map(|s| (*s).to_string()))
            .collect()
    };
    let ok = parse_mcp_args(&args(&["--read-only"])).expect("valid");
    assert_eq!(ok.write_scope, WriteScope::None);

    let err = parse_mcp_args(&args(&["--read-only", "--allow-writes"]))
        .expect_err("contradiction must be refused");
    assert!(
        err.contains("contradict"),
        "picking a winner silently hands the operator a posture they did not \
             choose, either way: {err}"
    );
    assert!(parse_mcp_args(&args(&["--allow-writes", "--read-only"])).is_err());
    assert!(parse_mcp_args(&args(&["--read-only", "--read-only"])).is_err());
}

/// A silence is not a refusal, and must not be answered as one.
///
/// Both outcomes ended with the decline guard verbatim, which for
/// a timeout names the operator as the only way forward — the very
/// party who was not there. A peer session hit this live and had
/// to invent the redirect itself.
#[test]
fn a_timeout_and_a_decline_tell_the_agent_different_things() {
    let declined = AskOutcome::Declined.guidance();
    let silent = AskOutcome::Unanswered.guidance();
    assert_ne!(declined, silent, "the two must not share wording");

    assert!(
        declined.contains("unless the operator asks for it"),
        "a decline keeps the flat prohibition with one named exception — a \
             rationale-shaped guard does not catch an agent under \
             task-completion pressure, which is a different pull from \
             permission-seeking: {declined}"
    );

    assert!(
        silent.contains("NOT a refusal"),
        "a silence must not be reported as a refusal — nobody refused: {silent}"
    );
    assert!(
        silent.contains("Tell them"),
        "and must name a legitimate next act, or the agent's only options are \
             invent one or go quiet: {silent}"
    );
    assert!(
        !silent.contains("unless the operator asks for it"),
        "naming the absent party as the only unblock leaves no move at all: {silent}"
    );
    // The retry permission must be STATED, not implied by an
    // adverb. "Do not QUIETLY re-plan" left an agent inferring
    // that a non-quiet re-plan was allowed — correct, and an
    // inference rather than an instruction.
    assert!(
        silent.contains("plan it fresh"),
        "a legitimate retry must be granted outright, or the agent reasons its \
             way to one from an adverb: {silent}"
    );
    assert!(
        !silent.contains("quietly"),
        "the adverb is what made the permission implicit: {silent}"
    );

    // The two that never reach an agent say nothing.
    assert!(AskOutcome::Approved.guidance().is_empty());
    assert!(AskOutcome::NotAsked.guidance().is_empty());
}

/// An agent can tell a standing grant from a human gate.
///
/// `doctor` reported `elicitation: true` and `writes: every verb`
/// as adjacent facts, and a peer session confirmed it could not
/// relate them. The two imply opposite things to tell a user: a
/// flag means "I can do this"; elicitation means "I can propose
/// this, and someone must approve it". The same string for both
/// over-promises in one of them.
#[tokio::test]
async fn doctor_says_why_writes_are_available_not_just_how_wide() {
    async fn doctor(s: &Server) -> String {
        call(s, "doctor", json!({})).await.1.to_string()
    }

    // Granted by the flag: a standing grant.
    let flagged = Server::with_scope(true, false, WriteScope::All);
    let d = doctor(&flagged).await;
    assert!(d.contains("--allow-writes"), "{d}");
    assert!(
        !d.contains("client-elicitation"),
        "a flag-granted surface must not claim every write is put to a person — \
             on a client that cannot be asked, nobody is: {d}"
    );

    // Granted by the parity default: every write is asked.
    let asked = demo_server();
    asked
        .client_supports_elicitation
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let d = doctor(&asked).await;
    assert!(
        d.contains("client-elicitation") && d.contains("may decline"),
        "an elicitation-gated surface must say so, or the agent tells its user \
             it can act when it can only propose: {d}"
    );

    // BOTH: a flag grants the scope AND the client can be asked.
    //
    // This assertion previously read "an explicit flag outranks
    // the default even when both are true" and pinned the wrong
    // behaviour — a guard asserting the defect, which is the worst
    // shape available. The ask fires on capability alone, so this
    // connection DOES put every write to a person; reporting only
    // the flag told the agent it held a standing grant, and
    // `docs/headless.md` tells it that means "I can act". The plan
    // on the same connection said a person may decline. One
    // connection, two answers, and this was the false one.
    let both = Server::with_scope(true, false, WriteScope::All);
    both.client_supports_elicitation
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let d = doctor(&both).await;
    assert!(
        d.contains("--allow-writes"),
        "the flag is still where the scope came from: {d}"
    );
    assert!(
        d.contains("still put to them") && d.contains("may decline"),
        "and the ask fires on capability regardless of the flag, so doctor must \
             not report this as a bare standing grant: {d}"
    );

    // Read-only says neither.
    let ro = demo_server();
    let d = doctor(&ro).await;
    assert!(d.contains("no writes are available"), "{d}");
}

/// The plan tells the agent a person is asked.
///
/// `next` read as a mechanical step. An agent that never timed out
/// would model confirm_action as a formality and report a decline
/// to its user as an error rather than as someone's decision.
#[tokio::test]
async fn the_plan_says_a_person_will_be_asked() {
    let s = demo_server();
    s.client_supports_elicitation
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let env = demo_fixture::envs()[0].name.clone();
    let (_, plan) = call(&s, "restart", json!({"env": &env})).await;
    let next = plan["next"].as_str().expect("a next hint");
    assert!(
        next.contains("OPERATOR") && next.contains("decline"),
        "the plan must say the confirm is a request put to a person: {next}"
    );

    // And must NOT say it where nobody will be asked.
    let flagged = Server::with_scope(true, false, WriteScope::All);
    let (_, plan) = call(&flagged, "restart", json!({"env": env})).await;
    let next = plan["next"].as_str().expect("a next hint");
    assert!(
        !next.contains("OPERATOR"),
        "on a client that cannot be asked, promising an operator dialog is a \
             lie the agent would relay to its user: {next}"
    );
}

/// A timed-out ask withdraws its dialog.
///
/// Observed during release QA: the server gave up after the ask
/// window and the operator was left looking at a live-seeming
/// approval prompt for an action that could no longer happen.
/// Pressing it was inert — `forget_ask` had already removed the
/// entry — which is safe and also the problem: a prompt that does
/// nothing teaches that prompts may do nothing.
#[tokio::test(start_paused = true)]
async fn a_timed_out_ask_tells_the_client_to_withdraw_it() {
    let s = std::sync::Arc::new(Server::with_scope(true, false, WriteScope::None));
    s.client_supports_elicitation
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    if let Ok(mut slot) = s.outbound.lock() {
        *slot = Some(tx);
    }

    let srv = std::sync::Arc::clone(&s);
    let asking = tokio::spawn(async move { srv.ask_operator("terminate poly-prod", None).await });

    let ask: Value = serde_json::from_str(&rx.recv().await.expect("the ask")).expect("json");
    let asked_id = ask["id"].clone();
    assert_eq!(ask["method"], json!("elicitation/create"));

    tokio::time::advance(std::time::Duration::from_secs(ASK_TIMEOUT_SECS + 1)).await;
    assert_eq!(asking.await.expect("join"), AskOutcome::Unanswered);

    let cancel: Value =
        serde_json::from_str(&rx.recv().await.expect("a cancellation")).expect("json");
    assert_eq!(
        cancel["method"],
        json!("notifications/cancelled"),
        "the client must be told to take the dialog down: {cancel}"
    );
    assert_eq!(
        cancel["params"]["requestId"], asked_id,
        "and it must name the request it withdraws, or it cancels someone else's \
             dialog: {cancel}"
    );
    assert!(
        cancel.get("id").is_none(),
        "a notification carries no id — an id makes it a request the client must \
             answer: {cancel}"
    );
}

/// A response is never answered.
///
/// A frame with an id and no method is a RESPONSE. If the ask it
/// belongs to has already timed out, `take_ask_reply` will not
/// claim it, and the dispatch below would reply `-32601` — telling
/// the client that its well-formed reply named a method that does
/// not exist. JSON-RPC says a response is not answered at all.
#[test]
fn a_reply_to_a_forgotten_ask_is_dropped_not_answered() {
    let src = crate::app::tests::scan::production_source("cli/mcp/mod.rs");
    let src = src.as_str();
    let body = src;
    let claim = body
        .find("if server.take_ask_reply(&req)")
        .expect("the loop routes ask replies");
    // Anchored on the first line only: `cargo fmt` split this
    // condition across three lines the moment it grew a clause,
    // and an anchor written against the pre-fmt shape silently
    // stops matching.
    let drop = body
        .find("if req.get(\"method\").is_none()")
        .expect("the loop must drop unclaimed responses");
    let dispatch = body
        .find("server.handle_request(&req).await")
        .expect("the loop dispatches requests");
    assert!(
        claim < drop && drop < dispatch,
        "the order must be claim, then drop, then dispatch: claiming after \
             dropping loses every real answer, and dispatching before dropping \
             answers a response"
    );
    // And the drop is narrowed to an ACTUAL response. A frame with
    // an id, no method and neither result nor error is a malformed
    // request, which JSON-RPC answers -32600 — it must fall
    // through rather than vanish.
    assert!(
        body[drop..dispatch].contains("result") && body[drop..dispatch].contains("error"),
        "the drop must require result or error, or it silently swallows a \
             malformed request as well as a response"
    );
}

/// The agent is told to say the surface widened without a flag.
///
/// ebman has no channel to the operator except a dialog. On a bare
/// registration the write surface arrives when the client
/// reconnects against a 0.42 binary — an action operators take for
/// unrelated reasons — so the only way they hear about it is the
/// agent saying so.
#[test]
fn an_ask_opened_surface_tells_the_agent_to_warn_the_operator() {
    let opened = WriteScope::All.agent_summary(None, true, true);
    assert!(
        opened.contains("YOUR CLIENT") && opened.contains("--read-only"),
        "the agent must be told to explain WHY writes exist and name the way \
             back: {opened}"
    );
    assert!(
        opened.contains("before you plan a write"),
        "and to say it before acting, not after: {opened}"
    );
    // The BREADTH, not just the fact. An agent on a live
    // production fleet observed that the reconnect granted
    // `terminate` alongside the `dlq_delete` someone actually
    // wanted — correct by design, and worth saying out loud
    // rather than letting it arrive quietly with the narrow thing.
    assert!(
        opened.contains("terminate") && opened.contains("--allow-writes=verb,verb"),
        "it must name what else came with it, and the way to narrow it: {opened}"
    );
    // And it must actively counteract over-caution. An agent told
    // "the operator may not know you can write" can easily read
    // that as "so do not". The note says the opposite outright —
    // asserting the absence of the phrase, as a first version of
    // this test did, checked nothing and failed on the note's own
    // wording.
    assert!(
        opened.contains("not a reason to avoid")
            || opened.contains("Do not treat this as a reason to avoid"),
        "the note must say plainly that this is not a reason to stop proposing \
             work, or it reads as a discouragement: {opened}"
    );

    // A flag-granted surface says nothing: the operator typed the
    // flag, so there is nothing they did not know.
    let flagged = WriteScope::All.agent_summary(None, true, false);
    assert!(
        !flagged.contains("YOUR CLIENT"),
        "an operator who passed --allow-writes already knows: {flagged}"
    );
    // And a standing refusal still outranks both.
    let refused = WriteScope::All.agent_summary(Some("read_only is set."), true, true);
    assert!(!refused.contains("YOUR CLIENT"), "{refused}");
}

/// An approval leaves a record that a question was put.
///
/// Before this, a dispatch carried `can_ask=true` — an ask was
/// POSSIBLE — and nothing said one happened or what answered it.
/// A client that declares elicitation and answers its own dialogs
/// produced a cleaner log than an operator at a keyboard, which
/// `docs/design/protection-levels.md` names as precisely the wrong
/// incentive.
#[test]
fn the_ask_audit_vocabulary_is_stable_and_excludes_the_unasked() {
    assert_eq!(AskOutcome::Approved.answer_label(), "approved");
    assert_eq!(AskOutcome::Declined.answer_label(), "declined");
    assert_eq!(AskOutcome::Unanswered.answer_label(), "unanswered");

    // Distinct tokens, or a log reader cannot filter on them.
    let all = [
        AskOutcome::Approved.answer_label(),
        AskOutcome::Declined.answer_label(),
        AskOutcome::Unanswered.answer_label(),
    ];
    assert_eq!(
        all.iter().collect::<std::collections::HashSet<_>>().len(),
        3,
        "every outcome must be distinguishable in the log"
    );

    // And the vocabulary is separate from the agent-facing prose,
    // so rewording one cannot silently reshape the other.
    for o in [
        AskOutcome::Approved,
        AskOutcome::Declined,
        AskOutcome::Unanswered,
    ] {
        assert!(
            !o.answer_label().contains(' '),
            "a log token must not contain spaces — `escape_value` does not quote \
                 them, so a spaced token can forge a field: {:?}",
            o.answer_label()
        );
    }
}

/// The unasked case must not produce a line claiming a question.
#[test]
fn a_connection_that_was_never_asked_writes_no_ask_line() {
    let src = crate::app::tests::scan::production_source("cli/mcp/writes.rs");
    let src = src.as_str();
    let body = src;
    let call = body
        .find("append_action_asked")
        .expect("the confirm path must audit the ask");
    let guard = body[..call]
        .rfind("if was_actually_asked")
        .expect("and must exclude the case where no question was put");
    assert!(
        call - guard < 400,
        "the guard must be the condition on THIS call — a line saying a question \
             was asked when none was is the false record this stage exists to prevent"
    );
    // And it must exclude EVERY outcome where nothing was put to
    // anyone, not just the first one anybody thought of. A
    // mutation dropping `Undeliverable` from the set passed until
    // this existed: the guard checked that an exclusion was
    // present, never which cases it covered.
    // From the BINDING, not the `if` — the variant list lives in
    // the `let`, and a window starting at the condition misses it
    // entirely.
    let binding = body[..call]
        .rfind("let was_actually_asked")
        .expect("the exclusion must be a named binding");
    let cond = &body[binding..call];
    for never_asked in ["AskOutcome::NotAsked", "AskOutcome::Undeliverable"] {
        assert!(
            cond.contains(never_asked),
            "{never_asked} means no question reached anybody, so it must not \
                 produce a `stage=asked` line: {cond}"
        );
    }
}

/// Terminate and purge demand a TYPED name; nothing else does.
///
/// The TUI makes a human type the environment name for both. Over
/// MCP `confirm_name` is supplied by the agent, so the human's
/// whole contribution to destroying an environment was one click.
#[tokio::test(start_paused = true)]
async fn terminate_and_purge_require_the_operator_to_type_the_name() {
    async fn ask_with(reply_content: Option<Value>, expect: Option<&str>) -> AskOutcome {
        let s = std::sync::Arc::new(Server::with_scope(true, false, WriteScope::All));
        s.client_supports_elicitation
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(4);
        if let Ok(mut slot) = s.outbound.lock() {
            *slot = Some(tx);
        }
        let want = expect.map(str::to_string);
        let srv = std::sync::Arc::clone(&s);
        let asking = tokio::spawn(async move {
            srv.ask_operator("terminate poly-prod", want.as_deref())
                .await
        });
        let frame: Value = serde_json::from_str(&rx.recv().await.expect("ask")).expect("json");

        // The schema must actually demand a field when one is required.
        let required = frame["params"]["requestedSchema"]["required"].clone();
        if expect.is_some() {
            assert_eq!(
                required,
                json!(["confirm"]),
                "a typed confirm must be a REQUIRED property, or a client may \
                     render nothing and the operator types nothing: {frame}"
            );
        } else {
            assert!(
                required.is_null(),
                "ordinary confirms demand nothing: {frame}"
            );
        }

        let mut result = json!({"action": "accept"});
        if let Some(c) = reply_content {
            result["content"] = c;
        }
        let reply = json!({"jsonrpc": "2.0", "id": frame["id"].clone(), "result": result});
        assert!(s.take_ask_reply(&reply));
        asking.await.expect("join")
    }

    // The right name, typed: approved.
    assert_eq!(
        ask_with(Some(json!({"confirm": "poly-prod"})), Some("poly-prod")).await,
        AskOutcome::Approved
    );
    // The wrong name: NOT a decline — nobody refused.
    assert_eq!(
        ask_with(Some(json!({"confirm": "poly-prd"})), Some("poly-prod")).await,
        AskOutcome::Unconfirmed,
        "a mistyped name is a failed confirmation, and logging it as a decline \
             records a refusal that did not happen"
    );
    // No content at all — what a client that cannot render an
    // input field returns. Must fail CLOSED.
    assert_eq!(
        ask_with(None, Some("poly-prod")).await,
        AskOutcome::Unconfirmed,
        "a client that cannot show a text field must not be able to one-click a \
             terminate"
    );
    // Case and whitespace are not close enough.
    for near in ["Poly-Prod", " poly-prod", "poly-prod "] {
        assert_eq!(
            ask_with(Some(json!({"confirm": near})), Some("poly-prod")).await,
            AskOutcome::Unconfirmed,
            "{near:?} must not pass — the only thing this step buys is that \
                 somebody read the name and reproduced it"
        );
    }
    // And a verb that demands nothing still works on a bare accept.
    assert_eq!(ask_with(None, None).await, AskOutcome::Approved);
}

/// Only the two destructive-and-irreversible verbs demand it.
#[test]
fn the_typed_confirm_is_scoped_to_terminate_and_purge() {
    let src = crate::app::tests::scan::production_source("cli/mcp/writes.rs");
    let src = src.as_str();
    let body = src;
    let arm = body
        .find("WriteVerb::Terminate | WriteVerb::DlqPurge => Some(pending.env.as_str())")
        .expect("terminate and purge demand a typed name");
    let rest = &body[arm..arm + 200];
    assert!(
        rest.contains("_ => None"),
        "every other verb must demand nothing — a typed confirm on a restart is \
             friction that teaches operators to type past the ones that matter"
    );
}

/// doctor states the honest limit: a capability is not a human.
///
/// The design note is explicit — "never describe elicitation as
/// 'a human confirmed'" — because the capability is a self-report
/// in the client's `initialize` frame, and a framework that
/// declares it and answers its own dialogs satisfies every ask.
/// ebman cannot tell the difference at the time. An agent that
/// relays "a person approved this" asserts something neither it
/// nor ebman can check.
#[tokio::test]
async fn doctor_says_a_declared_capability_is_not_proof_of_a_human() {
    async fn doctor(s: &Server) -> String {
        call(s, "doctor", json!({})).await.1.to_string()
    }
    let s = demo_server();
    s.client_supports_elicitation
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let d = doctor(&s).await;
    // PARSED, not substring-matched. This note was pushed through
    // `json_string` and then re-encoded by the notes renderer, so
    // its array element carried literal quote characters inside
    // the string — valid JSON, garbled content. A `contains()` on
    // the raw payload matched anyway and the test passed.
    let body: Value = serde_json::from_str(&d).unwrap_or_else(|e| panic!("{d}: {e}"));
    let note = body["notes"]
        .as_array()
        .and_then(|n| n.iter().find_map(Value::as_str))
        .unwrap_or_else(|| panic!("no notes in {body}"));
    assert!(
        !note.starts_with('"') && !note.ends_with('"'),
        "a note must not carry its own quotes — that is a double encode: {note:?}"
    );
    assert!(
        d.contains("not proof a person saw"),
        "the limit must be stated where an agent reads it: {d}"
    );
    assert!(
        d.contains("not that a human approved it"),
        "and must name the specific over-claim to avoid, not just gesture at \
             uncertainty: {d}"
    );

    // A client that never declared it gets no such note — there
    // is no ask to be sceptical about.
    let quiet = demo_server();
    assert!(
        !doctor(&quiet).await.contains("not proof a person saw"),
        "a connection with no elicitation has no ask to qualify"
    );
}

/// Nothing claims a person declined.
///
/// Measured 2026-09-20 against headless `claude -p`: it declares
/// `elicitation: true`, gets the full write surface, and
/// auto-declines a confirmation in under a second with no human in
/// the session. The text said "declined by the operator", the
/// agent relayed "the operator simply said no", and nobody had.
#[test]
fn a_decline_does_not_assert_that_a_person_made_it() {
    let reason = AskOutcome::Declined.reason();
    assert!(
        !reason.contains("operator") && !reason.contains("human"),
        "ebman cannot see who answered — asserting a person did is a false \
             record the agent repeats to its user: {reason}"
    );
    assert!(
        reason.contains("declined"),
        "while still saying plainly what happened: {reason}"
    );

    let guidance = AskOutcome::Declined.guidance();
    assert!(
        guidance.contains("do not re-plan the same action unless the operator"),
        "the flat prohibition must survive the hedging — it is what stops a \
             retry loop: {guidance}"
    );
    assert!(
        guidance.contains("do NOT tell your user a person refused"),
        "and the agent must be told not to attribute it: {guidance}"
    );
    assert!(
        guidance.contains("-p") || guidance.contains("CI harness"),
        "naming the concrete case beats gesturing at uncertainty — an agent that \
             knows a `-p` run declines by itself can say something useful: {guidance}"
    );
}
