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
pub(super) fn tool_table(scope: &super::WriteScope, peek_bodies: bool) -> Value {
    let mut tools = read_tool_table();
    if !peek_bodies {
        // The agent must be told the policy is ON, or a withheld body
        // reads as an empty one. The description is the only channel
        // that reaches it — a config key it cannot see, and a
        // placeholder it might not look at, are not a contract.
        note_withheld_bodies(&mut tools);
    }
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
                    n == writes::CONFIRM_TOOL || n == writes::UNDO_TOOL || scope.allows(n)
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

/// Tell the agent that `mcp.peek_bodies = false` is in force.
///
/// Appended to the description of every tool that can return a message
/// body. Without it the suppression is invisible at the point of use:
/// the operator sees the config, the agent sees a body that is not
/// there, and "withheld" and "empty" are the same observation.
fn note_withheld_bodies(tools: &mut Value) {
    const NOTE: &str = " BODIES ARE WITHHELD on this server (`mcp.peek_bodies = false`): each message's `body` is replaced with a marker naming that setting, NOT omitted, so a withheld body is never an empty one. The `beanstalk.sqsd.*` task fields are unaffected. A message posted by an application rather than by EB's scheduler carries no such fields, so for those this server can tell you a message dead-lettered and not what it was — say so rather than reporting the queue as uninformative.";
    let Some(arr) = tools.as_array_mut() else {
        return;
    };
    for t in arr.iter_mut() {
        // The two tools that can carry a body. Keyed by name rather
        // than by scanning descriptions, so adding a third is a
        // deliberate edit here and not an accident of wording.
        let carries_body = t
            .get("name")
            .and_then(|n| n.as_str())
            .is_some_and(|n| n == "worker_queues" || n == "why");
        if !carries_body {
            continue;
        }
        if let Some(d) = t.get_mut("description").and_then(|d| d.as_str()).map(|d| {
            let mut s = d.to_string();
            s.push_str(NOTE);
            s
        }) {
            t["description"] = json!(d);
        }
    }
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
            "description": "The NEWEST log lines for an environment from CloudWatch Logs. NOT REDACTED: log lines are free text and this tool returns them verbatim, so anything an application logged — tokens, connection strings, customer data — reaches the client. ebman's redaction is namespace-and-key based (`get_option_settings`, `drift`, `audit_log`) and cannot apply here; use `filter` to narrow what you pull rather than relying on it being scrubbed. CAVEATS: returns the newest in the window, not the oldest — `FilterLogEvents` itself returns matches oldest-first, so a naive query answers \"is this still running?\" with lines from hours ago and looks plausible doing it. If `complete` is false the SCAN stopped early and what you have is the OLDEST part of the window: narrow `since_minutes` rather than trusting the result. `complete` is about the scan, NOT about the result — `truncated_by_limit` is the other half, and says the window held more than `limit` so you have the newest slice of a larger set. Both can be true at once: a complete scan of two hours returning the newest 5 of thousands is `complete: true, truncated_by_limit: true`, and reading the first without the second gives you \"that is all there was\". When `complete` is FALSE and `truncated_by_limit` is true, what you hold is the newest slice of the OLDEST scanned prefix — a middle slice, not the newest overall; narrow the window before reading anything into the ordering. `log_group` defaults to the environment's own groups (`/aws/elasticbeanstalk/<env>/…`); if the env has none, the result says so rather than erroring. `filter` is CloudWatch Logs filter-pattern syntax, not a regex.",
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
            "description": "Run ebman's diagnostic rule engine over the fleet (or one env). CAVEATS: EBL011 (worker DLQ) never fires here — the lint path does not poll queues; call `worker_queues` for depth, and with `peek` for which task dead-lettered; EBL016 (live health probe) does not run in this tool. A clean result does NOT clear those rules. EBL015 (stale custom platforms, account-level) runs only when not scoped to a single env. Anything that could not be checked is skipped, not fatal, and listed in a `skipped_envs` array: an env whose inputs failed, a probe or input fetch that errored (e.g. EBL010 tags, EBL012 health), and a failed account-level pass (EBL008 stack listing, EBL015). Check it before treating the run as full coverage.",
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
            "name": "doctor",
            "description": "What THIS connection can and cannot do, and why. Reports the ebman build, what your client declared at handshake, the write surface in force, and the standing restrictions the operator has set. Call this before reporting a capability as missing: most of what looks like a gap in ebman is a client that does not carry a feature, or an operator who has forbidden something deliberately. Reads no AWS and takes no arguments.",
            "inputSchema": {"type": "object", "properties": {}}
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

/// The one place that decides whether a dead-letter queue can be
/// peeked, and at which URL.
///
/// Gated on `dlq_stats`, not on `dlq_url`. A DERIVED url — one ebman
/// guessed by the `<main>-dlq` convention — routinely names a queue
/// that does not exist, and `describe_worker_queues` treats that as
/// THE genuine "this env has no dead-letter queue" shape: it swallows
/// NonExistentQueue and leaves `dlq_stats: None` with `dlq_url: Some`.
/// Peeking that url raises NonExistentQueue again, which failed the
/// whole call in `worker_queues` — throwing away the depth answer
/// already in hand — and recorded a spurious "we could not look" in
/// `why`'s `errors` for every healthy worker env whose guess missed.
///
/// **Consolidated because it was written three times and missed
/// twice.** The gate was added to the live `worker_queues` path,
/// then found absent from `why`, then found absent from the demo path
/// three commits later — where it answered `peeked: true` for a web
/// env with no queue at all, on the path agents rehearse against. Each
/// copy carried a comment claiming to be "the same gate as" another
/// one, which is what a policy looks like shortly before it diverges.
///
/// Taking `requested` as well means the whole decision — may we, and
/// were we asked — is one value, so `peeked` cannot be computed from a
/// different expression than the one that chose the URL. That
/// divergence is precisely the `peeked: true, messages: []` defect.
pub(super) fn dlq_peek_target(queues: &crate::aws::WorkerQueues, requested: bool) -> Option<&str> {
    if !requested {
        return None;
    }
    answered_dlq_url(queues)
}

/// The URL of a dead-letter queue that actually answered.
///
/// The predicate itself, separate from the peek question, because
/// `writes.rs` needs the same one for a different purpose: before
/// planning a resend, delete or purge it must know there is a real
/// queue to act on, and it was asking with its own
/// `dlq_url.filter(|_| dlq_stats.is_some())` — a fourth copy of this
/// rule, found while consolidating the first three.
///
/// `dlq_stats: None` with `dlq_url: Some` is the ordinary shape for an
/// env with no dead-letter queue: ebman guessed the url from the
/// `<main>-dlq` convention and `describe_worker_queues` swallowed the
/// resulting NonExistentQueue. Acting on such a url — peeking it or
/// planning against it — raises that error again at a point where it
/// reads as a fault rather than as "there is no queue here".
pub(super) fn answered_dlq_url(queues: &crate::aws::WorkerQueues) -> Option<&str> {
    // The queue answered: `describe_worker_queues` got stats back for
    // it. Without this the url alone is only a guess ebman made.
    queues.dlq_stats.as_ref()?;
    queues.dlq_url.as_deref()
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
    truncated_by_limit: bool,
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
        "{{\"env\":\"{}\",\"groups\":[{}],\"complete\":{},\"truncated_by_limit\":{},\"events\":[{}]}}",
        esc(env),
        gs.join(","),
        complete,
        truncated_by_limit,
        evs.join(",")
    )
}

/// Render worker queue state as JSON.
///
/// `peeked` is reported explicitly so a consumer can tell "no messages
/// in the dead-letter queue" from "we did not look" — the two are the
/// same empty array otherwise, and they mean opposite things during
/// triage.
/// Why a queue answer is empty, when it is.
///
/// `None` when there is something to report — a reason beside real
/// data is noise, and noise is how the meaningful ones stop being
/// read.
fn empty_queue_reason(tier: &str, queues: &aws::WorkerQueues) -> Option<&'static str> {
    if queues.main_url.is_some() || queues.dlq_url.is_some() {
        return None;
    }
    // Three-way, not two. `tier` is "Web" / "Worker" / "?" — EB can
    // omit the tier block entirely, and an unrecognised name passes
    // through verbatim. Treating everything-not-Worker as web asserted
    // "there is nothing here to read" about an env whose tier ebman
    // does not know, which CLOSES the triage question with a claim it
    // cannot support: the same defect class this field exists to fix,
    // inverted for the third value.
    if tier.eq_ignore_ascii_case("Worker") {
        // A worker env SHOULD have a queue. EB reporting none is not
        // the ordinary case and should not read like one.
        Some(
            "worker tier, but EB reported no queues for this environment — unexpected; \
             check the environment's configuration",
        )
    } else if tier.eq_ignore_ascii_case("Web") || tier.eq_ignore_ascii_case("WebServer") {
        Some(
            "web tier — web environments have no worker queues, so there is nothing \
             here to read",
        )
    } else {
        Some(
            "the environment's tier could not be determined, so whether queues are \
             expected here is unknown — treat their absence as unconfirmed rather than \
             as an answer",
        )
    }
}

fn render_worker_queues_json(
    queues: &aws::WorkerQueues,
    messages: &[aws::QueueMessage],
    peeked: bool,
    bodies: bool,
    reason: Option<&str>,
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
            // `body` is REPLACED, never dropped. A missing key reads as
            // "this message had no body", which is a different claim
            // and the one an agent would act on — the same
            // absence-that-reads-as-an-answer shape `peeked` exists to
            // prevent. The placeholder says what happened and which
            // control produced it.
            let body = if bodies {
                format!("\"{}\"", crate::util::json_escape(&m.body))
            } else {
                "\"(withheld: mcp.peek_bodies = false)\"".to_string()
            };
            format!(
                "{{\"id\":\"{}\",\"sent_at\":{},\"receive_count\":{},\"task\":{},\"body\":{}}}",
                crate::util::json_escape(&m.id),
                sent,
                m.receive_count,
                task,
                body
            )
        })
        .collect();
    // WHY there is nothing, when there is nothing. All-nulls is
    // consistent with three different worlds — a web tier that has no
    // queues, a failure reading queue configuration, and EB not
    // reporting queues for an env that has them — and the tool
    // description naming the first is read once and elsewhere. Field
    // report: `peeked: false` correctly said "I did not look" and
    // nothing said why there was nothing to look at.
    let reason = match reason {
        Some(r) => format!(",\"reason\":{}", crate::util::json_string(r)),
        None => String::new(),
    };
    format!(
        "{{\"main_queue\":{{\"url\":{},\"stats\":{}}},\"dead_letter_queue\":{{\"url\":{},\"stats\":{},\"origin\":{}}},\"peeked\":{},\"messages\":[{}]{reason}}}",
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
            "dlq_undo" => self
                .tool_dlq_undo(args)
                .await
                .map_err(writes::WriteError::into_message),
            "doctor" => Ok(self.tool_doctor()),
            "audit_log" => self.tool_audit_log(args),
            "recent_events" => self.tool_recent_events(args).await,
            "list_versions" => self.tool_list_versions(args).await,
            "fleet_cost" => self.tool_fleet_cost(args).await,
            // Write surface (only reachable under --allow-writes — the
            // RPC layer gates the table on it). Phase 1 verbs plan;
            // confirm_action dispatches.
            "deploy" => self
                .tool_write_plan(writes::WriteVerb::Deploy, args)
                .await
                .map_err(writes::WriteError::into_message),
            "restart" => self
                .tool_write_plan(writes::WriteVerb::Restart, args)
                .await
                .map_err(writes::WriteError::into_message),
            "rebuild" => self
                .tool_write_plan(writes::WriteVerb::Rebuild, args)
                .await
                .map_err(writes::WriteError::into_message),
            "dlq_resend" => self
                .tool_write_plan(writes::WriteVerb::DlqResend, args)
                .await
                .map_err(writes::WriteError::into_message),
            "dlq_delete" => self
                .tool_write_plan(writes::WriteVerb::DlqDelete, args)
                .await
                .map_err(writes::WriteError::into_message),
            "dlq_purge" => self
                .tool_write_plan(writes::WriteVerb::DlqPurge, args)
                .await
                .map_err(writes::WriteError::into_message),
            "terminate" => self
                .tool_write_plan(writes::WriteVerb::Terminate, args)
                .await
                .map_err(writes::WriteError::into_message),
            "set_option" => self
                .tool_write_plan(writes::WriteVerb::SetOption, args)
                .await
                .map_err(writes::WriteError::into_message),
            "confirm_action" => self
                .tool_confirm_action(args)
                .await
                .map_err(writes::WriteError::into_message),
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
                // A failed stack listing is lost EBL008 coverage, and goes
                // in `skipped_envs` like every other. The comment here
                // said "same tolerance as the CLI path" and had been
                // false since 0.44, when the CLI started degrading on
                // it: an agent got a clean result for a check that
                // never ran.
                let latest_stacks = match client.list_solution_stacks().await {
                    Ok(stacks) => aws::latest_stack_versions(&stacks),
                    Err(e) => {
                        if !disabled.iter().any(|d| d == "EBL008") {
                            skipped.push(format!(
                                "EBL008 skipped — ListAvailableSolutionStacks: {e:#}"
                            ));
                        }
                        std::collections::HashMap::new()
                    }
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
                        &required_tags,
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
                // disabled. A failure does not fail the tool (an
                // Info-severity side pass), but it is lost coverage and
                // is REPORTED: `if let Ok` dropped a whole failed pass
                // with nothing in the result at all, while the CLI
                // degraded on it.
                if env_filter.is_none() && !disabled.iter().any(|d| d == "EBL015") {
                    match fetch_stale_platform_issues(&client, chrono::Utc::now()).await {
                        Ok((issues, branch_warnings)) => {
                            all_issues.extend(issues);
                            // Per-branch failures too — the CLI degrades
                            // on them since 0.45, and the tool
                            // description tells agents that
                            // `skipped_envs` is where lost coverage is.
                            skipped.extend(branch_warnings);
                        }
                        Err(e) => {
                            skipped.push(format!("EBL015 skipped — ListPlatformVersions: {e}"))
                        }
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
        Ok(append_cannot_fire(append_skipped_envs(
            lint::render_issues_json(&all_issues),
            &skipped,
        )))
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

    /// Answer "why can't I do X" without the agent having to guess.
    ///
    /// Three things look identical from the agent's side: a feature
    /// ebman lacks, a feature its CLIENT lacks, and a thing the
    /// operator forbade. It cannot tell them apart, and the default
    /// assumption — "ebman cannot do this" — is the one that gets
    /// reported as a capability gap and wastes everyone's time.
    ///
    /// The precedent is concrete. On 2026-09-17 the update checker
    /// logged `current="0.36.0" latest=0.38.0` three times while a
    /// session reported gaps against that same binary: the fact
    /// existed, in the right file, with no route to its consumer. The
    /// version line in `instructions` fixed that for version. This is
    /// the fix for everything else.
    ///
    /// Deliberately AWS-free and synchronous: a diagnostic that can
    /// fail for the reasons it exists to diagnose is not one.
    fn tool_doctor(&self) -> String {
        let elicits = self
            .client_supports_elicitation
            .load(std::sync::atomic::Ordering::Relaxed);
        let client = self
            .client_name
            .lock()
            .map(|c| c.clone())
            .unwrap_or_else(|_| "unknown".into());

        let writes = match &self.effective_scope() {
            super::WriteScope::None => "none - this server is read-only".to_string(),
            super::WriteScope::All => "every verb".to_string(),
            super::WriteScope::Only(v) => format!("{} only", v.join(", ")),
        };
        // WHY the surface is open, not just how wide it is.
        //
        // doctor reported `elicitation: true` and `writes: every verb`
        // as adjacent facts and never related them, and adjacency is
        // not causation — an agent cannot derive one from the other,
        // and a peer session confirmed it could not. The two
        // provenances imply OPPOSITE things to tell a user: a flag is
        // a standing grant, so "I can do this"; elicitation means
        // every action is gated on a person answering a dialog, so the
        // honest sentence is "I can propose this, and someone has to
        // approve it — they may decline, or not answer." Rendering the
        // same string for both makes one of those an over-promise.
        // TWO independent facts, and conflating them told the agent
        // something false.
        //
        // Where the SCOPE came from (a flag, or the parity default) is
        // not the same question as whether every write is ASKED. The
        // ask fires on the client's capability alone — see
        // `ask_operator` — so a connection holding BOTH an
        // `--allow-writes` grant and elicitation still puts every
        // write to a person. Branching on scope provenance alone
        // reported that connection as a standing grant, which
        // `docs/headless.md` tells the agent means "I can act", while
        // the plan on the same connection correctly said a person may
        // decline. One connection, two contradictory answers, and the
        // doctor half was the wrong one — the exact over-promise the
        // field was added to prevent, reintroduced by the addition.
        let writes_via = if matches!(self.effective_scope(), super::WriteScope::None) {
            "nothing - no writes are available".to_string()
        } else if self.write_scope.any() {
            let base = "--allow-writes - a standing grant from the operator";
            if elicits {
                format!(
                    "{base}, AND every write is still put to them, who may decline or \
                     not answer"
                )
            } else {
                base.to_string()
            }
        } else {
            "client-elicitation - EVERY write is put to the operator, who may decline \
             or not answer"
                .to_string()
        };

        // Counted, not listed: an agent does not need the operator's
        // whole pin table, and a refusal names the specific rule when
        // one actually fires. The count comes from `Config`, not from
        // the raw maps and not through `write_gate` — reading the maps
        // here tripped the CLI gate guard, and routing through the gate
        // tripped the one that says only the gates may touch the shared
        // decision. Both were right; the count belongs to neither.
        let pinned = self.safety_cfg.pinned_target_count();

        // The freeze is the ONE gate rung that changes mid-connection:
        // `gate_refusal` re-reads the cross-process marker on every
        // dispatch, so a `:freeze-deploys` from a live TUI session
        // refuses every MCP write while it stands. Doctor promised "the
        // standing restrictions in force" and never looked — during an
        // incident it reported writes as available, and an incident is
        // exactly when a triage agent calls this. A local file read,
        // so it costs nothing the AWS-free contract forbids.
        let frozen = crate::freeze::read_active().is_some();
        // Every write is refused when the safety config cannot be
        // parsed — `write_gate::decide` checks that FIRST and
        // unconditionally. Reporting only `safety_read_only` here said
        // `all_writes_refused: false` about a server refusing
        // everything.
        let unreadable = !self.safety_cfg.safety_parse_errors.is_empty();
        let all_refused = self.safety_cfg.safety_read_only || unreadable || frozen;

        let mut notes: Vec<String> = Vec::new();
        // The honest limit on the whole design, stated where an agent
        // reads it rather than left implicit.
        //
        // `docs/design/protection-levels.md`: "never describe
        // elicitation as 'a human confirmed'". The capability is a
        // self-report in the client's `initialize` frame; a framework
        // that declares it and routes the question to its own model
        // satisfies every ask. ebman cannot tell the difference at the
        // time, and an agent that tells its user "a person approved
        // this" would be asserting something neither of them can
        // check.
        if elicits {
            notes.push(String::from(
                "Writes here are gated on an elicitation your CLIENT said it \
                 supports. That is the client's word, not proof a person saw \
                 anything: ebman cannot distinguish an operator answering a dialog \
                 from a client answering for itself. Tell your user the \
                 confirmation was approved, not that a human approved it. The audit \
                 records each ask with its answer and how long it took, so the two \
                 can be told apart afterwards -- which is the smaller and true \
                 claim.",
            ));
        }
        if unreadable {
            notes.push(
                "The safety config could not be parsed, which fails CLOSED: every write is \
                 refused until the operator fixes it. This is not a fault in ebman and \
                 retrying will not clear it."
                    .into(),
            );
        }
        if frozen {
            notes.push(
                "A deploy freeze is active (set from a TUI session), so every write is \
                 refused while it stands. It is the one restriction here that can lift \
                 without anything restarting — the operator clears it with :thaw-deploys \
                 or :incident END."
                    .into(),
            );
        }
        if self.safety_cfg.safety_read_only {
            notes.push(
                "safety.read_only is set: EVERY write is refused, everywhere. No grant \
                 or confirmation lifts it - only the operator editing their config."
                    .into(),
            );
        }
        if !elicits {
            notes.push(
                "Your client did not declare elicitation support, so this server cannot \
                 put a question in front of your operator mid-call. Anything needing \
                 their decision has to be arranged by them instead."
                    .into(),
            );
        }
        if !self.redact {
            notes.push(
                "Redaction is OFF (--no-redact): `get_option_settings` and `why` return \
                 environment variable values and DBPassword verbatim. Treat what comes \
                 back as secret material — do not quote it, echo it into a summary, or \
                 paste it anywhere it will outlive this conversation."
                    .into(),
            );
        }
        if !self.safety_cfg.mcp_peek_bodies {
            notes.push(
                "mcp.peek_bodies is off: dead-lettered message bodies are withheld and \
                 replaced with a marker. A message shown with no body is not an empty \
                 message."
                    .into(),
            );
        }
        if matches!(self.backend, Backend::Demo) {
            notes.push(
                "This is a DEMO server. Every environment, queue and message is \
                 synthetic, no AWS call is made, and writes report success without \
                 doing anything."
                    .into(),
            );
        }

        format!(
            "{{\"ebman\":{},\"client\":{},\"client_declared\":{{\"elicitation\":{}}},\"writes\":{},\"writes_via\":{},\"standing_restrictions\":{{\"all_writes_refused\":{},\"pinned_targets\":{},\"config_unreadable\":{}}},\"redacting\":{},\"notes\":[{}]}}",
            util::json_string(env!("CARGO_PKG_VERSION")),
            util::json_string(&client),
            elicits,
            util::json_string(&writes),
            util::json_string(&writes_via),
            all_refused,
            pinned,
            !self.safety_cfg.safety_parse_errors.is_empty(),
            self.redact,
            notes
                .iter()
                .map(|n| util::json_string(n))
                .collect::<Vec<_>>()
                .join(",")
        )
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
                &render_worker_queues_json(
                    &queues,
                    &[],
                    false,
                    self.safety_cfg.mcp_peek_bodies,
                    empty_queue_reason(&env.tier, &queues),
                ),
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
                // Same gate as `worker_queues` — the function. `why`
                // always wants the peek, so `requested` is true and
                // the only question the gate answers here is whether
                // there is a queue to look in.
                let peek = match dlq_peek_target(&q, true) {
                    Some(url) => Some(
                        client
                            .peek_messages(url, 5)
                            .await
                            .map_err(|e| e.to_string()),
                    ),
                    None => None,
                };
                let (msgs, peeked) = dlq_peek_outcome(peek, &mut errors);
                render_worker_queues_json(
                    &q,
                    &msgs,
                    peeked,
                    self.safety_cfg.mcp_peek_bodies,
                    empty_queue_reason(&env.tier, &q),
                )
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
                // Hand-built, and it went stale the moment
                // `truncated_by_limit` was added: a client keying on
                // the field the tool description promises got a
                // missing key in demo. Both flags, both false, because
                // nothing was read and nothing was cut.
                "{{\"env\":\"{}\",\"groups\":[],\"complete\":true,\"truncated_by_limit\":false,\"events\":[],\"note\":\"demo mode reads no logs\"}}",
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
        let mut truncated = false;
        for g in &groups {
            let (evs, done, cut) = client
                .fetch_latest_log_events(g, since_ms, limit, filter.as_deref())
                .await
                .map_err(|e| tool_error(&profile, "fetch_latest_log_events", &e.to_string()))?;
            // One incomplete group makes the whole answer incomplete —
            // a consumer cannot act on "some of this is the oldest part
            // of the window" per group.
            complete &= done;
            truncated |= cut;
            events.extend(evs.into_iter().map(|e| (g.clone(), e)));
        }
        // The merge across groups can truncate even when no single
        // group did: two groups of `limit` events each yield `limit`
        // between them, and half of what was read is dropped here.
        truncated |= events.len() > limit;
        merge_newest(&mut events, limit);
        Ok(render_recent_logs_json(
            &env_name, &groups, complete, truncated, &events,
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
            // Demo looks at the FIXTURE, never at SQS. `peeked` still
            // reports whether we looked, so it tracks the request here
            // — the fixture is a real thing to look at, and what comes
            // back is what is in it.
            //
            // Before the fixture had messages this had to be `false`:
            // a demo peek of `poly-batch` answered `peeked: true,
            // messages: []` beside `visible: 12`, which reads as "the
            // dead-letter queue is empty" — a false all-clear next to
            // the depth contradicting it. The honest fix then was to
            // stop claiming to have looked; the better one is to have
            // something to look at.
            let queues = demo_fixture::worker_queues_for_env(&env_name);
            // The SAME gate as the live path below — the function,
            // not a second copy of the intent. Passing raw `peek` here
            // answered `peeked: true` for a web env with no queue at
            // all, which is exactly the defect the live gate was added
            // to stop, reintroduced on the path agents rehearse
            // against. Found by review three commits after the live
            // fix, which is why this now calls rather than restates.
            let peeked = dlq_peek_target(&queues, peek).is_some();
            let msgs = if peeked {
                demo_fixture::dlq_messages_for_env(&env_name)
            } else {
                Vec::new()
            };
            return Ok(render_worker_queues_json(
                &queues,
                &msgs,
                peeked,
                self.safety_cfg.mcp_peek_bodies,
                empty_queue_reason(&env.tier, &queues),
            ));
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

        let target = dlq_peek_target(&queues, peek);
        let messages = match target {
            Some(url) => client.peek_messages(url, max).await.map_err(|e| {
                tool_error(&arg_str(args, "profile"), "peek_messages", &e.to_string())
            })?,
            None => Vec::new(),
        };
        // `peeked` reports whether we LOOKED, not what was asked for.
        // Passing the request flag through said "we looked, it was
        // empty" for a queue that does not exist — the exact
        // distinction this field carries, and `why` already answered it
        // the other way for the same env. Derived from `target`, so it
        // cannot disagree with the decision that chose the URL.
        Ok(render_worker_queues_json(
            &queues,
            &messages,
            target.is_some(),
            self.safety_cfg.mcp_peek_bodies,
            empty_queue_reason(&env.tier, &queues),
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
#[path = "tests/tools.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/tools_renderer.rs"]
mod renderer_tests;
