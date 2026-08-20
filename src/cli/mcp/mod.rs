//! `ebman mcp serve [--demo] [--no-redact]` — stdio MCP server
//! exposing ebman's read seams as tools, so Claude Code (or any MCP
//! client) can query fleet state first-class. Spec: BACKLOG.md "0.26
//! candidates". Registration: `claude mcp add ebman -- ebman mcp serve`.
//!
//! Two hard rules the implementation stands on:
//! - **stdout is protocol-only; stderr is diagnostics-only.** Tools
//!   call the underlying seams (`run_rules`, `parse_audit_line`,
//!   `aws::*`), never the `println!`-ing CLI `run()` wrappers.
//! - **Concurrent `tools/call`, responsive loop.** Every tool call is
//!   spawned and bounded at [`TOOL_TIMEOUT_SECS`]; `ping` never waits
//!   behind a slow AWS fan-out. `notifications/cancelled` is ignored
//!   in v1 (documented limitation).
//!
//! `--demo` serves the synthetic `demo_fixture` fleet through the
//! same tool layer (the demo AwsClient is a fail-loudly stub, so demo
//! data enters above the client) — this is the zero-AWS e2e harness.
//! `--no-redact` disables the `get_option_settings` env-var redaction.
//!
//! v1 is reads-only. Writes (`--allow-writes`) are a spec'd v2 with
//! their own safety review; nothing here dispatches.

use std::sync::Arc;

use color_eyre::eyre::Result;
use serde_json::{json, Value};

use crate::cli::lint::{
    fetch_env_lint_inputs, fetch_stale_platform_issues, run_rules_for_env, EnvLintInputs,
};
use crate::{audit as audit_log, aws, cost_cache, demo_fixture, lint, terraform, util};

/// The MCP protocol revision this server claims. Clients offering a
/// different revision get this one back (echo-negotiate); the golden
/// frame test pins it so a bump is a conscious act.
mod tools;
mod writes;
use tools::*;

pub(crate) const PROTOCOL_VERSION: &str = "2025-06-18";

/// Hard wall-clock bound on a single tool call so a hung AWS call
/// can't wedge the agent turn.
const TOOL_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, PartialEq, Eq)]
struct McpArgs {
    demo: bool,
    no_redact: bool,
    allow_writes: bool,
}

const MCP_USAGE: &str = "usage: ebman mcp serve [--demo] [--no-redact] [--allow-writes]";

fn parse_mcp_args(args: &[String]) -> Result<McpArgs, String> {
    // args[0] = "mcp"; the only sub-verb is "serve".
    if args.get(1).map(String::as_str) != Some("serve") {
        return Err(MCP_USAGE.into());
    }
    let mut demo = false;
    let mut no_redact = false;
    let mut allow_writes = false;
    for arg in args.iter().skip(2) {
        match arg.as_str() {
            "--demo" => demo = true,
            "--no-redact" => no_redact = true,
            // Flag-only opt-in by spec: write capability must be
            // visible in the process table / .mcp.json, never hidden
            // in a config file.
            "--allow-writes" => allow_writes = true,
            other => return Err(format!("ebman mcp: unknown flag '{other}' — {MCP_USAGE}")),
        }
    }
    Ok(McpArgs {
        demo,
        no_redact,
        allow_writes,
    })
}

/// Non-object frames (batch arrays, bare scalars) are invalid
/// requests per JSON-RPC 2.0 — answer -32600 rather than silently
/// dropping them (a strict client would wait out its timeout). MCP
/// 2025-06-18 removed batching, so arrays are not-supported by spec.
/// Returns the response frame to send, `None` for a valid object.
fn invalid_request_response(req: &Value) -> Option<String> {
    if req.is_object() {
        return None;
    }
    Some(
        json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": {"code": -32600, "message": "invalid request: expected a single JSON-RPC object"}
        })
        .to_string(),
    )
}

pub(crate) use crate::util::redact_option_value;

enum Backend {
    Aws,
    Demo,
}

pub(crate) struct Server {
    backend: Backend,
    redact: bool,
    allow_writes: bool,
    /// Safety config loaded once at startup (pins). Demo servers get
    /// the default (hermetic).
    safety_cfg: crate::config::Config,
    /// Two-phase write state: the single pending-plan slot (spec:
    /// writes are serialized server-wide).
    writes: tokio::sync::Mutex<writes::WriteState>,
    /// Whether a write dispatch is in flight — separate AtomicBool so
    /// the confirm path's RAII guard can reset it on an unwind
    /// (pre-tag review I2).
    dispatching: std::sync::atomic::AtomicBool,
    /// `clientInfo.name` from initialize — lands in audit extras so
    /// agent-dispatched writes are attributable.
    client_name: std::sync::Mutex<String>,
}

impl Server {
    pub(crate) fn new(demo: bool, no_redact: bool, allow_writes: bool) -> Self {
        let safety_cfg = if demo {
            crate::config::Config::default()
        } else {
            crate::config::load()
        };
        Self::with_config(demo, no_redact, allow_writes, safety_cfg)
    }

    /// Test seam: inject the safety config (pin tests must not read
    /// the operator's real config.toml).
    pub(crate) fn with_config(
        demo: bool,
        no_redact: bool,
        allow_writes: bool,
        safety_cfg: crate::config::Config,
    ) -> Self {
        Server {
            backend: if demo { Backend::Demo } else { Backend::Aws },
            redact: !no_redact,
            allow_writes,
            safety_cfg,
            writes: tokio::sync::Mutex::new(writes::WriteState::default()),
            dispatching: std::sync::atomic::AtomicBool::new(false),
            client_name: std::sync::Mutex::new("unknown".to_string()),
        }
    }

    /// One JSON-RPC frame in, at most one out (`None` for
    /// notifications). Pure protocol layer — tool dispatch lives in
    /// [`Server::call_tool`] — so tests can drive full frames.
    pub(crate) async fn handle_request(&self, req: &Value) -> Option<Value> {
        let id = req.get("id").cloned();
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        // Frames without an id — or with `"id": null` — are
        // notifications per JSON-RPC 2.0: never answered, whatever the
        // method (an id-less tools/call must not produce an
        // `"id": null` response, and answering an explicit null id
        // collides with the -32700 parse-error convention).
        match id {
            None | Some(Value::Null) => return None,
            Some(_) => {}
        }
        match method {
            "initialize" => {
                // Capture the client name for write-audit attribution.
                if let Some(name) = req
                    .get("params")
                    .and_then(|p| p.get("clientInfo"))
                    .and_then(|c| c.get("name"))
                    .and_then(Value::as_str)
                {
                    if let Ok(mut cn) = self.client_name.lock() {
                        *cn = name.to_string();
                    }
                }
                // Echo-negotiate: accept the client's revision when it
                // matches ours, otherwise offer ours.
                let client_version = req
                    .get("params")
                    .and_then(|p| p.get("protocolVersion"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let version = if client_version == PROTOCOL_VERSION {
                    client_version
                } else {
                    PROTOCOL_VERSION
                };
                Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": version,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "ebman", "version": env!("CARGO_PKG_VERSION")}
                    }
                }))
            }
            // notifications/initialized + notifications/cancelled land
            // in the id-less early-return above, like all notifications.
            "ping" => Some(json!({"jsonrpc": "2.0", "id": id, "result": {}})),
            "tools/list" => Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {"tools": tool_table(self.allow_writes)}
            })),
            "tools/call" => {
                let params = req.get("params").cloned().unwrap_or_else(|| json!({}));
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                if !tool_table(self.allow_writes)
                    .as_array()
                    .is_some_and(|t| t.iter().any(|d| d["name"] == name.as_str()))
                {
                    return Some(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32602, "message": format!("unknown tool '{name}'")}
                    }));
                }
                let outcome = tokio::time::timeout(
                    std::time::Duration::from_secs(TOOL_TIMEOUT_SECS),
                    self.call_tool(&name, &args),
                )
                .await
                .unwrap_or_else(|_| {
                    Err(format!(
                        "tool '{name}' timed out after {TOOL_TIMEOUT_SECS}s"
                    ))
                });
                let (text, is_error) = match outcome {
                    Ok(body) => (body, false),
                    Err(msg) => (msg, true),
                };
                Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{"type": "text", "text": text}],
                        "isError": is_error
                    }
                }))
            }
            // Unknown request (id present — notifications returned
            // above): method-not-found per JSON-RPC.
            _ => Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": format!("method '{method}' not found")}
            })),
        }
    }
}

pub async fn run(args: &[String]) -> Result<()> {
    let McpArgs {
        demo,
        no_redact,
        allow_writes,
    } = match parse_mcp_args(args) {
        Ok(parsed) => parsed,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };
    // Writes fan audit lines out to the configured webhook — the
    // reads-only server stays free of the config-disk read.
    if allow_writes && !demo {
        crate::audit::init_from_config_disk();
    }
    let server = Arc::new(Server::new(demo, no_redact, allow_writes));
    // Frame-level tools/call concurrency cap (see the spawn site).
    let tool_slots = Arc::new(tokio::sync::Semaphore::new(16));

    // Single writer task: concurrent tool tasks send completed frames
    // through the channel so stdout writes can't interleave. Bounded:
    // a client that writes requests but stops reading stdout must
    // apply backpressure (senders park at `send().await`), not grow
    // an unbounded queue of completed frames.
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<String>(256);
    let writer = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let mut stdout = tokio::io::stdout();
        while let Some(line) = out_rx.recv().await {
            // A write error means the client closed its read end —
            // keep-running would execute AWS-hitting tool calls whose
            // results nobody can ever receive. Stop draining; senders
            // then error out and the tasks unwind.
            if stdout.write_all(line.as_bytes()).await.is_err()
                || stdout.write_all(b"\n").await.is_err()
                || stdout.flush().await.is_err()
            {
                eprintln!("ebman mcp: stdout closed — dropping remaining frames");
                break;
            }
        }
    });

    use futures::StreamExt;
    use tokio_util::codec::{FramedRead, LinesCodec};
    // LinesCodec with a max length: a frame streamed without a newline
    // used to accumulate in the read buffer without bound. 16MB is far
    // beyond any real MCP frame.
    const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
    let mut lines = FramedRead::new(
        tokio::io::stdin(),
        LinesCodec::new_with_max_length(MAX_LINE_BYTES),
    );
    // Read errors (e.g. one invalid-UTF-8 byte → InvalidData) must not
    // masquerade as EOF: previously the server exited 0 silently. Skip
    // the bad line loudly; bail after a run of consecutive errors so a
    // permanently-broken stream can't spin.
    let mut consecutive_read_errors: u32 = 0;
    // FramedRead sets an internal errored flag on any decode error and
    // the NEXT poll returns None — then resets, so the stream is
    // resumable. A None right after an Err is therefore NOT EOF: treat
    // it as part of the error recovery and poll again (without this,
    // one oversized/invalid line killed the whole session as a silent
    // exit 0 — verified live before the fix).
    let mut last_was_error = false;
    loop {
        let line = match lines.next().await {
            Some(Ok(line)) => {
                consecutive_read_errors = 0;
                last_was_error = false;
                line
            }
            None => {
                if last_was_error {
                    last_was_error = false;
                    continue;
                }
                break;
            }
            Some(Err(e)) => {
                consecutive_read_errors += 1;
                last_was_error = true;
                eprintln!("ebman mcp: stdin read error (skipping line): {e}");
                if consecutive_read_errors >= 5 {
                    eprintln!(
                        "ebman mcp: {consecutive_read_errors} consecutive read errors — exiting"
                    );
                    break;
                }
                continue;
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                let _ = out_tx
                    .send(
                        json!({
                            "jsonrpc": "2.0",
                            "id": null,
                            "error": {"code": -32700, "message": "parse error"}
                        })
                        .to_string(),
                    )
                    .await;
                continue;
            }
        };
        if let Some(resp) = invalid_request_response(&req) {
            let _ = out_tx.send(resp).await;
            continue;
        }
        // tools/call may hit AWS for many seconds — spawn it so the
        // loop stays responsive to ping / further calls. Everything
        // else is answered inline (cheap + ordering-sensitive).
        // The semaphore caps frame-level concurrency: a flood of
        // one-line tools/call frames must not spawn unlimited tasks
        // each building an AwsClient (in-CALL fan-out is separately
        // bounded at FETCH_CONCURRENCY).
        if req.get("method").and_then(Value::as_str) == Some("tools/call") {
            let permit = Arc::clone(&tool_slots).acquire_owned().await;
            let server = Arc::clone(&server);
            let out_tx = out_tx.clone();
            tokio::spawn(async move {
                let _permit = permit;
                if let Some(resp) = server.handle_request(&req).await {
                    let _ = out_tx.send(resp.to_string()).await;
                }
            });
        } else if let Some(resp) = server.handle_request(&req).await {
            let _ = out_tx.send(resp.to_string()).await;
        }
    }
    // stdin closed: drop the sender. The writer keeps draining until
    // in-flight tool tasks (which hold out_tx clones) finish — bounded
    // by the per-call timeout — then exits.
    drop(out_tx);
    let _ = writer.await;
    if allow_writes {
        crate::audit::drain_webhooks(std::time::Duration::from_secs(12)).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    fn demo_server() -> Server {
        Server::new(true, false, false)
    }

    fn demo_writes_server() -> Server {
        Server::new(true, false, true)
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
            let arr = tool_table(s.allow_writes);
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
        assert_eq!(plan["plan"]["action"], "Restart");
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
        let s = Server::with_config(true, false, true, cfg);
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
                "lint",
                "get_option_settings",
                "drift",
                "audit_log",
                "recent_events",
                "list_versions",
                "fleet_cost",
            ],
            "tool registry changed — update docs/headless.md's table"
        );
        // Coverage caveats are part of the contract, not prose fluff.
        let lint_desc = tools[1]["description"].as_str().unwrap();
        assert!(lint_desc.contains("EBL011") && lint_desc.contains("EBL016"));
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
        let open = Server::new(true, true, false);
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
        let rendered = terraform::render_drift_json(None, &reports);
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
        let server = Server::new(true, false, false);
        let req: Value = serde_json::from_str(
            r#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"nope"}}"#,
        )
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
        let arr: Value =
            serde_json::from_str(r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#).unwrap();
        let resp = invalid_request_response(&arr).expect("array is invalid");
        let parsed: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(parsed["error"]["code"], -32600);
        assert!(parsed["id"].is_null());
        let scalar: Value = serde_json::from_str("42").unwrap();
        assert!(invalid_request_response(&scalar).is_some());
        let obj: Value =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).unwrap();
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
}
