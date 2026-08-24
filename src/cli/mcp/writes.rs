//! MCP v2 write tools (`--allow-writes`, 0.28): `deploy`, `restart`,
//! `rebuild`, `terminate`, `set_option`, plus the `confirm_action`
//! second phase. Spec: BACKLOG.md "0.28 candidates" — decisions are
//! LOCKED there; the shape here implements them:
//!
//! - Every write is two-phase: the verb tool validates (env exists,
//!   pin, freeze, verb-specific checks) and returns a `pending` plan
//!   with a single-use 60s `confirm_token`; `confirm_action`
//!   dispatches. The plan is transcript-visible by construction.
//! - `terminate`'s phase 2 additionally requires `confirm_name` ==
//!   the env name (the MCP equivalent of the TUI's strict-typed
//!   confirm); one retry within the TTL on mismatch.
//! - Writes are serialized server-wide: one pending plan (a new plan
//!   replaces it — the agent re-planned), one in-flight dispatch.
//! - Dispatch-only semantics: no wait-for-green; the agent polls the
//!   read tools. Keeps every call inside the 30s tool bound.
//! - Audit parity with the CLI: dispatched/completed pairs tagged
//!   `via=mcp client=<clientInfo.name>`. Demo mode writes NO audit
//!   lines and fires NO webhooks — synthetic success only.
//!
//! Tokens are single-use and short-lived; they force the round-trip,
//! they are not a cryptographic boundary (the agent that plans is the
//! agent that receives the token — the audience for the plan is the
//! HUMAN reading the agent's transcript).

use super::*;

/// Two-phase write state — the single pending-plan slot, guarded by
/// the server's mutex. `dispatching` (whether a write is in flight)
/// lives on `Server` as an `AtomicBool` so the RAII reset guard can
/// clear it synchronously even on an unwind (see `tool_confirm_action`).
#[derive(Default)]
pub(super) struct WriteState {
    pub pending: Option<PendingWrite>,
    /// Tokens minted for plans this session that are no longer live,
    /// newest last.
    ///
    /// Confirming one of these used to return "unknown confirm_token",
    /// which is what a TYPO returns — so an agent that re-planned and
    /// then confirmed the older token was told its token was garbage
    /// rather than superseded, and had no way to tell the two apart.
    /// The distinction changes what the agent should do next: re-read
    /// the newer plan, versus re-send the token it already has.
    ///
    /// Bounded, because it is only a diagnostic: past the cap the
    /// oldest are dropped and confirming one of those falls back to
    /// the unknown-token message, which is honest — we genuinely no
    /// longer know.
    pub retired: std::collections::VecDeque<String>,
}

impl WriteState {
    /// Install a freshly minted plan, retiring whatever it replaces.
    ///
    /// The retirement lives here rather than at the call site because
    /// the call site needs a running server to reach — so a test could
    /// pin the message branch but not the fact that anything ever
    /// reaches `retired`. That is the same shape as an earlier cache
    /// whose read path was tested while nothing wrote to it.
    pub(super) fn install(&mut self, plan: PendingWrite) {
        if let Some(old) = self.pending.take() {
            if self.retired.len() >= RETIRED_TOKEN_MEMORY {
                self.retired.pop_front();
            }
            self.retired.push_back(old.token);
        }
        self.pending = Some(plan);
    }
}

/// What to tell an agent whose `confirm_token` doesn't match the
/// pending plan.
///
/// Split out so the distinction is testable: `confirm` needs a live
/// server, and the branch — not the plumbing — is what was wrong.
fn mismatched_token_message(retired: &std::collections::VecDeque<String>, token: &str) -> String {
    if retired.iter().any(|t| t == token) {
        // The agent re-planned and then confirmed the older token.
        // Saying "unknown" here is what a TYPO gets, and the two want
        // different next moves: re-read the newer plan, versus re-send
        // the token you already hold.
        "confirm_token superseded by a newer plan — confirm that one, or re-plan".to_string()
    } else {
        "unknown confirm_token — re-plan required".to_string()
    }
}

/// How many retired tokens to remember. An agent re-planning more than
/// a handful of times inside one 60-second TTL is not a case worth
/// spending memory on.
const RETIRED_TOKEN_MEMORY: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WriteVerb {
    Deploy,
    Restart,
    Rebuild,
    Terminate,
    SetOption,
}

impl WriteVerb {
    fn label(self) -> &'static str {
        match self {
            WriteVerb::Deploy => "Deploy",
            WriteVerb::Restart => "Restart",
            WriteVerb::Rebuild => "Rebuild",
            WriteVerb::Terminate => "Terminate",
            WriteVerb::SetOption => "SetOption",
        }
    }
}

pub(super) struct PendingWrite {
    pub token: String,
    pub verb: WriteVerb,
    pub env: String,
    pub version: Option<String>,
    pub settings: Vec<(String, String, String)>,
    pub profile: Option<String>,
    pub region: Option<String>,
    pub expires_at: tokio::time::Instant,
    /// Terminate only: one `confirm_name` mismatch keeps the token
    /// alive for a single retry; the second drops the plan.
    pub name_retry_used: bool,
}

/// Token TTL — long enough for an agent round-trip, short enough
/// that a stale plan can't be confirmed against changed reality.
const CONFIRM_TTL_SECS: u64 = 60;

/// `set_option` per-call cap (spec-locked).
const SET_OPTION_MAX: usize = 10;

/// Single-use token: uniqueness is what matters (the agent receives
/// it; see module doc), sourced from pid + monotonic counter + nanos
/// through sha256.
fn mint_token() -> String {
    use sha2::{Digest, Sha256};
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut h = Sha256::new();
    h.update(std::process::id().to_le_bytes());
    h.update(n.to_le_bytes());
    h.update(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
            .to_le_bytes(),
    );
    let digest = h.finalize();
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// Tool descriptors for the write surface — appended to tools/list
/// ONLY under `--allow-writes` (spec: the listing is honest).
pub(super) fn write_tool_descriptors() -> Vec<Value> {
    let confirm_note = "TWO-PHASE: this tool DISPATCHES NOTHING. It validates and returns {pending:true, confirm_token, plan}; you must surface the plan, then call confirm_action with the token (60s TTL, single-use) to dispatch. Dispatch-only — poll the read tools for progress.";
    vec![
        json!({
            "name": "deploy",
            "description": format!("Deploy an existing application version to an environment. {confirm_note}"),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "env": {"type": "string", "description": "Environment name (required)"},
                    "version": {"type": "string", "description": "Existing application version label (required)"},
                    "profile": {"type": "string"},
                    "region": {"type": "string"}
                },
                "required": ["env", "version"]
            }
        }),
        json!({
            "name": "restart",
            "description": format!("Restart the app server on an environment's instances. {confirm_note}"),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "env": {"type": "string", "description": "Environment name (required)"},
                    "profile": {"type": "string"},
                    "region": {"type": "string"}
                },
                "required": ["env"]
            }
        }),
        json!({
            "name": "rebuild",
            "description": format!("Rebuild an environment (replaces its resources). {confirm_note}"),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "env": {"type": "string", "description": "Environment name (required)"},
                    "profile": {"type": "string"},
                    "region": {"type": "string"}
                },
                "required": ["env"]
            }
        }),
        json!({
            "name": "terminate",
            "description": format!("TERMINATE an environment — destructive and irreversible. {confirm_note} Additionally, confirm_action requires confirm_name equal to the env name (strict-typed confirm; one retry per token)."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "env": {"type": "string", "description": "Environment name (required)"},
                    "profile": {"type": "string"},
                    "region": {"type": "string"}
                },
                "required": ["env"]
            }
        }),
        json!({
            "name": "set_option",
            "description": format!("Update up to {SET_OPTION_MAX} option settings on one environment. Namespaces must already exist in the env's configuration (no cross-env blast). {confirm_note} The plan shows old -> new per setting; old env-var values are redacted per the standing contract."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "env": {"type": "string", "description": "Environment name (required)"},
                    "settings": {
                        "type": "array",
                        "description": "Settings to apply (max 10)",
                        "items": {
                            "type": "object",
                            "properties": {
                                "namespace": {"type": "string"},
                                "name": {"type": "string"},
                                "value": {"type": "string"}
                            },
                            "required": ["namespace", "name", "value"]
                        }
                    },
                    "profile": {"type": "string"},
                    "region": {"type": "string"}
                },
                "required": ["env", "settings"]
            }
        }),
        json!({
            "name": "confirm_action",
            "description": "Phase 2 of every write tool: dispatch the pending plan identified by confirm_token (single-use, 60s TTL). terminate additionally requires confirm_name equal to the plan's env name. Writes are serialized — a confirm while another dispatch is in flight is refused.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "confirm_token": {"type": "string", "description": "Token from the write tool's pending plan (required)"},
                    "confirm_name": {"type": "string", "description": "terminate only: must equal the env name"}
                },
                "required": ["confirm_token"]
            }
        }),
    ]
}

impl Server {
    /// Phase 1 for every write verb: shared gates (writes enabled,
    /// not mid-dispatch, freeze, pins, env exists), verb-specific
    /// validation, then a pending plan + token.
    pub(super) async fn tool_write_plan(
        &self,
        verb: WriteVerb,
        args: &Value,
    ) -> Result<String, String> {
        if !self.allow_writes {
            // Unreachable via the gated table; belt-and-braces.
            return Err("writes are disabled — start the server with --allow-writes".into());
        }
        if self.dispatching.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("another write is in flight — wait for it to complete".into());
        }
        let env_name = arg_str(args, "env").ok_or("'env' is required")?;

        // Freeze + pin gate — run at plan time AND re-run at confirm,
        // because the 60s token window is long enough for an operator
        // to declare an incident between the two and the whole point of
        // the gates is to stop a write dispatching then.
        //
        // The FREEZE is what the re-run catches: it is re-read from
        // disk each time. `safety_cfg` is snapshotted at server
        // construction, so a *pin* added during a long-lived session is
        // not seen until restart. This comment used to claim it was.
        let profile = arg_str(args, "profile");
        if let Some(msg) = crate::cli::write_refusal(
            &self.safety_cfg,
            &env_name,
            &profile,
            crate::freeze::read_active(),
        ) {
            return Err(msg);
        }

        let envs = self.fetch_envs(args).await?;
        let env = envs
            .iter()
            .find(|e| e.name == env_name)
            .ok_or_else(|| format!("env '{env_name}' not found"))?
            .clone();

        let mut version: Option<String> = None;
        let mut settings: Vec<(String, String, String)> = Vec::new();
        let mut plan_extra = String::new();

        match verb {
            WriteVerb::Deploy => {
                let label = arg_str(args, "version").ok_or("'version' is required")?;
                let known = match self.backend {
                    Backend::Demo => demo_fixture::deploys_for_app(&env.application)
                        .iter()
                        .any(|v| v.label == label),
                    Backend::Aws => {
                        let client = self.client(args).await?;
                        client
                            .list_application_versions(&env.application)
                            .await
                            .map_err(|e| {
                                tool_error(&profile, "list_application_versions", &e.to_string())
                            })?
                            .iter()
                            .any(|v| v.label == label)
                    }
                };
                if !known {
                    return Err(format!(
                        "version '{label}' does not exist for application '{}'",
                        env.application
                    ));
                }
                plan_extra = format!(
                    ",\"current_version\":{},\"target_version\":{}",
                    util::json_string(&env.version_label),
                    util::json_string(&label),
                );
                version = Some(label);
            }
            WriteVerb::SetOption => {
                let raw = args
                    .get("settings")
                    .and_then(Value::as_array)
                    .ok_or("'settings' (array) is required")?;
                if raw.is_empty() {
                    return Err("'settings' is empty".into());
                }
                if raw.len() > SET_OPTION_MAX {
                    return Err(format!(
                        "set_option caps at {SET_OPTION_MAX} settings per call (got {})",
                        raw.len()
                    ));
                }
                for s in raw {
                    let ns = s.get("namespace").and_then(Value::as_str).unwrap_or("");
                    let name = s.get("name").and_then(Value::as_str).unwrap_or("");
                    let value = s.get("value").and_then(Value::as_str).unwrap_or("");
                    if ns.is_empty() || name.is_empty() {
                        return Err("each setting needs non-empty namespace and name".into());
                    }
                    settings.push((ns.to_string(), name.to_string(), value.to_string()));
                }
                // Namespaces must already exist in the env's config —
                // the spec's no-cross-env-blast rule.
                let current: Vec<(String, String, String)> = match self.backend {
                    Backend::Demo => demo_fixture::option_settings_for(&env.name),
                    Backend::Aws => {
                        let client = self.client(args).await?;
                        client
                            .fetch_env_option_settings(&env.application, &env.name)
                            .await
                            .map_err(|e| {
                                tool_error(&profile, "fetch_env_option_settings", &e.to_string())
                            })?
                    }
                };
                let known_ns: std::collections::HashSet<&str> =
                    current.iter().map(|(ns, _, _)| ns.as_str()).collect();
                for (ns, _, _) in &settings {
                    if !known_ns.contains(ns.as_str()) {
                        return Err(format!(
                            "namespace '{ns}' is not present in {}'s configuration — refusing (set_option only touches existing namespaces)",
                            env.name
                        ));
                    }
                }
                // Plan rows: old -> new, old redacted per the
                // standing contract (the NEW value is echoed — the
                // agent supplied it).
                let rows: Vec<String> = settings
                    .iter()
                    .map(|(ns, name, new_v)| {
                        let old = current
                            .iter()
                            .find(|(cns, cn, _)| cns == ns && cn == name)
                            .map(|(_, _, v)| redact_option_value(ns, name, v, self.redact))
                            .unwrap_or_else(|| "(unset)".into());
                        format!(
                            "{{\"namespace\":{},\"name\":{},\"old\":{},\"new\":{}}}",
                            util::json_string(ns),
                            util::json_string(name),
                            util::json_string(&old),
                            util::json_string(new_v),
                        )
                    })
                    .collect();
                plan_extra = format!(",\"changes\":[{}]", rows.join(","));
            }
            WriteVerb::Restart | WriteVerb::Rebuild | WriteVerb::Terminate => {}
        }

        // Recent events give the plan operational context (3 max).
        let events_json = match self.backend {
            Backend::Demo => String::new(),
            Backend::Aws => {
                let client = self.client(args).await?;
                match client.list_events_for_env(&env.name, 3).await {
                    Ok(evs) => {
                        let rows: Vec<String> = evs
                            .iter()
                            .map(|e| {
                                format!(
                                    "{{\"severity\":{},\"message\":{}}}",
                                    util::json_string(&e.severity),
                                    util::json_string(&e.message),
                                )
                            })
                            .collect();
                        format!(",\"recent_events\":[{}]", rows.join(","))
                    }
                    // Context, not a gate — a failed event fetch
                    // doesn't block the plan.
                    Err(_) => String::new(),
                }
            }
        };

        let token = mint_token();
        {
            let mut st = self.writes.lock().await;
            if self.dispatching.load(std::sync::atomic::Ordering::SeqCst) {
                return Err("another write is in flight — wait for it to complete".into());
            }
            // A new plan replaces any pending one: the agent
            // re-planned, and two live tokens would be ambiguous.
            // `install` remembers the token it retires so confirming
            // the old one can say what happened.
            st.install(PendingWrite {
                token: token.clone(),
                verb,
                env: env.name.clone(),
                version: version.clone(),
                settings: settings.clone(),
                profile: profile.clone(),
                region: arg_str(args, "region"),
                expires_at: tokio::time::Instant::now()
                    + std::time::Duration::from_secs(CONFIRM_TTL_SECS),
                name_retry_used: false,
            });
        }

        // `next` is a human-readable string VALUE — build it plain,
        // then json_string it so any quotes (terminate's confirm_name
        // hint carries them) are escaped rather than breaking the frame.
        let next = if verb == WriteVerb::Terminate {
            format!(
                "call confirm_action with the confirm_token AND confirm_name={} to dispatch",
                env.name
            )
        } else {
            "call confirm_action with the confirm_token to dispatch".to_string()
        };
        Ok(format!(
            "{{\"pending\":true,\"confirm_token\":{},\"expires_in_secs\":{CONFIRM_TTL_SECS},\"plan\":{{\"action\":{},\"env\":{},\"application\":{},\"health\":{},\"status\":{}{plan_extra}{events_json}}},\"next\":{}}}",
            util::json_string(&token),
            util::json_string(verb.label()),
            util::json_string(&env.name),
            util::json_string(&env.application),
            util::json_string(&env.health),
            util::json_string(&env.status),
            util::json_string(&next),
        ))
    }

    /// Phase 2: dispatch the pending plan.
    pub(super) async fn tool_confirm_action(&self, args: &Value) -> Result<String, String> {
        if !self.allow_writes {
            return Err("writes are disabled — start the server with --allow-writes".into());
        }
        let token = arg_str(args, "confirm_token").ok_or("'confirm_token' is required")?;
        let pending = {
            let mut st = self.writes.lock().await;
            if self.dispatching.load(std::sync::atomic::Ordering::SeqCst) {
                return Err("another write is in flight — wait for it to complete".into());
            }
            let Some(p) = st.pending.as_mut() else {
                return Err("no pending write — call a write tool first".into());
            };
            if p.token != token {
                return Err(mismatched_token_message(&st.retired, &token));
            }
            if tokio::time::Instant::now() >= p.expires_at {
                st.pending = None;
                return Err("confirm_token expired — re-plan required".into());
            }
            if p.verb == WriteVerb::Terminate {
                let supplied = arg_str(args, "confirm_name").unwrap_or_default();
                if supplied != p.env {
                    if p.name_retry_used {
                        st.pending = None;
                        return Err(
                            "confirm_name mismatch twice — plan dropped, re-plan required".into(),
                        );
                    }
                    p.name_retry_used = true;
                    return Err(format!(
                        "confirm_name must equal the env name ({}) — one retry remains on this token",
                        p.env
                    ));
                }
            }
            // Re-gate at CONFIRM time (R1, 0.28 panel): freeze/pin
            // were checked at plan time, but the token window is long
            // enough for an incident to be declared since. A refusal
            // here drops the plan — reality changed, re-plan required.
            if let Some(msg) = crate::cli::write_refusal(
                &self.safety_cfg,
                &p.env,
                &p.profile,
                crate::freeze::read_active(),
            ) {
                st.pending = None;
                return Err(msg);
            }
            // Set BEFORE releasing the writes lock: a concurrent
            // plan/confirm checking `dispatching` must see it true.
            self.dispatching
                .store(true, std::sync::atomic::Ordering::SeqCst);
            // Infallible: the `is_none()` check a few lines up runs
            // under this same `writes` lock, which is not released
            // between there and here, so nothing can take `pending` in
            // between. Restructuring to carry the value down from that
            // check would need the lock guard threaded through the
            // early-return arms for no safety gain.
            #[expect(clippy::expect_used)]
            {
                st.pending.take().expect("checked above under this lock")
            }
        };

        // RAII reset (0.28 pre-tag review I2): if `dispatch_write`
        // panics or unwinds, `dispatching` must still clear —
        // otherwise a single panicked task wedges the whole write
        // surface forever ("another write is in flight" on every
        // subsequent call). An AtomicBool store in Drop is
        // synchronous and runs on unwind; the plain set-false after
        // the await would be skipped.
        struct DispatchGuard<'a>(&'a std::sync::atomic::AtomicBool);
        impl Drop for DispatchGuard<'_> {
            fn drop(&mut self) {
                self.0.store(false, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let _guard = DispatchGuard(&self.dispatching);
        self.dispatch_write(&pending).await
    }

    async fn dispatch_write(&self, p: &PendingWrite) -> Result<String, String> {
        let verb_label = p.verb.label();
        if matches!(self.backend, Backend::Demo) {
            // Synthetic success: no AWS, no audit, no webhook.
            return Ok(format!(
                "{{\"dispatched\":true,\"demo\":true,\"action\":{},\"env\":{}}}",
                util::json_string(verb_label),
                util::json_string(&p.env),
            ));
        }
        let args = json!({
            "profile": p.profile.clone().unwrap_or_default(),
            "region": p.region.clone().unwrap_or_default(),
        });
        let client = self.client(&args).await?;
        let client_name = self
            .client_name
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| "unknown".into());
        let audit_profile = p
            .profile
            .clone()
            .or_else(|| std::env::var("AWS_PROFILE").ok());
        let mut extras: Vec<(&str, String)> =
            vec![("via", "mcp".to_string()), ("client", client_name.clone())];
        if let Some(v) = &p.version {
            extras.push(("version", v.clone()));
        }
        if !p.settings.is_empty() {
            extras.push(("settings", p.settings.len().to_string()));
        }
        let extras_ref: Vec<(&str, &str)> = extras.iter().map(|(k, v)| (*k, v.as_str())).collect();
        crate::audit::append_action_dispatched(
            None,
            audit_profile.as_deref(),
            &client.context.region,
            verb_label,
            &p.env,
            &extras_ref,
        );
        let outcome: Result<(), String> = match p.verb {
            WriteVerb::Deploy => client
                .deploy_version(&p.env, p.version.as_deref().unwrap_or_default())
                .await
                .map_err(|e| e.to_string()),
            WriteVerb::Restart => client
                .restart_app_server(&p.env)
                .await
                .map_err(|e| e.to_string()),
            WriteVerb::Rebuild => client.rebuild_env(&p.env).await.map_err(|e| e.to_string()),
            WriteVerb::Terminate => client
                .terminate_env(&p.env)
                .await
                .map_err(|e| e.to_string()),
            WriteVerb::SetOption => client
                .update_env_option_settings(&p.env, &p.settings, &[])
                .await
                .map_err(|e| e.to_string()),
        };
        crate::audit::append_action_completed(
            None,
            audit_profile.as_deref(),
            &client.context.region,
            verb_label,
            &p.env,
            match &outcome {
                Ok(()) => Ok(()),
                Err(e) => Err(e.as_str()),
            },
            &extras_ref,
        );
        match outcome {
            Ok(()) => Ok(format!(
                "{{\"dispatched\":true,\"action\":{},\"env\":{},\"note\":\"dispatch-only — poll list_environments / recent_events for progress\"}}",
                util::json_string(verb_label),
                util::json_string(&p.env),
            )),
            Err(e) => Err(tool_error(&p.profile, verb_label, &e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_gate_refuses_under_freeze_and_pin() {
        let cfg = crate::config::Config::default();
        // No freeze, no pin -> clear.
        assert!(crate::cli::write_refusal(&cfg, "prod", &None, None).is_none());
        // Active freeze -> refusal names it + the remedy.
        let m = crate::freeze::FreezeMarker {
            pid: 4242,
            reason: "checkout 5xx".into(),
            incident: true,
            at: "now".into(),
        };
        let msg = crate::cli::write_refusal(&cfg, "prod", &None, Some(m)).expect("refused");
        assert!(msg.contains("freeze active") && msg.contains(":incident END"));
        // Pin -> refusal (no freeze).
        let mut pinned = crate::config::Config::default();
        pinned.safety_envs.insert("prod".into(), true);
        let msg2 = crate::cli::write_refusal(&pinned, "prod", &None, None).expect("pin refused");
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
            name_retry_used: false,
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
}
