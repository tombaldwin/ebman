//! The MCP tool layer: the registry table, the read tools, and their
//! shared helpers — carved out of the former single-file server (0.28,
//! the registry-unification refactor the 0.26 architecture review
//! gated on v2 writes). The protocol loop stays in `mod.rs`; write
//! tools live in `writes.rs`.

use super::*;

/// Output caps (spec: every tool output is bounded — agents consume
/// results into finite context windows).
pub(super) const AUDIT_LOG_DEFAULT_LIMIT: usize = 100;
pub(super) const AUDIT_LOG_MAX_LIMIT: usize = 500;
pub(super) const EVENTS_DEFAULT_MAX: i32 = 50;
pub(super) const EVENTS_MAX_MAX: i32 = 200;
pub(super) const VERSIONS_DEFAULT_LIMIT: usize = 50;
pub(super) const VERSIONS_MAX_LIMIT: usize = 200;

/// Bound on concurrent per-env AWS fetches inside one tool call
/// (lint / drift fan-outs). Unbounded `join_all` over a large fleet
/// is exactly how you provoke `Throttling: Rate exceeded`.
pub(super) const FETCH_CONCURRENCY: usize = 4;

/// One env's drift entry: (env name, tf-matched, drifted fields).
pub(super) type DriftReport = (String, bool, Vec<terraform::DriftField>);

/// The CLI audit renderer emits JSON Lines (one object per line);
/// every MCP tool returns a single JSON document, so the audit tool
/// wraps the lines into an array (`[]` for an empty log).
pub(super) fn jsonl_to_array(jsonl: &str) -> String {
    let items: Vec<&str> = jsonl.lines().filter(|l| !l.trim().is_empty()).collect();
    format!("[{}]", items.join(","))
}

/// Apply the `get_option_settings` redaction contract to drift
/// reports in place: tf configs routinely pin env-var secrets, and a
/// drifted secret would otherwise leak both its tf and live values
/// through the `drift` tool. The drifted/not-drifted signal survives.
pub(super) fn redact_drift_reports(reports: &mut [DriftReport]) {
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
pub(super) fn redact_audit_entries(entries: &mut [audit_log::AuditEntry]) {
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
pub(super) fn append_string_array(mut body: String, key: &str, items: &[String]) -> String {
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
pub(super) fn append_skipped_envs(body: String, skipped: &[String]) -> String {
    append_string_array(body, "skipped_envs", skipped)
}

/// The static tool table. Descriptions carry the coverage caveats —
/// an agent treats "no findings" as authoritative, so a wiring gap
/// (EBL011/016/020 can't fire here) must be stated IN the tool.
pub(super) fn tool_table(allow_writes: bool) -> Value {
    let mut tools = read_tool_table();
    if allow_writes {
        if let Some(arr) = tools.as_array_mut() {
            arr.extend(writes::write_tool_descriptors());
        }
    }
    // Annotate here rather than at each descriptor, so the
    // classification lives in one table a guard can check against the
    // advertised tools. Inline hints would drift the moment someone
    // added a tool by copying its neighbour.
    super::annotations::annotate(&mut tools);
    tools
}

fn read_tool_table() -> Value {
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
            "name": "worker_queues",
            "description": "Worker-tier SQS queue state for one environment: depth on the main queue and the dead-letter queue, and — with `peek` — which scheduled task dead-lettered. This is the answer to EB's \"1 message in Dead Letter Queue\" health text, which names no task. CAVEATS: web-tier envs have no queues and return empty, not an error. `dlq_origin` says whether EB NAMED the dead-letter queue (`reported`) or ebman derived it by the `<main>-dlq` naming convention (`derived`) — a derived URL that returns nothing is the ordinary case for an env with no DLQ, while a reported one that does is a real anomaly. A `peek` is non-destructive (messages are never deleted and return to the queue) BUT it increments each returned message's `receive_count` by one per call: that field counts every receive, including this tool's, so it is NOT a retry count and must not be read as one.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "env": {"type": "string", "description": "Environment name (required)"},
                    "peek": {"type": "boolean", "description": "Also read dead-letter messages and their `beanstalk.sqsd.*` task attributes. Default false — depth alone touches nothing."},
                    "max": {"type": "integer", "description": "Max messages to peek (default 10)"},
                    "profile": {"type": "string", "description": "AWS profile (default: ambient)"},
                    "region": {"type": "string", "description": "AWS region (default: profile/env default)"}
                },
                "required": ["env"]
            }
        },
        {
            "name": "recent_logs",
            "description": "The NEWEST log lines for an environment from CloudWatch Logs. CAVEATS: returns the newest in the window, not the oldest — `FilterLogEvents` itself returns matches oldest-first, so a naive query answers \"is this still running?\" with lines from hours ago and looks plausible doing it. If `complete` is false the window held more than could be read and what you have is the OLDEST part of it: narrow `since_minutes` rather than trusting the result. `log_group` defaults to the environment's own groups (`/aws/elasticbeanstalk/<env>/…`); if the env has none, the result says so rather than erroring. `filter` is CloudWatch Logs filter-pattern syntax, not a regex.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "env": {"type": "string", "description": "Environment name (required)"},
                    "since_minutes": {"type": "integer", "description": "How far back to look (default 60)"},
                    "limit": {"type": "integer", "description": "Max lines to return, newest last (default 50, max 1000)"},
                    "filter": {"type": "string", "description": "CloudWatch Logs filter pattern, e.g. ERROR"},
                    "log_group": {"type": "string", "description": "One specific group; default is every group for the env"},
                    "profile": {"type": "string", "description": "AWS profile (default: ambient)"},
                    "region": {"type": "string", "description": "AWS region (default: profile/env default)"}
                },
                "required": ["env"]
            }
        },
        {
            "name": "lint",
            "description": "Run ebman's diagnostic rule engine over the fleet (or one env). CAVEATS: EBL011 (worker DLQ) never fires here — the lint path does not poll queues; call `worker_queues` for depth, and with `peek` for which task dead-lettered; EBL016 (live health probe) does not run in this tool. A clean result does NOT clear those rules. EBL015 (stale custom platforms, account-level) runs only when not scoped to a single env. Envs whose input fetch fails are skipped, not fatal — a `skipped_envs` array in the result lists them, so check it before treating the run as full coverage.",
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

/// Helper: string arg off a tools/call `arguments` object.
pub(super) fn arg_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

pub(super) fn arg_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(Value::as_u64)
}

/// Render a `recent_logs` answer.
///
/// `complete` sits at the top level rather than beside the events
/// because it changes how the whole array should be read: false means
/// these are the OLDEST lines in the window, not the newest, which is
/// the opposite of what the tool is for.
fn render_recent_logs_json(
    env: &str,
    groups: &[String],
    complete: bool,
    events: &[(String, crate::aws::LogEvent)],
) -> String {
    let esc = crate::util::json_escape;
    let gs: Vec<String> = groups.iter().map(|g| format!("\"{}\"", esc(g))).collect();
    let evs: Vec<String> = events
        .iter()
        .map(|(group, e)| {
            let ts = chrono::DateTime::from_timestamp_millis(e.timestamp_ms)
                .map(|d| d.to_rfc3339())
                .unwrap_or_default();
            format!(
                "{{\"timestamp\":\"{}\",\"group\":\"{}\",\"stream\":\"{}\",\"message\":\"{}\"}}",
                esc(&ts),
                esc(group),
                esc(&e.stream),
                esc(&e.message)
            )
        })
        .collect();
    format!(
        "{{\"env\":\"{}\",\"groups\":[{}],\"complete\":{},\"events\":[{}]}}",
        esc(env),
        gs.join(","),
        complete,
        evs.join(",")
    )
}

/// Render worker queue state as JSON.
///
/// `peeked` is reported explicitly so a consumer can tell "no messages
/// in the dead-letter queue" from "we did not look" — the two are the
/// same empty array otherwise, and they mean opposite things during
/// triage.
fn render_worker_queues_json(
    queues: &aws::WorkerQueues,
    messages: &[aws::QueueMessage],
    peeked: bool,
) -> String {
    let stats = |s: &Option<aws::QueueStats>| match s {
        Some(s) => format!(
            "{{\"visible\":{},\"in_flight\":{},\"delayed\":{}}}",
            s.visible, s.in_flight, s.delayed
        ),
        None => "null".to_string(),
    };
    let url = |u: &Option<String>| match u {
        Some(u) => format!("\"{}\"", crate::util::json_escape(u)),
        None => "null".to_string(),
    };
    let origin = match queues.dlq_origin {
        Some(aws::DlqOrigin::Reported) => "\"reported\"",
        Some(aws::DlqOrigin::Derived) => "\"derived\"",
        None => "null",
    };
    let msgs: Vec<String> = messages
        .iter()
        .map(|m| {
            let task = match &m.task {
                Some(t) => {
                    let f = |v: &Option<String>| match v {
                        Some(v) => format!("\"{}\"", crate::util::json_escape(v)),
                        None => "null".to_string(),
                    };
                    format!(
                        "{{\"name\":{},\"path\":{},\"scheduled_time\":{}}}",
                        f(&t.name),
                        f(&t.path),
                        f(&t.scheduled_time_raw)
                    )
                }
                None => "null".to_string(),
            };
            let sent = match m.sent_at {
                Some(t) => format!("\"{}\"", crate::util::json_escape(&t.to_rfc3339())),
                None => "null".to_string(),
            };
            format!(
                "{{\"id\":\"{}\",\"sent_at\":{},\"receive_count\":{},\"task\":{},\"body\":\"{}\"}}",
                crate::util::json_escape(&m.id),
                sent,
                m.receive_count,
                task,
                crate::util::json_escape(&m.body)
            )
        })
        .collect();
    format!(
        "{{\"main_queue\":{{\"url\":{},\"stats\":{}}},\"dead_letter_queue\":{{\"url\":{},\"stats\":{},\"origin\":{}}},\"peeked\":{},\"messages\":[{}]}}",
        url(&queues.main_url),
        stats(&queues.main_stats),
        url(&queues.dlq_url),
        stats(&queues.dlq_stats),
        origin,
        peeked,
        msgs.join(",")
    )
}

impl Server {
    /// Build the per-call AWS client. Errors go through the shared
    /// credential rewrite so an expired SSO token surfaces as the
    /// `aws sso login` hint the agent can relay, not SDK noise.
    pub(super) async fn client(&self, args: &Value) -> Result<aws::AwsClient, String> {
        let profile = arg_str(args, "profile");
        let region = arg_str(args, "region");
        aws::AwsClient::with(profile.clone(), region)
            .await
            .map_err(|e| tool_error(&profile, "AwsClient", &e.to_string()))
    }

    pub(super) async fn fetch_envs(&self, args: &Value) -> Result<Vec<aws::Environment>, String> {
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

    pub(super) async fn call_tool(&self, name: &str, args: &Value) -> Result<String, String> {
        match name {
            "list_environments" => {
                let envs = self.fetch_envs(args).await?;
                Ok(crate::cli::envs::render_envs_json(&envs))
            }
            "worker_queues" => self.tool_worker_queues(args).await,
            "recent_logs" => self.tool_recent_logs(args).await,
            "lint" => self.tool_lint(args).await,
            "get_option_settings" => self.tool_option_settings(args).await,
            "drift" => self.tool_drift(args).await,
            "audit_log" => self.tool_audit_log(args),
            "recent_events" => self.tool_recent_events(args).await,
            "list_versions" => self.tool_list_versions(args).await,
            "fleet_cost" => self.tool_fleet_cost(args).await,
            // Write surface (only reachable under --allow-writes — the
            // RPC layer gates the table on it). Phase 1 verbs plan;
            // confirm_action dispatches.
            "deploy" => self.tool_write_plan(writes::WriteVerb::Deploy, args).await,
            "restart" => self.tool_write_plan(writes::WriteVerb::Restart, args).await,
            "rebuild" => self.tool_write_plan(writes::WriteVerb::Rebuild, args).await,
            "terminate" => {
                self.tool_write_plan(writes::WriteVerb::Terminate, args)
                    .await
            }
            "set_option" => {
                self.tool_write_plan(writes::WriteVerb::SetOption, args)
                    .await
            }
            "confirm_action" => self.tool_confirm_action(args).await,
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
            .map(|v| crate::util::split_csv(&v))
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
                    fetches.push(fetch_env_lint_inputs(
                        &client,
                        env,
                        &latest_stacks,
                        false,
                        &disabled,
                    ));
                }
                let fetched: Vec<Result<EnvLintInputs, String>> = futures::stream::iter(fetches)
                    .buffered(FETCH_CONCURRENCY)
                    .collect()
                    .await;
                for (env, inputs) in targets.iter().zip(fetched) {
                    match inputs {
                        Ok(inputs) => {
                            // A probe that could not run is not a clean
                            // result, and this tool's output is
                            // something an agent treats as
                            // authoritative. The CLI reports these on
                            // stderr; here they belong in `skipped_envs`
                            // for the same reason the fetch failures do
                            // — the agent cannot otherwise know that
                            // EBL018/EBL020 coverage shrank.
                            skipped.extend(inputs.coverage_warnings.iter().cloned());
                            all_issues.extend(run_rules_for_env(
                                &rules,
                                env,
                                &inputs,
                                &required_tags,
                            ));
                        }
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

    /// The newest log lines for an env — the "has it recovered?" read.
    ///
    /// Reports `complete` because the failure mode here is a plausible
    /// wrong answer rather than an error: a truncated window hands back
    /// the OLDEST lines in it, which reads as "the task stopped running
    /// hours ago" for a task that is running fine.
    async fn tool_recent_logs(&self, args: &Value) -> Result<String, String> {
        let env_name = arg_str(args, "env").ok_or("'env' is required")?;
        let since_minutes = arg_u64(args, "since_minutes")
            .unwrap_or(60)
            .clamp(1, 10_080);
        let limit = arg_u64(args, "limit").unwrap_or(50).clamp(1, 1000) as usize;
        let filter = arg_str(args, "filter");

        if matches!(self.backend, Backend::Demo) {
            return Ok(format!(
                "{{\"env\":\"{}\",\"groups\":[],\"complete\":true,\"events\":[],\"note\":\"demo mode reads no logs\"}}",
                crate::util::json_escape(&env_name)
            ));
        }

        let client = self.client(args).await?;
        let profile = arg_str(args, "profile");
        let groups = match arg_str(args, "log_group") {
            Some(g) => vec![g],
            None => client
                .discover_env_log_groups(&env_name)
                .await
                .map_err(|e| tool_error(&profile, "discover_env_log_groups", &e.to_string()))?,
        };

        let since_ms = (chrono::Utc::now() - chrono::Duration::minutes(since_minutes as i64))
            .timestamp_millis();
        let mut events: Vec<(String, crate::aws::LogEvent)> = Vec::new();
        let mut complete = true;
        for g in &groups {
            let (evs, done) = client
                .fetch_latest_log_events(g, since_ms, limit, filter.as_deref())
                .await
                .map_err(|e| tool_error(&profile, "fetch_latest_log_events", &e.to_string()))?;
            // One incomplete group makes the whole answer incomplete —
            // a consumer cannot act on "some of this is the oldest part
            // of the window" per group.
            complete &= done;
            events.extend(evs.into_iter().map(|e| (g.clone(), e)));
        }
        // Merge across groups, then keep the newest `limit` overall:
        // each group was capped independently, so the union can exceed
        // it and would otherwise be ordered by group rather than time.
        events.sort_by_key(|(_, e)| e.timestamp_ms);
        if events.len() > limit {
            events.drain(0..events.len() - limit);
        }
        Ok(render_recent_logs_json(
            &env_name, &groups, complete, &events,
        ))
    }

    /// Worker queue state for one env — the read that turned EB's
    /// "1 message in Dead Letter Queue" into a named task.
    ///
    /// Depth always; messages only on `peek`, because a peek increments
    /// each returned message's receive count and the default path
    /// should touch nothing.
    async fn tool_worker_queues(&self, args: &Value) -> Result<String, String> {
        let env_name = arg_str(args, "env").ok_or("'env' is required")?;
        let peek = args.get("peek").and_then(Value::as_bool).unwrap_or(false);
        let max = arg_u64(args, "max")
            .map(|m| i32::try_from(m.min(100)).unwrap_or(10))
            .unwrap_or(10)
            .max(1);

        let envs = self.fetch_envs(args).await?;
        let env = envs
            .iter()
            .find(|e| e.name == env_name)
            .ok_or_else(|| format!("env '{env_name}' not found"))?;

        if matches!(self.backend, Backend::Demo) {
            // Demo never peeks: the fixture's queues are synthetic and
            // there is no SQS behind them. `peeked` is reported as asked
            // so the shape matches the live path.
            let queues = demo_fixture::worker_queues_for_env(&env_name);
            return Ok(render_worker_queues_json(&queues, &[], peek));
        }

        let client = self.client(args).await?;
        let queues = client
            .describe_worker_queues(&env.application, &env_name)
            .await
            .map_err(|e| {
                tool_error(
                    &arg_str(args, "profile"),
                    "describe_worker_queues",
                    &e.to_string(),
                )
            })?;

        let messages = match (peek, queues.dlq_url.as_deref()) {
            (true, Some(url)) => client.peek_messages(url, max).await.map_err(|e| {
                tool_error(&arg_str(args, "profile"), "peek_messages", &e.to_string())
            })?,
            _ => Vec::new(),
        };
        Ok(render_worker_queues_json(&queues, &messages, peek))
    }

    async fn tool_recent_events(&self, args: &Value) -> Result<String, String> {
        // Clamp in u64 first — an `as i32` cast bit-truncates, so
        // max=2^32+5 used to mean 5, not the cap.
        let max = arg_u64(args, "max")
            .map(|m| i32::try_from(m.min(EVENTS_MAX_MAX as u64)).unwrap_or(EVENTS_MAX_MAX))
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
pub(super) fn tool_error(profile: &Option<String>, op: &str, msg: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Names of the `match name` arms in `call_tool`, read from source.
    ///
    /// The descriptors are data and the handlers are code, so nothing
    /// makes them agree — the same gap `src/commands.rs` closes for the
    /// TUI registry with a test rather than a restructure. A descriptor
    /// with no arm is a tool an agent calls and gets nothing from; an
    /// arm with no descriptor is dead, because `tools/call` refuses any
    /// name absent from the table.
    fn dispatch_arm_names() -> Vec<String> {
        let src = include_str!("tools.rs");
        let start = src.find("async fn call_tool").expect("call_tool exists");
        let body = &src[start..];
        let end = body.find("\n    }\n").unwrap_or(body.len());
        body[..end]
            .lines()
            .filter_map(|l| {
                let rest = l.trim().strip_prefix('"')?;
                let (name, after) = rest.split_once('"')?;
                after
                    .trim_start()
                    .starts_with("=>")
                    .then(|| name.to_string())
            })
            .collect()
    }

    fn names_in(table: &Value) -> Vec<String> {
        table
            .as_array()
            .expect("tool table is an array")
            .iter()
            .map(|d| {
                d["name"]
                    .as_str()
                    .expect("every tool has a name")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn every_advertised_tool_has_a_handler_and_vice_versa() {
        let arms = dispatch_arm_names();
        assert!(
            arms.len() >= 10,
            "the source scan found only {}",
            arms.len()
        );
        let advertised = names_in(&tool_table(true));

        let mut missing: Vec<&String> = advertised.iter().filter(|n| !arms.contains(n)).collect();
        missing.sort();
        assert!(
            missing.is_empty(),
            "advertised in tools/list with no handler — an agent calls these \
             and gets nothing back: {missing:?}"
        );

        let mut dead: Vec<&String> = arms.iter().filter(|n| !advertised.contains(n)).collect();
        dead.sort();
        assert!(
            dead.is_empty(),
            "handled but never advertised — `tools/call` refuses names absent \
             from the table, so these are unreachable: {dead:?}"
        );
    }

    #[test]
    fn no_write_tool_is_advertised_without_allow_writes() {
        // The membership check in `mod.rs` makes the table the authority
        // on what can be called at all, so a write tool leaking into the
        // read-only table opens a write surface — not a listing cosmetic.
        let read_only = names_in(&tool_table(false));
        let writes: Vec<String> = super::super::writes::write_tool_descriptors()
            .iter()
            .map(|d| d["name"].as_str().expect("name").to_string())
            .collect();
        assert!(!writes.is_empty(), "there are write tools to check");
        for w in &writes {
            assert!(
                !read_only.contains(w),
                "{w} is advertised with --allow-writes off"
            );
        }
        let enabled = names_in(&tool_table(true));
        for w in &writes {
            assert!(enabled.contains(w), "{w} missing under --allow-writes");
        }
    }
}
