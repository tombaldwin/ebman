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
pub(crate) const PROTOCOL_VERSION: &str = "2025-06-18";

/// Hard wall-clock bound on a single tool call so a hung AWS call
/// can't wedge the agent turn.
const TOOL_TIMEOUT_SECS: u64 = 30;

/// Output caps (spec: every tool output is bounded — agents consume
/// results into finite context windows).
const AUDIT_LOG_DEFAULT_LIMIT: usize = 100;
const AUDIT_LOG_MAX_LIMIT: usize = 500;
const EVENTS_DEFAULT_MAX: i32 = 50;
const EVENTS_MAX_MAX: i32 = 200;
const VERSIONS_DEFAULT_LIMIT: usize = 50;
const VERSIONS_MAX_LIMIT: usize = 200;

/// Bound on concurrent per-env AWS fetches inside one tool call
/// (lint / drift fan-outs). Unbounded `join_all` over a large fleet
/// is exactly how you provoke `Throttling: Rate exceeded`.
const FETCH_CONCURRENCY: usize = 4;

#[derive(Debug, PartialEq, Eq)]
struct McpArgs {
    demo: bool,
    no_redact: bool,
}

const MCP_USAGE: &str = "usage: ebman mcp serve [--demo] [--no-redact]";

fn parse_mcp_args(args: &[String]) -> Result<McpArgs, String> {
    // args[0] = "mcp"; the only sub-verb is "serve".
    if args.get(1).map(String::as_str) != Some("serve") {
        return Err(MCP_USAGE.into());
    }
    let mut demo = false;
    let mut no_redact = false;
    for arg in args.iter().skip(2) {
        match arg.as_str() {
            "--demo" => demo = true,
            "--no-redact" => no_redact = true,
            other => return Err(format!("ebman mcp: unknown flag '{other}' — {MCP_USAGE}")),
        }
    }
    Ok(McpArgs { demo, no_redact })
}

/// One env's drift entry: (env name, tf-matched, drifted fields).
type DriftReport = (String, bool, Vec<terraform::DriftField>);

/// The CLI audit renderer emits JSON Lines (one object per line);
/// every MCP tool returns a single JSON document, so the audit tool
/// wraps the lines into an array (`[]` for an empty log).
fn jsonl_to_array(jsonl: &str) -> String {
    let items: Vec<&str> = jsonl.lines().filter(|l| !l.trim().is_empty()).collect();
    format!("[{}]", items.join(","))
}

/// Apply the `get_option_settings` redaction contract to drift
/// reports in place: tf configs routinely pin env-var secrets, and a
/// drifted secret would otherwise leak both its tf and live values
/// through the `drift` tool. The drifted/not-drifted signal survives.
fn redact_drift_reports(reports: &mut [DriftReport]) {
    for (_, _, fields) in reports.iter_mut() {
        terraform::redact_drift_fields(fields);
    }
}

/// Apply the redaction contract to audit entries before serving them
/// through the MCP tool: `:set-option` / `lint --fix` audit lines
/// carry namespace+name+value extras, and env-var values / DBPassword
/// must not be readable here when `get_option_settings` withholds
/// them (0.26 max-review C1 — third instance of the leak class).
/// Keys stay visible; both extra-key spellings (`ns` from the TUI,
/// `namespace` from lint --fix) are honoured.
fn redact_audit_entries(entries: &mut [audit_log::AuditEntry]) {
    for e in entries.iter_mut() {
        let ns = e
            .extras
            .get("ns")
            .or_else(|| e.extras.get("namespace"))
            .cloned()
            .unwrap_or_default();
        let name = e.extras.get("name").cloned().unwrap_or_default();
        if let Some(v) = e.extras.get_mut("value") {
            *v = redact_option_value(&ns, &name, v, true);
        }
    }
}

/// Splice a string array into the trailing `}` of a JSON document.
/// No-op for an empty list, so the common-case schema stays
/// byte-identical to the CLI's.
fn append_string_array(mut body: String, key: &str, items: &[String]) -> String {
    if items.is_empty() {
        return body;
    }
    let rendered: Vec<String> = items.iter().map(|s| util::json_string(s)).collect();
    body.truncate(body.len() - 1);
    body.push_str(&format!(
        ",{}:[{}]}}",
        util::json_string(key),
        rendered.join(",")
    ));
    body
}

/// Degraded-coverage note for the lint/drift tools (see the tool
/// descriptions: the agent must check it before treating a run as
/// full coverage).
fn append_skipped_envs(body: String, skipped: &[String]) -> String {
    append_string_array(body, "skipped_envs", skipped)
}

pub(crate) use crate::util::redact_option_value;

/// The static tool table. Descriptions carry the coverage caveats —
/// an agent treats "no findings" as authoritative, so a wiring gap
/// (EBL011/016/020 can't fire here) must be stated IN the tool.
fn tool_table() -> Value {
    json!([
        {
            "name": "list_environments",
            "description": "List Elastic Beanstalk environments (name, application, status, health, platform, cname, version_label). Same schema as `ebman envs --json`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "profile": {"type": "string", "description": "AWS profile (default: ambient)"},
                    "region": {"type": "string", "description": "AWS region (default: profile/env default)"}
                }
            }
        },
        {
            "name": "lint",
            "description": "Run ebman's diagnostic rule engine over the fleet (or one env). CAVEATS: EBL011 (worker DLQ) never fires here (queue depths aren't polled outside the TUI); EBL016 (live health probe) does not run in this tool. A clean result does NOT clear those rules. EBL015 (stale custom platforms, account-level) runs only when not scoped to a single env. Envs whose input fetch fails are skipped, not fatal — a `skipped_envs` array in the result lists them, so check it before treating the run as full coverage.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "env": {"type": "string", "description": "Limit to one environment"},
                    "severity": {"type": "string", "description": "Minimum severity: info | warn | error"},
                    "rules": {"type": "string", "description": "Comma-separated rule ids to keep (e.g. EBL001,EBL014)"},
                    "profile": {"type": "string"},
                    "region": {"type": "string"}
                }
            }
        },
        {
            "name": "get_option_settings",
            "description": "One environment's resolved option settings (namespace / name / value). Env-var VALUES and DBPassword are redacted by default (keys stay visible); start the server with --no-redact to disable.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "env": {"type": "string", "description": "Environment name (required)"},
                    "profile": {"type": "string"},
                    "region": {"type": "string"}
                },
                "required": ["env"]
            }
        },
        {
            "name": "drift",
            "description": "Terraform drift report: live env config vs the tfstate's recorded settings. tfstate discovery walks up from the SERVER's working directory (correct for project-scoped .mcp.json which launches in the repo; pass tfstate_path otherwise). Drifted env-var values and DBPassword are redacted like get_option_settings (the drifted signal survives; --no-redact disables).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "env": {"type": "string", "description": "Limit to one environment"},
                    "tfstate_path": {"type": "string", "description": "Explicit terraform.tfstate path"},
                    "profile": {"type": "string"},
                    "region": {"type": "string"}
                }
            }
        },
        {
            "name": "audit_log",
            "description": "Read ebman's local audit log (~/.cache/ebman/audit.log): every dispatched action + outcome, as a JSON array of entries. Local to this machine — actions dispatched elsewhere are not recorded.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "since": {"type": "string", "description": "Window like 5m / 1h / 2d"},
                    "env": {"type": "string", "description": "Filter by target env"},
                    "action": {"type": "string", "description": "Filter by action label (e.g. Deploy)"},
                    "limit": {"type": "integer", "description": "Max entries, newest kept (default 100, cap 500)"}
                }
            }
        },
        {
            "name": "recent_events",
            "description": "Recent Elastic Beanstalk events, fleet-wide or for one env, newest first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "env": {"type": "string", "description": "Limit to one environment"},
                    "max": {"type": "integer", "description": "Max events (default 50, cap 200)"},
                    "profile": {"type": "string"},
                    "region": {"type": "string"}
                }
            }
        },
        {
            "name": "list_versions",
            "description": "Application versions for an environment's application, newest first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "env": {"type": "string", "description": "Environment name (required)"},
                    "limit": {"type": "integer", "description": "Max versions (default 50)"},
                    "profile": {"type": "string"},
                    "region": {"type": "string"}
                },
                "required": ["env"]
            }
        },
        {
            "name": "fleet_cost",
            "description": "Cached Cost Explorer summary per environment ($/month). Reads ebman's local cost cache only (populated by `:cost on` in the TUI) — never calls Cost Explorer itself. `stale: true` means the cache is older than 24h; an empty result means cost tracking hasn't been enabled.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "profile": {"type": "string"},
                    "region": {"type": "string"}
                }
            }
        }
    ])
}

/// Where tool data comes from. `Demo` serves the synthetic fixture
/// (zero AWS, zero disk) — the e2e harness and screenshot mode.
enum Backend {
    Aws,
    Demo,
}

pub(crate) struct Server {
    backend: Backend,
    redact: bool,
}

/// Helper: string arg off a tools/call `arguments` object.
fn arg_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn arg_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(Value::as_u64)
}

impl Server {
    pub(crate) fn new(demo: bool, no_redact: bool) -> Self {
        Server {
            backend: if demo { Backend::Demo } else { Backend::Aws },
            redact: !no_redact,
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
                "result": {"tools": tool_table()}
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
                if !tool_table()
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

    /// Build the per-call AWS client. Errors go through the shared
    /// credential rewrite so an expired SSO token surfaces as the
    /// `aws sso login` hint the agent can relay, not SDK noise.
    async fn client(&self, args: &Value) -> Result<aws::AwsClient, String> {
        let profile = arg_str(args, "profile");
        let region = arg_str(args, "region");
        aws::AwsClient::with(profile.clone(), region)
            .await
            .map_err(|e| tool_error(&profile, "AwsClient", &e.to_string()))
    }

    async fn fetch_envs(&self, args: &Value) -> Result<Vec<aws::Environment>, String> {
        match self.backend {
            Backend::Demo => Ok(demo_fixture::envs()),
            Backend::Aws => {
                let profile = arg_str(args, "profile");
                let client = self.client(args).await?;
                client
                    .list_environments()
                    .await
                    .map_err(|e| tool_error(&profile, "list_environments", &e.to_string()))
            }
        }
    }

    async fn call_tool(&self, name: &str, args: &Value) -> Result<String, String> {
        match name {
            "list_environments" => {
                let envs = self.fetch_envs(args).await?;
                Ok(crate::cli::envs::render_envs_json(&envs))
            }
            "lint" => self.tool_lint(args).await,
            "get_option_settings" => self.tool_option_settings(args).await,
            "drift" => self.tool_drift(args).await,
            "audit_log" => self.tool_audit_log(args),
            "recent_events" => self.tool_recent_events(args).await,
            "list_versions" => self.tool_list_versions(args).await,
            "fleet_cost" => self.tool_fleet_cost(args).await,
            // Belt-and-braces: the RPC layer already 32602s names not
            // in tool_table(), so this is unreachable unless the table
            // and this match drift — in which case failing loud here
            // beats a silent gap.
            other => Err(format!("unknown tool '{other}'")),
        }
    }

    async fn tool_lint(&self, args: &Value) -> Result<String, String> {
        let env_filter = arg_str(args, "env");
        let severity = match arg_str(args, "severity") {
            None => None,
            Some(s) => Some(
                lint::Severity::parse(&s)
                    .ok_or_else(|| format!("unknown severity '{s}' (info / warn / error)"))?,
            ),
        };
        let rule_filter: Vec<String> = arg_str(args, "rules")
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        // Hermetic in demo mode: no config-driven disables.
        let disabled = match self.backend {
            Backend::Demo => Vec::new(),
            Backend::Aws => {
                let mut disabled = crate::config::load_lint_disables();
                disabled.extend(crate::project::load_lint_disables_from_cwd());
                disabled
            }
        };
        let rules = lint::default_rules(&disabled);
        let required_tags = match self.backend {
            Backend::Demo => Vec::new(),
            Backend::Aws => crate::config::load().required_tags,
        };
        let envs = self.fetch_envs(args).await?;
        let targets: Vec<&aws::Environment> = match env_filter.as_deref() {
            Some(name) => {
                let found = envs
                    .iter()
                    .find(|e| e.name == name)
                    .ok_or_else(|| format!("env '{name}' not found"))?;
                vec![found]
            }
            None => envs.iter().collect(),
        };
        let mut all_issues: Vec<lint::Issue> = Vec::new();
        let mut platform_warnings: Vec<String> = Vec::new();
        // Envs whose input fetch failed — reported in the result as
        // `skipped_envs` so the agent knows coverage shrank (the CLI's
        // `cycle_degraded` tolerance, in tool-result shape). One
        // terminating env must not turn fleet lint into an error.
        let mut skipped: Vec<String> = Vec::new();
        match self.backend {
            Backend::Demo => {
                for env in targets {
                    let inputs = EnvLintInputs::bare(demo_fixture::option_settings_for(&env.name));
                    all_issues.extend(run_rules_for_env(&rules, env, &inputs, &required_tags));
                }
            }
            Backend::Aws => {
                let profile = arg_str(args, "profile");
                let client = self.client(args).await?;
                let latest_stacks = match client.list_solution_stacks().await {
                    Ok(stacks) => aws::latest_stack_versions(&stacks),
                    // EBL008 quietly loses its input — same tolerance
                    // as the CLI path.
                    Err(_) => std::collections::HashMap::new(),
                };
                // Bounded concurrent fan-out — serial cost is ~2s/env,
                // which brushes the 30s tool timeout on large fleets;
                // unbounded join_all provokes throttling. Order is
                // preserved, so output stays deterministic.
                use futures::StreamExt;
                // Eagerly-built future list — the lazy closure-map
                // form trips rustc's HRTB inference (same as drift).
                let mut fetches = Vec::with_capacity(targets.len());
                for env in targets.iter().copied() {
                    fetches.push(fetch_env_lint_inputs(&client, env, &latest_stacks, false));
                }
                let fetched: Vec<Result<EnvLintInputs, String>> = futures::stream::iter(fetches)
                    .buffered(FETCH_CONCURRENCY)
                    .collect()
                    .await;
                for (env, inputs) in targets.iter().zip(fetched) {
                    match inputs {
                        Ok(inputs) => all_issues.extend(run_rules_for_env(
                            &rules,
                            env,
                            &inputs,
                            &required_tags,
                        )),
                        // Route through the credential rewrite so an
                        // expired-SSO skip still carries the fix hint.
                        Err(e) => skipped.push(format!(
                            "{}: {}",
                            env.name,
                            tool_error(&profile, "fetch_env_lint_inputs", &e)
                        )),
                    }
                }
                // EBL015 — account-level pass via the assembly shared
                // with the CLI: skipped when scoped to one env or
                // disabled; failures skip silently (a tool result
                // shouldn't fail over an Info-severity side pass).
                if env_filter.is_none() && !disabled.iter().any(|d| d == "EBL015") {
                    if let Ok((issues, warnings)) =
                        fetch_stale_platform_issues(&client, chrono::Utc::now()).await
                    {
                        all_issues.extend(issues);
                        // Per-branch date-fetch failures surface like the
                        // CLI's stderr warnings do — dropped silently, an
                        // agent can't know EBL015 coverage shrank.
                        platform_warnings = warnings;
                    }
                }
            }
        }
        if let Some(min) = severity {
            all_issues.retain(|i| i.severity >= min);
        }
        if !rule_filter.is_empty() {
            all_issues.retain(|i| rule_filter.contains(&i.rule_id));
        }
        Ok(append_string_array(
            append_skipped_envs(lint::render_issues_json(&all_issues), &skipped),
            "warnings",
            &platform_warnings,
        ))
    }

    async fn tool_option_settings(&self, args: &Value) -> Result<String, String> {
        let env_name = arg_str(args, "env").ok_or("'env' is required")?;
        let options: Vec<(String, String, String)> = match self.backend {
            Backend::Demo => {
                // Unknown demo env still errors like live would.
                if !demo_fixture::envs().iter().any(|e| e.name == env_name) {
                    return Err(format!("env '{env_name}' not found"));
                }
                demo_fixture::option_settings_for(&env_name)
            }
            Backend::Aws => {
                let profile = arg_str(args, "profile");
                let client = self.client(args).await?;
                let envs = client
                    .list_environments()
                    .await
                    .map_err(|e| tool_error(&profile, "list_environments", &e.to_string()))?;
                let env = envs
                    .iter()
                    .find(|e| e.name == env_name)
                    .ok_or_else(|| format!("env '{env_name}' not found"))?;
                client
                    .fetch_env_option_settings(&env.application, &env.name)
                    .await
                    .map_err(|e| {
                        tool_error(&profile, "fetch_env_option_settings", &e.to_string())
                    })?
            }
        };
        let entries: Vec<String> = options
            .iter()
            .map(|(ns, n, v)| {
                format!(
                    "{{\"namespace\":{},\"name\":{},\"value\":{}}}",
                    util::json_string(ns),
                    util::json_string(n),
                    util::json_string(&redact_option_value(ns, n, v, self.redact)),
                )
            })
            .collect();
        Ok(format!(
            "{{\"env\":{},\"redacted\":{},\"options\":[{}]}}",
            util::json_string(&env_name),
            self.redact,
            entries.join(",")
        ))
    }

    async fn tool_drift(&self, args: &Value) -> Result<String, String> {
        // Demo mode ships no tfstate — honest empty report.
        if matches!(self.backend, Backend::Demo) {
            return Ok(terraform::render_drift_json(None, &[]));
        }
        let (state, used_path) = match arg_str(args, "tfstate_path") {
            Some(p) => {
                let path = std::path::PathBuf::from(&p);
                let state = terraform::load_from_path(&path)
                    .ok_or_else(|| format!("could not parse tfstate at '{p}'"))?;
                (state, Some(path))
            }
            None => {
                let found = terraform::find_tfstate(std::path::Path::new(".")).ok_or(
                    "no terraform.tfstate discovered walking up from cwd — pass tfstate_path",
                )?;
                let state = terraform::load_from_path(&found)
                    .ok_or_else(|| format!("could not parse tfstate at '{}'", found.display()))?;
                (state, Some(found))
            }
        };
        let profile = arg_str(args, "profile");
        let client = self.client(args).await?;
        let envs = client
            .list_environments()
            .await
            .map_err(|e| tool_error(&profile, "list_environments", &e.to_string()))?;
        let env_filter = arg_str(args, "env");
        // Bounded concurrent fetch for tf-matched envs — same 30s
        // tool-timeout math as the lint tool's fan-out; capped so a
        // large fleet can't provoke AWS throttling.
        let targets: Vec<&aws::Environment> = envs
            .iter()
            .filter(|env| env_filter.as_deref().is_none_or(|only| env.name == only))
            .collect();
        use futures::StreamExt;
        let (client_ref, state_ref, profile_ref) = (&client, &state, &profile);
        // Eagerly-built future list (not a lazy closure map) so the
        // per-env borrows get one concrete lifetime — the inline
        // async-move-closure form trips rustc's HRTB inference here.
        let mut fetches = Vec::with_capacity(targets.len());
        for env in targets.iter().copied() {
            fetches.push(async move {
                let Some(tf) = state_ref.env_by_name(&env.name) else {
                    return Ok((env.name.clone(), false, Vec::new()));
                };
                let opts = client_ref
                    .fetch_env_option_settings(&env.application, &env.name)
                    .await
                    .map_err(|e| {
                        tool_error(profile_ref, "fetch_env_option_settings", &e.to_string())
                    })?;
                Ok((
                    env.name.clone(),
                    true,
                    terraform::compute_drift(tf, env, &opts),
                ))
            });
        }
        let fetched: Vec<Result<DriftReport, String>> = futures::stream::iter(fetches)
            .buffered(FETCH_CONCURRENCY)
            .collect()
            .await;
        // Same degradation contract as the lint tool: one env's fetch
        // failure (terminating env, throttle) skips that env and is
        // reported in `skipped_envs`, instead of erroring the whole
        // fleet report.
        let mut reports: Vec<DriftReport> = Vec::new();
        let mut skipped: Vec<String> = Vec::new();
        for (env, r) in targets.iter().zip(fetched) {
            match r {
                Ok(rep) => reports.push(rep),
                Err(e) => skipped.push(format!("{}: {e}", env.name)),
            }
        }
        if self.redact {
            redact_drift_reports(&mut reports);
        }
        Ok(append_skipped_envs(
            terraform::render_drift_json(used_path.as_deref(), &reports),
            &skipped,
        ))
    }

    fn tool_audit_log(&self, args: &Value) -> Result<String, String> {
        // Hermetic in demo mode: the real local log is operator data,
        // not fixture data.
        if matches!(self.backend, Backend::Demo) {
            return Ok(jsonl_to_array(&audit_log::render_audit_entries_json(&[])));
        }
        let limit = arg_u64(args, "limit")
            .map(|l| (l as usize).clamp(1, AUDIT_LOG_MAX_LIMIT))
            .unwrap_or(AUDIT_LOG_DEFAULT_LIMIT);
        let since_dt = match arg_str(args, "since") {
            None => None,
            Some(s) => {
                let ms = aws::parse_window_ms(&s)
                    .ok_or_else(|| format!("bad 'since' window '{s}' (use 5m / 1h / 2d)"))?;
                // checked_sub: parse_window_ms bounds the window, but a
                // panic here would leave the request unanswered forever
                // — never trust a subtraction on client input.
                Some(
                    chrono::Utc::now()
                        .checked_sub_signed(chrono::Duration::milliseconds(ms))
                        .ok_or_else(|| format!("'since' window '{s}' is out of range"))?,
                )
            }
        };
        let env = arg_str(args, "env");
        let action = arg_str(args, "action");
        let filter = audit_log::AuditFilter {
            since: since_dt,
            env: env.as_deref(),
            rule: None,
            action: action.as_deref(),
        };
        let path = util::cache_dir().join("audit.log");
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut entries: Vec<audit_log::AuditEntry> = text
            .lines()
            .filter_map(audit_log::parse_audit_line)
            .filter(|e| filter.matches(e))
            .collect();
        if self.redact {
            redact_audit_entries(&mut entries);
        }
        // Newest kept: the file is append-ordered, so take the tail.
        let start = entries.len().saturating_sub(limit);
        Ok(jsonl_to_array(&audit_log::render_audit_entries_json(
            &entries[start..],
        )))
    }

    async fn tool_recent_events(&self, args: &Value) -> Result<String, String> {
        // Clamp in u64 first — an `as i32` cast bit-truncates, so
        // max=2^32+5 used to mean 5, not the cap.
        let max = arg_u64(args, "max")
            .map(|m| i32::try_from(m.min(EVENTS_MAX_MAX as u64)).expect("clamped to i32 range"))
            .unwrap_or(EVENTS_DEFAULT_MAX)
            .max(1);
        let env = arg_str(args, "env");
        let events: Vec<aws::Event> = match self.backend {
            Backend::Demo => {
                let mut all: Vec<aws::Event> = match env.as_deref() {
                    Some(name) => demo_fixture::events_for_env(name),
                    None => demo_fixture::envs()
                        .iter()
                        .flat_map(|e| demo_fixture::events_for_env(&e.name))
                        .collect(),
                };
                // The fleet-wide concat is grouped by env; sort so the
                // cap keeps the globally newest (the promised order).
                all.sort_by_key(|e| std::cmp::Reverse(e.at));
                all.into_iter().take(max as usize).collect()
            }
            Backend::Aws => {
                let profile = arg_str(args, "profile");
                let client = self.client(args).await?;
                match env.as_deref() {
                    Some(name) => client.list_events_for_env(name, max).await,
                    None => client.list_events(max).await,
                }
                .map_err(|e| tool_error(&profile, "describe_events", &e.to_string()))?
            }
        };
        let entries: Vec<String> = events
            .iter()
            .map(|e| {
                format!(
                    "{{\"at\":{},\"env\":{},\"severity\":{},\"message\":{}}}",
                    e.at.map(|t| util::json_string(&t.to_rfc3339()))
                        .unwrap_or_else(|| "null".into()),
                    util::json_string(&e.env),
                    util::json_string(&e.severity),
                    util::json_string(&e.message),
                )
            })
            .collect();
        Ok(format!("[{}]", entries.join(",")))
    }

    async fn tool_list_versions(&self, args: &Value) -> Result<String, String> {
        let env_name = arg_str(args, "env").ok_or("'env' is required")?;
        let limit = arg_u64(args, "limit")
            .map(|l| (l as usize).clamp(1, VERSIONS_MAX_LIMIT))
            .unwrap_or(VERSIONS_DEFAULT_LIMIT);
        let versions: Vec<aws::AppVersion> = match self.backend {
            Backend::Demo => {
                let envs = demo_fixture::envs();
                let env = envs
                    .iter()
                    .find(|e| e.name == env_name)
                    .ok_or_else(|| format!("env '{env_name}' not found"))?;
                demo_fixture::deploys_for_app(&env.application)
            }
            Backend::Aws => {
                let profile = arg_str(args, "profile");
                let client = self.client(args).await?;
                let envs = client
                    .list_environments()
                    .await
                    .map_err(|e| tool_error(&profile, "list_environments", &e.to_string()))?;
                let env = envs
                    .iter()
                    .find(|e| e.name == env_name)
                    .ok_or_else(|| format!("env '{env_name}' not found"))?;
                client
                    .list_application_versions(&env.application)
                    .await
                    .map_err(|e| {
                        tool_error(&profile, "list_application_versions", &e.to_string())
                    })?
            }
        };
        let entries: Vec<String> = versions
            .iter()
            .take(limit)
            .map(|v| {
                format!(
                    "{{\"label\":{},\"created\":{},\"description\":{}}}",
                    util::json_string(&v.label),
                    v.created
                        .map(|t| util::json_string(&t.to_rfc3339()))
                        .unwrap_or_else(|| "null".into()),
                    util::json_string(&v.description),
                )
            })
            .collect();
        Ok(format!("[{}]", entries.join(",")))
    }

    async fn tool_fleet_cost(&self, args: &Value) -> Result<String, String> {
        let (account, region, cache) = match self.backend {
            Backend::Demo => {
                let cache = cost_cache::CostCache {
                    fetched_at: None,
                    costs: demo_fixture::envs()
                        .iter()
                        .map(|e| (e.name.clone(), 42.0))
                        .collect(),
                };
                ("123456789012".to_string(), "us-east-1".to_string(), cache)
            }
            Backend::Aws => {
                let profile = arg_str(args, "profile");
                let client = self.client(args).await?;
                let identity = client
                    .verify_identity()
                    .await
                    .map_err(|e| tool_error(&profile, "sts get-caller-identity", &e.to_string()))?;
                let account = identity.account_id.unwrap_or_else(|| "unknown".into());
                let region = client.context.region.clone();
                let cache = cost_cache::load(&account, &region);
                (account, region, cache)
            }
        };
        let stale = cache.is_stale(chrono::Utc::now());
        let by_env: Vec<String> = {
            let mut pairs: Vec<(&String, &f64)> = cache.costs.iter().collect();
            pairs.sort_by(|a, b| a.0.cmp(b.0));
            pairs
                .iter()
                .map(|(name, usd)| format!("{}:{usd:.2}", util::json_string(name)))
                .collect()
        };
        // f64's Sum impl folds from -0.0, so an empty cache would render
        // "-0.00"; adding 0.0 normalises negative zero to positive.
        let total: f64 = cache.costs.values().sum::<f64>() + 0.0;
        Ok(format!(
            "{{\"account\":{},\"region\":{},\"fetched_at\":{},\"stale\":{stale},\"total_usd_month\":{total:.2},\"by_env\":{{{}}}}}",
            util::json_string(&account),
            util::json_string(&region),
            cache
                .fetched_at
                .map(|t| util::json_string(&t.to_rfc3339()))
                .unwrap_or_else(|| "null".into()),
            by_env.join(",")
        ))
    }
}

/// Tool-error formatting: route through the shared credential
/// rewrite (`aws::rewrite_credential_error`) so an expired SSO token
/// reaches the agent as `aws sso login --profile X`, then fall back
/// to `op failed: msg`.
fn tool_error(profile: &Option<String>, op: &str, msg: &str) -> String {
    let profile_name = profile
        .clone()
        .or_else(|| std::env::var("AWS_PROFILE").ok())
        .unwrap_or_else(|| "default".into());
    match crate::aws::rewrite_credential_error(&profile_name, msg) {
        Some(crate::aws::CredentialHint::Expired(text))
        | Some(crate::aws::CredentialHint::Invalid(text)) => text,
        None => format!("{op} failed: {msg}"),
    }
}

pub async fn run(args: &[String]) -> Result<()> {
    let McpArgs { demo, no_redact } = match parse_mcp_args(args) {
        Ok(parsed) => parsed,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };
    let server = Arc::new(Server::new(demo, no_redact));
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
        // Non-object frames (batch arrays, bare scalars) are invalid
        // requests per JSON-RPC 2.0 — answer -32600 rather than
        // silently dropping them (a strict client would wait out its
        // timeout). MCP 2025-06-18 removed batching, so arrays are
        // not-supported by spec.
        if !req.is_object() {
            let _ = out_tx
                .send(
                    json!({
                        "jsonrpc": "2.0",
                        "id": null,
                        "error": {"code": -32600, "message": "invalid request: expected a single JSON-RPC object"}
                    })
                    .to_string(),
                )
                .await;
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    fn demo_server() -> Server {
        Server::new(true, false)
    }

    async fn rpc(server: &Server, frame: Value) -> Option<Value> {
        server.handle_request(&frame).await
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
        let open = Server::new(true, true);
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
        let server = Server::new(true, false);
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
