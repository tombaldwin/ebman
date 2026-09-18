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

/// Rules that CANNOT fire over MCP, attached to every lint result.
///
/// The description already says so, and that was not enough. Linting an
/// environment that is Yellow BECAUSE of a dead-lettered message
/// returns unrelated warnings and nothing about the queue — so a reader
/// who did not re-read the description sees two findings and concludes
/// lint is happy with it. Observed against a live fleet, on exactly
/// that environment.
///
/// In the RESULT, next to the findings, the way `skipped_envs` already
/// reports degraded coverage: a caveat an agent has to go back and look
/// up is a caveat that gets skipped.
pub(super) fn append_cannot_fire(body: String) -> String {
    append_string_array(
        body,
        "rules_not_checked",
        &[
            "EBL011 (worker dead-letter queue) — call `worker_queues`, \
             with `peek` for which task dead-lettered"
                .to_string(),
            "EBL016 (live health probe) — not run by this tool".to_string(),
        ],
    )
}

/// The static tool table. Descriptions carry the coverage caveats —
/// an agent treats "no findings" as authoritative, so a wiring gap
/// (EBL011/016/020 can't fire here) must be stated IN the tool.
pub(super) fn tool_table(scope: &super::WriteScope) -> Value {
    let mut tools = read_tool_table();
    if scope.any() {
        if let Some(arr) = tools.as_array_mut() {
            // Only the scoped verbs are advertised. A verb outside the
            // scope is absent rather than present-and-refused: a client
            // that cannot see a tool will not plan around it, and the
            // dispatch gate still refuses it in case one cached an
            // older list.
            arr.extend(writes::write_tool_descriptors().into_iter().filter(|d| {
                d.get("name").and_then(|n| n.as_str()).is_some_and(|n| {
                    // `confirm_action` rides along with any grant —
                    // it is the second phase of every write rather
                    // than a verb of its own, so filtering it out
                    // would leave a narrow grant able to plan a write
                    // and never dispatch it.
                    n == writes::CONFIRM_TOOL || scope.allows(n)
                })
            }));
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
            "description": "List Elastic Beanstalk environments (name, application, tier, status, health, platform, cname, version_label, updated, region). Same schema as `ebman envs --json`.",
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
            "description": "Worker-tier SQS queue state for one environment: depth on the main queue and the dead-letter queue, and — with `peek` — which scheduled task dead-lettered. NOT REDACTED under `peek`: each message's body is returned verbatim, and a worker queue carries whatever your application POSTed to it — ebman's redaction is namespace-and-key based and cannot touch free text. This is the answer to EB's \"1 message in Dead Letter Queue\" health text, which names no task. CAVEATS: web-tier envs have no queues and return empty, not an error. `dead_letter_queue.origin` says whether EB NAMED the dead-letter queue (`reported`) or ebman derived it by the `<main>-dlq` naming convention (`derived`) — a derived URL that returns nothing is the ordinary case for an env with no DLQ, while a reported one that does is a real anomaly. A `peek` is non-destructive (messages are never deleted and return to the queue) BUT it increments each returned message's `receive_count` by one per call: that field counts every receive, including this tool's, so it is NOT a retry count and must not be read as one.",
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
            "description": "The NEWEST log lines for an environment from CloudWatch Logs. NOT REDACTED: log lines are free text and this tool returns them verbatim, so anything an application logged — tokens, connection strings, customer data — reaches the client. ebman's redaction is namespace-and-key based (`get_option_settings`, `drift`, `audit_log`) and cannot apply here; use `filter` to narrow what you pull rather than relying on it being scrubbed. CAVEATS: returns the newest in the window, not the oldest — `FilterLogEvents` itself returns matches oldest-first, so a naive query answers \"is this still running?\" with lines from hours ago and looks plausible doing it. If `complete` is false the window held more than could be read and what you have is the OLDEST part of it: narrow `since_minutes` rather than trusting the result. `log_group` defaults to the environment's own groups (`/aws/elasticbeanstalk/<env>/…`); if the env has none, the result says so rather than erroring. `filter` is CloudWatch Logs filter-pattern syntax, not a regex.",
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
            "name": "why",
            "description": "Everything that bears on one environment's health, assembled in a single call: recent events, alarms, instances, dead-letter queue depth and its messages, and the application's recent versions. NOT REDACTED: this peeks the dead-letter queue automatically (up to 5 messages, no opt-in) and returns each body verbatim — a worker queue carries whatever your application POSTed to it. This is the TUI's `:why` overlay. Deliberately NOT a narrative — it puts the facts side by side and leaves the conclusion to the reader, because a confident wrong story is harder to disagree with than adjacent facts. CAVEATS: the dead-letter peek increments each returned message's `receive_count`, which counts every receive and is not a retry count. Any section that failed to fetch comes back as null with the reason in `errors`, so a partial answer is visible as partial rather than reading as \"nothing there\".",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "env": {"type": "string", "description": "Environment name (required)"},
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
            "description": "Terraform drift report: live env config vs the tfstate's recorded settings. State resolution: the `tfstate_path` argument, then `terraform.state_path` in config.toml, then discovery from the SERVER's working directory (correct for project-scoped .mcp.json which launches in the repo). For a fleet whose state is in a remote backend (HCP, S3, Consul), `terraform state pull > state.json` and set the config key — ebman reads state files and does not talk to backends. The report carries a `state` block (`serial`, `lineage`, `pulled_at`): a pulled file goes stale silently, and ebman cannot tell whether its serial is current, so it names the one it compared. Drifted env-var values and DBPassword are redacted like get_option_settings (the drifted signal survives; --no-redact disables).",
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

/// Turn a dead-letter peek result into `(messages, peeked)`, recording
/// a failure rather than swallowing it.
///
/// The first version was `peek_messages(...).unwrap_or_default()` with
/// `peeked = dlq_url.is_some()`, which reported `peeked: true,
/// messages: []` when the peek FAILED — "we looked, there is nothing
/// there" for a queue we were denied. Reading messages needs
/// `sqs:ReceiveMessage`, a different permission from the attributes
/// call that produced the depth, so this is an ordinary IAM shape and
/// not a corner case.
///
/// It is also the exact distinction `peeked` and `errors` exist to
/// preserve, destroyed by an `unwrap_or_default` at the one call site —
/// which is why CLAUDE.md says to grep for those after widening a type.
pub(super) fn dlq_peek_outcome(
    peek: Option<std::result::Result<Vec<crate::aws::QueueMessage>, String>>,
    errors: &mut Vec<(String, String)>,
) -> (Vec<crate::aws::QueueMessage>, bool) {
    match peek {
        // No dead-letter queue: nothing to look at, and we did not look.
        None => (Vec::new(), false),
        Some(Ok(msgs)) => (msgs, true),
        Some(Err(e)) => {
            errors.push(("dlq_peek".to_string(), e));
            (Vec::new(), false)
        }
    }
}

/// A `why` section's rendered JSON, or `null` with the reason recorded.
///
/// Extracted from `tool_why`'s closure so it is reachable: the closure
/// needs AWS, and mutating it to return `[]` instead of `null` — or to
/// swallow the error entirely — left the whole suite green. Both are
/// the same defect, which is that "we could not look" starts reading as
/// "there is nothing there", and during triage those are opposite
/// conclusions.
pub(super) fn section_or_error(
    name: &str,
    r: std::result::Result<String, String>,
    errors: &mut Vec<(String, String)>,
) -> String {
    match r {
        Ok(v) => v,
        Err(e) => {
            errors.push((name.to_string(), e));
            "null".to_string()
        }
    }
}

/// The `why` bundle. Sections are pre-rendered JSON so each can be
/// `null` independently — an unfetched section must never look like an
/// empty one.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_why_json(
    env: &str,
    events: &str,
    alarms: &str,
    instances: &str,
    queues: &str,
    versions: &str,
    errors: &[(String, String)],
) -> String {
    let errs: Vec<String> = errors
        .iter()
        .map(|(section, message)| {
            format!(
                "{{\"section\":{},\"error\":{}}}",
                util::json_string(section),
                util::json_string(message)
            )
        })
        .collect();
    format!(
        "{{\"env\":{},\"events\":{},\"alarms\":{},\"instances\":{},\"queues\":{},\"recent_versions\":{},\"errors\":[{}]}}",
        util::json_string(env),
        events,
        alarms,
        instances,
        queues,
        versions,
        errs.join(",")
    )
}

fn render_alarms_json(alarms: &[aws::CwAlarm]) -> String {
    let entries: Vec<String> = alarms
        .iter()
        .map(|a| {
            format!(
                "{{\"name\":{},\"state\":{},\"reason\":{},\"metric\":{},\"namespace\":{}}}",
                util::json_string(&a.name),
                util::json_string(&a.state),
                util::json_string(&a.state_reason),
                util::json_string(&a.metric_name),
                util::json_string(&a.namespace)
            )
        })
        .collect();
    format!("[{}]", entries.join(","))
}

fn render_instances_json(instances: &[aws::Instance]) -> String {
    let entries: Vec<String> = instances
        .iter()
        .map(|i| {
            let causes: Vec<String> = i.causes.iter().map(|c| util::json_string(c)).collect();
            format!(
                "{{\"id\":{},\"health\":{},\"color\":{},\"instance_type\":{},\"availability_zone\":{},\"launched_at\":{},\"causes\":[{}]}}",
                util::json_string(&i.id),
                util::json_string(&i.health),
                util::json_string(&i.color),
                util::json_string(&i.instance_type),
                util::json_string(&i.availability_zone),
                i.launched_at
                    .map(|t| util::json_string(&t.to_rfc3339()))
                    .unwrap_or_else(|| "null".into()),
                causes.join(",")
            )
        })
        .collect();
    format!("[{}]", entries.join(","))
}

fn render_versions_json(versions: &[aws::AppVersion]) -> String {
    let entries: Vec<String> = versions
        .iter()
        .take(10)
        .map(|v| {
            format!(
                "{{\"label\":{},\"description\":{},\"created\":{}}}",
                util::json_string(&v.label),
                util::json_string(&v.description),
                v.created
                    .map(|t| util::json_string(&t.to_rfc3339()))
                    .unwrap_or_else(|| "null".into())
            )
        })
        .collect();
    format!("[{}]", entries.join(","))
}

/// Render events as JSON. Shared by `recent_events` and `why`: a second
/// copy is how two surfaces start disagreeing about the same records.
fn render_events_json(events: &[aws::Event]) -> String {
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
    format!("[{}]", entries.join(","))
}

/// Cap how many log groups one `recent_logs` call walks, and say
/// whether anything was dropped.
///
/// Bounded like the drift and lint fan-outs, for the same reason: the
/// loop is sequential and each group costs up to 20 paged calls, so an
/// env with many groups walks past the 30s tool timeout and the client
/// sees a dead tool rather than a partial answer.
///
/// A truncated group list makes the answer incomplete, because the
/// dropped groups might hold the newest lines — the same instruction
/// `complete: false` already carries. An EXPLICIT `log_group` is never
/// truncated: the caller named one, so there is nothing to drop.
fn cap_log_groups(groups: Vec<String>, explicit: bool) -> (Vec<String>, bool) {
    const MAX_GROUPS: usize = 8;
    let dropped = groups.len() > MAX_GROUPS;
    let kept: Vec<String> = groups.into_iter().take(MAX_GROUPS).collect();
    (kept, explicit || !dropped)
}

/// Merge per-group results into one newest-last, `limit`-capped list.
///
/// Each group is fetched and capped independently, so the union can
/// exceed `limit` AND arrives ordered by group rather than by time — a
/// reader scanning the tail of the array would see the last group's
/// oldest lines rather than the fleet's newest. Extracted because it
/// sits in an AWS-only path: mutating away the sort, and the trim, both
/// left the whole suite green.
fn merge_newest(events: &mut Vec<(String, crate::aws::LogEvent)>, limit: usize) {
    events.sort_by_key(|(_, e)| e.timestamp_ms);
    if events.len() > limit {
        events.drain(0..events.len() - limit);
    }
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
    pub(super) async fn client(
        &self,
        args: &Value,
    ) -> Result<std::sync::Arc<aws::AwsClient>, String> {
        // An injected client short-circuits construction, so tool
        // ORCHESTRATION can be driven without ambient credentials —
        // which calls did this body make, with which arguments. The
        // layers under it are covered; this is the one that was not,
        // and it is where the dead-letter peek bug lived.
        // `Arc`, not the client itself: `AwsClient` is not `Clone`
        // (it holds SDK clients that are cheap to share but not to
        // duplicate), and every call site binds it and calls methods,
        // so an `Arc` is transparent to all fourteen of them.
        #[cfg(test)]
        if let Some(c) = &self.injected_client {
            return Ok(std::sync::Arc::clone(c));
        }
        let profile = arg_str(args, "profile");
        let region = arg_str(args, "region");
        aws::AwsClient::with(profile.clone(), region)
            .await
            .map(std::sync::Arc::new)
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
            "why" => self.tool_why(args).await,
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
            "dlq_resend" => {
                self.tool_write_plan(writes::WriteVerb::DlqResend, args)
                    .await
            }
            "dlq_delete" => {
                self.tool_write_plan(writes::WriteVerb::DlqDelete, args)
                    .await
            }
            "dlq_purge" => {
                self.tool_write_plan(writes::WriteVerb::DlqPurge, args)
                    .await
            }
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
            append_cannot_fire(append_skipped_envs(
                lint::render_issues_json(&all_issues),
                &skipped,
            )),
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
            return Ok(terraform::render_drift_json(None, None, &[]));
        }
        // Explicit argument, then `terraform.state_path`, then
        // discovery — the same order the CLI uses. Without the config
        // rung a fleet on a remote backend has no drift at all over
        // MCP, because discovery only ever finds a local file.
        let explicit = arg_str(args, "tfstate_path").map(std::path::PathBuf::from);
        // The ABSOLUTE cwd, not `"."`. `Path::new(".").ancestors()`
        // yields exactly `"."` and `""` — so discovery checked the
        // server's own directory and nothing above it, while the tool
        // description said it walks up from there. A project-scoped
        // `.mcp.json` launches in the repo root and usually got away
        // with it; a server started one directory down silently found
        // nothing. `load_from_cwd` and the CLI both canonicalise first.
        let start = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let path = terraform::resolve_state_path(
            explicit.as_deref(),
            self.safety_cfg.terraform_state_path.as_deref(),
            &start,
        )
        .ok_or_else(|| terraform::no_state_hint("tfstate_path"))?;
        let state = terraform::load_from_path(&path)
            .ok_or_else(|| format!("could not parse tfstate at '{}'", path.display()))?;
        let (state, used_path) = (state, Some(path));
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
            terraform::render_drift_json(
                used_path.as_deref(),
                Some(&terraform::StateProvenance::of(
                    &state,
                    used_path.as_deref(),
                )),
                &reports,
            ),
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

    /// The `:why` bundle: every fact bearing on one env's health, in
    /// one call.
    ///
    /// Six fetches that an operator otherwise makes by hand. Each is
    /// independent and each can fail on its own — a section that failed
    /// is `null` with its reason in `errors`, never an empty array,
    /// because "we could not look" and "there is nothing there" are
    /// opposite conclusions during triage.
    ///
    /// No narrative. The facts go side by side and the reader draws the
    /// conclusion: a generated sentence is confidently wrong in a way
    /// adjacent facts are not.
    async fn tool_why(&self, args: &Value) -> Result<String, String> {
        let env_name = arg_str(args, "env").ok_or("'env' is required")?;
        let envs = self.fetch_envs(args).await?;
        let env = envs
            .iter()
            .find(|e| e.name == env_name)
            .ok_or_else(|| format!("env '{env_name}' not found"))?;
        let app = env.application.clone();

        if matches!(self.backend, Backend::Demo) {
            let queues = demo_fixture::worker_queues_for_env(&env_name);
            let events = demo_fixture::events_for_env(&env_name);
            return Ok(render_why_json(
                &env_name,
                &render_events_json(&events),
                "null",
                "null",
                &render_worker_queues_json(&queues, &[], false),
                "null",
                &[],
            ));
        }

        let client = self.client(args).await?;
        let mut errors: Vec<(String, String)> = Vec::new();
        // Each section records its own failure rather than aborting the
        // bundle: five good sections and one error is a far more useful
        // answer than one error.
        let mut section = |name: &str, r: std::result::Result<String, String>| -> String {
            section_or_error(name, r, &mut errors)
        };

        let events = section(
            "events",
            client
                .list_events_for_env(&env_name, 50)
                .await
                .map(|e| render_events_json(&e))
                .map_err(|e| e.to_string()),
        );
        let alarms = section(
            "alarms",
            client
                .list_alarms_for_env(&env_name, &self.safety_cfg.alarm_dimensions)
                .await
                .map(|a| render_alarms_json(&a))
                .map_err(|e| e.to_string()),
        );
        let instances = section(
            "instances",
            client
                .list_instances(&env_name)
                .await
                .map(|i| render_instances_json(&i))
                .map_err(|e| e.to_string()),
        );
        let versions = section(
            "recent_versions",
            client
                .list_application_versions(&app)
                .await
                .map(|v| render_versions_json(&v))
                .map_err(|e| e.to_string()),
        );
        let queues = match client.describe_worker_queues(&app, &env_name).await {
            Ok(q) => {
                // Same gate as `worker_queues`: without it, every
                // healthy worker env whose derived DLQ guess missed
                // recorded a spurious "we could not look" in `errors`,
                // which is triage noise in the array that exists to
                // make partial answers load-bearing.
                let peek = match q.dlq_url.as_deref().filter(|_| q.dlq_stats.is_some()) {
                    Some(url) => Some(
                        client
                            .peek_messages(url, 5)
                            .await
                            .map_err(|e| e.to_string()),
                    ),
                    None => None,
                };
                let (msgs, peeked) = dlq_peek_outcome(peek, &mut errors);
                render_worker_queues_json(&q, &msgs, peeked)
            }
            Err(e) => {
                errors.push(("queues".into(), e.to_string()));
                "null".to_string()
            }
        };

        Ok(render_why_json(
            &env_name, &events, &alarms, &instances, &queues, &versions, &errors,
        ))
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
        // Bounded like the drift and lint fan-outs, and for the same
        // reason: this loop is sequential and each group costs up to 20
        // paged calls, so an env with many log groups walks straight
        // past the 30s tool timeout and the client sees a dead tool
        // rather than a partial answer. Truncating is reported, not
        // hidden — `complete: false` already means "narrow the window",
        // and a dropped group is the same instruction.
        let (groups, mut complete) = cap_log_groups(groups, arg_str(args, "log_group").is_some());
        let mut events: Vec<(String, crate::aws::LogEvent)> = Vec::new();
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
        merge_newest(&mut events, limit);
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
            // there is no SQS behind them. So `peeked` is FALSE even
            // when a peek was asked for — it reports whether we looked,
            // not what was requested.
            //
            // It passed `peek` through, for shape parity with the live
            // path. Shape parity does not need the boolean to lie: a
            // demo peek of `poly-batch` answered `peeked: true,
            // messages: []` beside `visible: 12`, which reads as "the
            // dead-letter queue is empty" — a false all-clear, next to
            // the depth that contradicts it. This is the third site of
            // the same defect; `dlq_peek_outcome` and the live path
            // above were each fixed for it already.
            let queues = demo_fixture::worker_queues_for_env(&env_name);
            return Ok(render_worker_queues_json(&queues, &[], false));
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

        // Gated on `dlq_stats`, not on `dlq_url`. A DERIVED url — one
        // ebman guessed by the `<main>-dlq` convention — routinely names
        // a queue that does not exist, and `describe_worker_queues`
        // treats that as THE genuine "this env has no dead-letter
        // queue" shape: it swallows NonExistentQueue and leaves
        // `dlq_stats: None` with `dlq_url: Some`. Peeking that url
        // raises NonExistentQueue again, and the `?` here failed the
        // WHOLE call — throwing away the depth answer we already had,
        // on the case the tool's own description calls ordinary.
        let peekable = queues.dlq_stats.is_some();
        let messages = match (peek && peekable, queues.dlq_url.as_deref()) {
            (true, Some(url)) => client.peek_messages(url, max).await.map_err(|e| {
                tool_error(&arg_str(args, "profile"), "peek_messages", &e.to_string())
            })?,
            _ => Vec::new(),
        };
        // `peeked` reports whether we LOOKED, not what was asked for.
        // Passing the request flag through said "we looked, it was
        // empty" for a queue that does not exist — the exact
        // distinction this field carries, and `why` already answered it
        // the other way for the same env.
        Ok(render_worker_queues_json(
            &queues,
            &messages,
            peek && peekable,
        ))
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
        Ok(render_events_json(&events))
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
        let advertised = names_in(&tool_table(&crate::cli::mcp::WriteScope::All));

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
        let read_only = names_in(&tool_table(&crate::cli::mcp::WriteScope::None));
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
        let enabled = names_in(&tool_table(&crate::cli::mcp::WriteScope::All));
        for w in &writes {
            assert!(enabled.contains(w), "{w} missing under --allow-writes");
        }
    }
}

#[cfg(test)]
mod renderer_tests {
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
        let v = parse(&render_recent_logs_json("api-prod", &[], true, &events));
        assert_eq!(v["env"], "api-prod");
        assert_eq!(v["complete"], true);
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

    /// A demo peek must not claim to have looked.
    ///
    /// Demo has no SQS behind it, so it never peeks — but it passed the
    /// REQUEST flag through as the answer. `poly-batch` came back
    /// `peeked: true, messages: []` beside `visible: 12`: an explicit
    /// all-clear on a dead-letter queue the server never opened, next
    /// to the depth that contradicts it. An agent triaging that env
    /// reads "the DLQ is empty" and stops.
    ///
    /// The third site of one defect — `dlq_peek_outcome` and the live
    /// path were each fixed for it — and the only one with no test,
    /// which is why it survived both fixes.
    #[tokio::test]
    async fn a_demo_peek_does_not_claim_to_have_looked() {
        let s = Server::with_scope(true, false, crate::cli::mcp::WriteScope::None);
        let body = s
            .tool_worker_queues(&json!({"env": "poly-batch", "peek": true}))
            .await
            .expect("demo worker_queues");
        let v: Value = serde_json::from_str(&body).expect("json");

        assert_eq!(
            v["peeked"], false,
            "demo never opens the queue, so it must not report that it did: {v}"
        );
        // The fixture that makes the lie visible: a non-empty DLQ.
        assert!(
            v["dead_letter_queue"]["stats"]["visible"]
                .as_u64()
                .is_some_and(|n| n > 0),
            "this test needs a non-empty demo DLQ to be meaningful: {v}"
        );
        assert_eq!(v["messages"].as_array().map(Vec::len), Some(0), "{v}");
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
        let v = parse(&render_worker_queues_json(&queues, &msgs, true));

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
            receipt_handle: String::new(),
            body: "{}".into(),
            receive_count: 1,
            sent_at: None,
            task: None,
        }];
        let v = parse(&render_worker_queues_json(&queues, &plain, true));
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
        let v = parse(&render_worker_queues_json(&none, &[], false));
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
}
