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

use crate::cli::lint::{fetch_env_lint_inputs, run_rules_for_env, EnvLintInputs};
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

/// Redaction rule for `get_option_settings`: env-var VALUES are
/// secrets (`aws:elasticbeanstalk:application:environment` carries DB
/// URLs, API keys); keys stay visible so config shape is inspectable.
/// `DBPassword` matches the `:rds` precedent. Everything else passes
/// through.
pub(crate) fn redact_option_value(
    namespace: &str,
    name: &str,
    value: &str,
    redact: bool,
) -> String {
    if !redact {
        return value.to_string();
    }
    if namespace == "aws:elasticbeanstalk:application:environment"
        || name.eq_ignore_ascii_case("DBPassword")
    {
        return "(redacted)".to_string();
    }
    value.to_string()
}

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
            "description": "Run ebman's diagnostic rule engine over the fleet (or one env). CAVEATS: EBL011 (worker DLQ) never fires here (queue depths aren't polled outside the TUI); EBL016 (live health probe) and EBL020 (X-Ray IAM) are probe-gated and do not run in this tool. A clean result does NOT clear those rules.",
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
            "description": "Terraform drift report: live env config vs the tfstate's recorded settings. tfstate discovery walks up from the SERVER's working directory (correct for project-scoped .mcp.json which launches in the repo; pass tfstate_path otherwise).",
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
            "description": "Read ebman's local audit log (~/.cache/ebman/audit.log): every dispatched action + outcome. Local to this machine — actions dispatched elsewhere are not recorded.",
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
            "notifications/initialized" | "notifications/cancelled" => None,
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
            _ if id.is_some() => Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": format!("method '{method}' not found")}
            })),
            // Unknown notification: ignore per JSON-RPC.
            _ => None,
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
        let rules = match self.backend {
            Backend::Demo => lint::default_rules(&[]),
            Backend::Aws => {
                let mut disabled = crate::config::load_lint_disables();
                disabled.extend(crate::project::load_lint_disables_from_cwd());
                lint::default_rules(&disabled)
            }
        };
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
        match self.backend {
            Backend::Demo => {
                for env in targets {
                    let inputs = EnvLintInputs {
                        options: demo_fixture::option_settings_for(&env.name),
                        env_tag_keys: Vec::new(),
                        healthy_count: None,
                        xray_denied: None,
                        probe_failure: None,
                        newer_stack: None,
                    };
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
                for env in targets {
                    let inputs = fetch_env_lint_inputs(&client, env, &latest_stacks, false)
                        .await
                        .map_err(|e| tool_error(&profile, "fetch_env_option_settings", &e))?;
                    all_issues.extend(run_rules_for_env(&rules, env, &inputs, &required_tags));
                }
            }
        }
        if let Some(min) = severity {
            all_issues.retain(|i| i.severity >= min);
        }
        if !rule_filter.is_empty() {
            all_issues.retain(|i| rule_filter.contains(&i.rule_id));
        }
        Ok(lint::render_issues_json(&all_issues))
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
        let mut reports: Vec<(String, bool, Vec<terraform::DriftField>)> = Vec::new();
        for env in &envs {
            if let Some(only) = env_filter.as_deref() {
                if env.name != only {
                    continue;
                }
            }
            let Some(tf) = state.env_by_name(&env.name) else {
                reports.push((env.name.clone(), false, Vec::new()));
                continue;
            };
            let opts = client
                .fetch_env_option_settings(&env.application, &env.name)
                .await
                .map_err(|e| tool_error(&profile, "fetch_env_option_settings", &e.to_string()))?;
            reports.push((
                env.name.clone(),
                true,
                terraform::compute_drift(tf, env, &opts),
            ));
        }
        Ok(terraform::render_drift_json(used_path.as_deref(), &reports))
    }

    fn tool_audit_log(&self, args: &Value) -> Result<String, String> {
        // Hermetic in demo mode: the real local log is operator data,
        // not fixture data.
        if matches!(self.backend, Backend::Demo) {
            return Ok(audit_log::render_audit_entries_json(&[]));
        }
        let limit = arg_u64(args, "limit")
            .map(|l| (l as usize).min(AUDIT_LOG_MAX_LIMIT))
            .unwrap_or(AUDIT_LOG_DEFAULT_LIMIT);
        let since_dt = match arg_str(args, "since") {
            None => None,
            Some(s) => {
                let ms = aws::parse_window_ms(&s)
                    .ok_or_else(|| format!("bad 'since' window '{s}' (use 5m / 1h / 2d)"))?;
                Some(chrono::Utc::now() - chrono::Duration::milliseconds(ms))
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
        let entries: Vec<audit_log::AuditEntry> = text
            .lines()
            .filter_map(audit_log::parse_audit_line)
            .filter(|e| filter.matches(e))
            .collect();
        // Newest kept: the file is append-ordered, so take the tail.
        let start = entries.len().saturating_sub(limit);
        Ok(audit_log::render_audit_entries_json(&entries[start..]))
    }

    async fn tool_recent_events(&self, args: &Value) -> Result<String, String> {
        let max = arg_u64(args, "max")
            .map(|m| (m as i32).min(EVENTS_MAX_MAX))
            .unwrap_or(EVENTS_DEFAULT_MAX)
            .max(1);
        let env = arg_str(args, "env");
        let events: Vec<aws::Event> = match self.backend {
            Backend::Demo => {
                let all = match env.as_deref() {
                    Some(name) => demo_fixture::events_for_env(name),
                    None => demo_fixture::envs()
                        .iter()
                        .flat_map(|e| demo_fixture::events_for_env(&e.name))
                        .collect(),
                };
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
            .map(|l| l as usize)
            .unwrap_or(VERSIONS_DEFAULT_LIMIT)
            .max(1);
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
        let total: f64 = cache.costs.values().sum();
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
/// rewrite (`app::rewrite_credential_error`) so an expired SSO token
/// reaches the agent as `aws sso login --profile X`, then fall back
/// to `op failed: msg`.
fn tool_error(profile: &Option<String>, op: &str, msg: &str) -> String {
    let profile_name = profile
        .clone()
        .or_else(|| std::env::var("AWS_PROFILE").ok())
        .unwrap_or_else(|| "default".into());
    match crate::app::rewrite_credential_error(&profile_name, msg) {
        Some(crate::app::CredentialHint::Expired(text))
        | Some(crate::app::CredentialHint::Invalid(text)) => text,
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

    // Single writer task: concurrent tool tasks send completed frames
    // through the channel so stdout writes can't interleave.
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let mut stdout = tokio::io::stdout();
        while let Some(line) = out_rx.recv().await {
            let _ = stdout.write_all(line.as_bytes()).await;
            let _ = stdout.write_all(b"\n").await;
            let _ = stdout.flush().await;
        }
    });

    use tokio::io::AsyncBufReadExt;
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                let _ = out_tx.send(
                    json!({
                        "jsonrpc": "2.0",
                        "id": null,
                        "error": {"code": -32700, "message": "parse error"}
                    })
                    .to_string(),
                );
                continue;
            }
        };
        // tools/call may hit AWS for many seconds — spawn it so the
        // loop stays responsive to ping / further calls. Everything
        // else is answered inline (cheap + ordering-sensitive).
        if req.get("method").and_then(Value::as_str) == Some("tools/call") {
            let server = Arc::clone(&server);
            let out_tx = out_tx.clone();
            tokio::spawn(async move {
                if let Some(resp) = server.handle_request(&req).await {
                    let _ = out_tx.send(resp.to_string());
                }
            });
        } else if let Some(resp) = server.handle_request(&req).await {
            let _ = out_tx.send(resp.to_string());
        }
    }
    // stdin closed: drop the sender so the writer drains and exits.
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
