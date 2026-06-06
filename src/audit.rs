//! Audit log: writers, parser, renderer, filter.
//!
//! Writers and parser are co-located so the line format has a single
//! source of truth. The typed writer APIs cover every shape:
//!
//! - [`append_action_dispatched`] / [`append_action_completed`] —
//!   normal TUI action lines (rebuild / restart / deploy / etc.).
//! - [`append_action_skipped`] / [`append_action_undone`] — one-shot
//!   action lines with no completion pair (a batch member skipped
//!   because its env left the view; an operator-driven undo).
//! - [`append_rollout`] — cross-region rollout lines tagged with a
//!   per-run `rollout_id` for post-mortem correlation.
//! - [`append_lint_fix`] — `ebman lint --fix` dispatches, tagged
//!   with the originating `rule_id`.
//! - [`append_dlq_op`] — one-shot DLQ ops (delete / resend / purge /
//!   replay), recorded at fire time.
//!
//! Plus [`append_raw`] — the lower-level "I already have a detail
//! string" entry point. As of 0.24 the hand-rolled `append_raw` action
//! sites have all moved to the typed siblings above; the only remaining
//! caller is the passive `stage=event kind=red_transition` health-log
//! line, which is genuinely an event, not an action.
//!
//! All paths funnel into the same private [`write_audit_line`]
//! helper (or its `_raw` sibling) so file rotation + webhook
//! fan-out apply uniformly to every line type.
//!
//! Line shapes:
//!
//! - Normal action:
//!   `{rfc3339}\taccount=A\tprofile=P\tregion=R\tstage=S action=Act target=Env [outcome=ok|err="..."]`
//! - Rollout:
//!   `{rfc3339}\trollout_id=ID\tregion=R\tstage=S action=Rollout target=Env version=V [outcome=ok|err="..."]`
//! - Lint fix:
//!   `{rfc3339}\tregion=R\tstage=fix action=SetOption target=Env rule_id=ID namespace=NS name=N value="V" outcome=ok|err="..."`
//!
//! The parser handles all three shapes uniformly: split on tab, then
//! tokenize every chunk as `key=value` pairs (with quoted-value
//! support). Known keys get promoted into typed fields on
//! [`AuditEntry`]; unknown keys land in `extras` so we don't drop
//! information.
//!
//! [`ebman audit`](../bin/ebman/cli/audit/index.html) — the CLI — uses
//! [`parse_audit_line`] + [`AuditFilter`] + the render helpers below
//! to surface entries for scripting / Slack-bot routing / on-call
//! dashboards / CI gating.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    pub when: String,
    pub account: Option<String>,
    pub profile: Option<String>,
    pub region: Option<String>,
    pub rollout_id: Option<String>,
    pub stage: Option<String>,
    pub action: Option<String>,
    pub target: Option<String>,
    pub version: Option<String>,
    pub rule_id: Option<String>,
    pub outcome: Option<String>,
    pub err: Option<String>,
    pub extras: BTreeMap<String, String>,
    pub raw: String,
}

/// Parse one audit-log line. Returns `None` for blank lines or lines
/// without an RFC3339-shaped timestamp as the first tab field.
pub fn parse_audit_line(line: &str) -> Option<AuditEntry> {
    let line = line.trim_end_matches('\n').trim_end_matches('\r');
    if line.is_empty() {
        return None;
    }
    let mut tabs = line.splitn(2, '\t');
    let when = tabs.next()?.trim().to_string();
    // Sanity: timestamp should at least look RFC3339-ish (`YYYY-MM-DDT...`).
    if when.len() < 10 || when.chars().nth(4) != Some('-') {
        return None;
    }
    let rest = tabs.next().unwrap_or("");
    // Tokenize the whole rest-of-line as key=value pairs. Tab and
    // space both function as separators; the kv parser walks until
    // the next `key=` boundary regardless of which one delimits.
    let pairs = parse_kv_pairs(rest);

    let mut entry = AuditEntry {
        when,
        account: None,
        profile: None,
        region: None,
        rollout_id: None,
        stage: None,
        action: None,
        target: None,
        version: None,
        rule_id: None,
        outcome: None,
        err: None,
        extras: BTreeMap::new(),
        raw: line.to_string(),
    };
    for (k, v) in pairs {
        // Treat literal "-" as missing — same convention
        // `write_audit_line` uses for absent account / profile.
        let v_opt = if v == "-" { None } else { Some(v) };
        match k.as_str() {
            "account" => entry.account = v_opt,
            "profile" => entry.profile = v_opt,
            "region" => entry.region = v_opt,
            "rollout_id" => entry.rollout_id = v_opt,
            "stage" => entry.stage = v_opt,
            "action" => entry.action = v_opt,
            "target" => entry.target = v_opt,
            "version" => entry.version = v_opt,
            "rule_id" => entry.rule_id = v_opt,
            "outcome" => entry.outcome = v_opt,
            "err" => entry.err = v_opt,
            _ => {
                if let Some(v) = v_opt {
                    entry.extras.insert(k, v);
                }
            }
        }
    }
    Some(entry)
}

/// Tokenize a string into `key=value` pairs. Keys are `[A-Za-z0-9_]+`.
/// Values are either `"quoted"` (everything between matched `"`s) or
/// unquoted (everything from `=` to the next whitespace-then-key=
/// boundary or end-of-string). Naked spaces inside an unquoted value
/// are preserved — e.g. `target=env-a ↔ env-b stage=dispatched`
/// yields `target` = `"env-a ↔ env-b"` and `stage` = `"dispatched"`.
pub fn parse_kv_pairs(text: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out: Vec<(String, String)> = Vec::new();
    let mut i = 0;
    while i < n {
        // Skip whitespace (space or tab).
        while i < n && (chars[i] == ' ' || chars[i] == '\t') {
            i += 1;
        }
        if i >= n {
            break;
        }
        // Read key: ident chars.
        let key_start = i;
        while i < n && (chars[i].is_alphanumeric() || chars[i] == '_') {
            i += 1;
        }
        if i == key_start || i >= n || chars[i] != '=' {
            // Not a key=value pair; skip to next whitespace.
            while i < n && chars[i] != ' ' && chars[i] != '\t' {
                i += 1;
            }
            continue;
        }
        let key: String = chars[key_start..i].iter().collect();
        i += 1; // consume '='

        // Read value.
        let value: String = if i < n && chars[i] == '"' {
            // Quoted: everything until the next `"`.
            i += 1;
            let val_start = i;
            while i < n && chars[i] != '"' {
                i += 1;
            }
            let v: String = chars[val_start..i].iter().collect();
            if i < n {
                i += 1;
            } // consume closing "
            v
        } else {
            // Unquoted: extend until the next `key=` boundary or EOL.
            let val_start = i;
            while i < n {
                if chars[i] == ' ' || chars[i] == '\t' {
                    // Lookahead: does the next non-whitespace chunk
                    // look like `ident=`? If so, stop here.
                    let mut j = i + 1;
                    while j < n && (chars[j] == ' ' || chars[j] == '\t') {
                        j += 1;
                    }
                    let ident_start = j;
                    while j < n && (chars[j].is_alphanumeric() || chars[j] == '_') {
                        j += 1;
                    }
                    if j > ident_start && j < n && chars[j] == '=' {
                        break;
                    }
                }
                i += 1;
            }
            let raw: String = chars[val_start..i].iter().collect();
            raw.trim().to_string()
        };
        out.push((key, value));
    }
    out
}

/// Sanitize a value-string for embedding in an audit-log line as the
/// inner content of a quoted `key="..."` pair. Replaces:
///
/// - `"` → `'` so the closing quote is unambiguous;
/// - `\n`, `\r`, `\t` → ` ` so a multi-line AWS error doesn't split
///   one audit entry into two on disk (the parser reads line-by-line,
///   so an embedded newline corrupts the next entry's RFC3339 prefix).
///
/// Used by [`append_action_completed`], [`append_rollout`], and
/// [`append_lint_fix`] (and the typed wrappers in `app.rs` that
/// route to them) so the escape rules stay consistent across every
/// writer.
pub fn escape_value(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '"' => '\'',
            '\n' | '\r' | '\t' => ' ',
            c => c,
        })
        .collect()
}

/// Filter spec applied to a parsed audit log. Returned subset is sorted
/// in the same order as the input.
#[derive(Debug, Default, Clone)]
pub struct AuditFilter<'a> {
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    pub env: Option<&'a str>,
    pub rule: Option<&'a str>,
    pub action: Option<&'a str>,
}

impl<'a> AuditFilter<'a> {
    pub fn matches(&self, entry: &AuditEntry) -> bool {
        if let Some(since) = self.since {
            if let Ok(when) = chrono::DateTime::parse_from_rfc3339(&entry.when) {
                if when.with_timezone(&chrono::Utc) < since {
                    return false;
                }
            } else {
                return false;
            }
        }
        if let Some(want) = self.env {
            if entry.target.as_deref() != Some(want) {
                return false;
            }
        }
        if let Some(want) = self.rule {
            if entry.rule_id.as_deref() != Some(want) {
                return false;
            }
        }
        if let Some(want) = self.action {
            if entry.action.as_deref() != Some(want) {
                return false;
            }
        }
        true
    }
}

/// Render audit entries as a pretty text table (TS / REGION / STAGE /
/// ACTION / TARGET / OUTCOME). Empty input yields a one-line `(no
/// entries)` so the operator sees the empty result didn't silently
/// match nothing.
pub fn render_audit_entries_text(entries: &[AuditEntry]) -> String {
    if entries.is_empty() {
        return "(no audit entries)\n".to_string();
    }
    let mut out = String::new();
    // Column widths sized to content.
    let w_ts = entries
        .iter()
        .map(|e| e.when.len())
        .max()
        .unwrap_or(20)
        .max(20);
    let w_region = entries
        .iter()
        .map(|e| e.region.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(6)
        .max(6);
    let w_stage = entries
        .iter()
        .map(|e| e.stage.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(5)
        .max(5);
    let w_action = entries
        .iter()
        .map(|e| e.action.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(6)
        .max(6);
    let w_target = entries
        .iter()
        .map(|e| e.target.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(6)
        .max(6);

    out.push_str(&format!(
        "{:<w_ts$}  {:<w_region$}  {:<w_stage$}  {:<w_action$}  {:<w_target$}  OUTCOME\n",
        "TS", "REGION", "STAGE", "ACTION", "TARGET",
    ));
    for e in entries {
        let outcome = match (e.outcome.as_deref(), e.err.as_deref()) {
            (_, Some(err)) => format!("err=\"{err}\""),
            (Some("ok"), _) => "ok".into(),
            (Some(s), _) => s.into(),
            _ => "-".into(),
        };
        out.push_str(&format!(
            "{:<w_ts$}  {:<w_region$}  {:<w_stage$}  {:<w_action$}  {:<w_target$}  {outcome}\n",
            e.when,
            e.region.as_deref().unwrap_or("-"),
            e.stage.as_deref().unwrap_or("-"),
            e.action.as_deref().unwrap_or("-"),
            e.target.as_deref().unwrap_or("-"),
        ));
    }
    out
}

/// Render audit entries as JSON Lines (one JSON object per line). Hand-
/// rolled so we don't pull in `serde_json` for this one path; values
/// are escaped per the JSON string-escape spec.
pub fn render_audit_entries_json(entries: &[AuditEntry]) -> String {
    let mut out = String::new();
    for e in entries {
        let mut first = true;
        out.push('{');
        let mut emit = |key: &str, val: Option<&str>| {
            if let Some(v) = val {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str(&format!("\"{key}\":{}", json_string(v)));
            }
        };
        emit("when", Some(&e.when));
        emit("account", e.account.as_deref());
        emit("profile", e.profile.as_deref());
        emit("region", e.region.as_deref());
        emit("rollout_id", e.rollout_id.as_deref());
        emit("stage", e.stage.as_deref());
        emit("action", e.action.as_deref());
        emit("target", e.target.as_deref());
        emit("version", e.version.as_deref());
        emit("rule_id", e.rule_id.as_deref());
        emit("outcome", e.outcome.as_deref());
        emit("err", e.err.as_deref());
        if !e.extras.is_empty() {
            if !first {
                out.push(',');
            }
            out.push_str("\"extras\":{");
            let mut first_extra = true;
            for (k, v) in &e.extras {
                if !first_extra {
                    out.push(',');
                }
                first_extra = false;
                out.push_str(&format!("{}:{}", json_string(k), json_string(v)));
            }
            out.push('}');
        }
        out.push_str("}\n");
    }
    out
}

/// Audit JSONL output uses the canonical [`crate::util::json_string`]
/// for value escaping.
use crate::util::json_string;

// ─── writers ─────────────────────────────────────────────────

/// Soft cap on `audit.log` size before we rotate to `audit.log.1`
/// (single historical backup, older history is discarded). 1 MiB ≈
/// ~5k action entries, plenty for an interactive operator tool.
const AUDIT_LOG_MAX_BYTES: u64 = 1 << 20;

/// Process-wide webhook URL for audit-line fan-out. Set once at App
/// or CLI startup from the resolved Config. `None` (or absent) means
/// no fan-out; the local audit file is always the source of truth.
static NOTIFY_WEBHOOK_URL: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

/// Configure the outbound webhook URL exactly once per process. Idempotent:
/// subsequent calls are no-ops (the first call wins, matching the previous
/// behaviour when this was an `OnceLock::set` site in `App::new`).
pub fn set_notify_webhook(url: Option<String>) {
    let _ = NOTIFY_WEBHOOK_URL.set(url);
}

/// Load `notify_webhook` from `~/.config/ebman/config.toml` and register
/// it for fan-out. Called once at CLI startup so audit lines emitted
/// by `ebman lint --fix`, `ebman action rollout`, etc. fan out to the
/// same webhook the TUI uses. Idempotent (the OnceLock guards repeat
/// calls). No-op when config can't be read; webhook is optional.
pub fn init_from_config_disk() {
    let cfg = crate::config::load();
    set_notify_webhook(cfg.notify_webhook);
}

/// Append a `stage=dispatched` line for a TUI-driven action. `target`
/// is pre-formatted by the caller so swap (`env-a ↔ env-b`) and
/// single-env (`env-a`) shapes are the caller's choice. `action_label`
/// goes into the `action=` field verbatim — typically the Debug-derived
/// variant name of [`crate::mode_action::Action`].
///
/// `extras` is an optional slice of `(key, value)` pairs emitted
/// after `target=`. Values are auto-quoted with `"..."` when they
/// contain whitespace; [`escape_value`] sanitises them so a stray
/// newline can't split the line on disk. Use this for additional
/// context the simple `action/target` shape doesn't carry (e.g.
/// `version=build-900`, `summary="MinSize=2 MaxSize=4"`).
pub fn append_action_dispatched(
    account: Option<&str>,
    profile: Option<&str>,
    region: &str,
    action_label: &str,
    target: &str,
    extras: &[(&str, &str)],
) {
    let mut detail = format!("stage=dispatched action={action_label} target={target}");
    append_extras(&mut detail, extras);
    write_audit_line(account, profile, region, &detail);
}

/// Append a `stage=completed` line for a TUI-driven action. `result`
/// is mapped to `outcome=ok` (Ok) or `outcome=err err="…"` (Err); the
/// error string goes through [`escape_value`] so a multi-line AWS
/// error doesn't split the entry across two log lines.
///
/// `extras` (same shape as in [`append_action_dispatched`]) lets
/// callers attach per-action context — e.g. `summary="..."` for an
/// option-settings update, `label=...` for a deploy, `cmd="..."`
/// for an SSM RunCommand. Emitted between `target=` and `outcome=`
/// so the wire shape stays stable.
pub fn append_action_completed(
    account: Option<&str>,
    profile: Option<&str>,
    region: &str,
    action_label: &str,
    target: &str,
    result: Result<(), &str>,
    extras: &[(&str, &str)],
) {
    let mut detail = format!("stage=completed action={action_label} target={target}");
    append_extras(&mut detail, extras);
    match result {
        Ok(()) => detail.push_str(" outcome=ok"),
        Err(e) => detail.push_str(&format!(" outcome=err err=\"{}\"", escape_value(e))),
    }
    write_audit_line(account, profile, region, &detail);
}

/// Append `extras` to a detail string. Pure helper so the
/// dispatched + completed paths share the encoding (and so the
/// tests cover it once). Auto-quotes values that contain
/// whitespace, `=`, or `"`; leaves simple values unquoted to match
/// the existing hand-rolled audit-line shape
/// (`namespace=ns name=opt value="..."`).
fn append_extras(detail: &mut String, extras: &[(&str, &str)]) {
    for (k, v) in extras {
        if v.is_empty() || v.contains(|c: char| c.is_whitespace() || c == '"' || c == '=') {
            detail.push_str(&format!(" {k}=\"{}\"", escape_value(v)));
        } else {
            detail.push_str(&format!(" {k}={v}"));
        }
    }
}

/// Append a rollout-shaped line. `stage` is `"dispatched"` or
/// `"completed"`; pass `err = Some(...)` to attach an error message
/// (and emit `outcome=err` on completion). `rollout_id` correlates
/// every per-region line within a single `ebman action rollout`
/// invocation.
pub fn append_rollout(
    rollout_id: &str,
    region: &str,
    env: &str,
    version: &str,
    stage: &str,
    err: Option<&str>,
) {
    let outcome_suffix = match (stage, err) {
        ("completed", None) => " outcome=ok".to_string(),
        ("completed", Some(e)) => format!(" outcome=err err=\"{}\"", escape_value(e)),
        (_, Some(e)) => format!(" err=\"{}\"", escape_value(e)),
        (_, None) => String::new(),
    };
    let line = format!(
        "\trollout_id={rollout_id}\tregion={region}\tstage={stage} action=Rollout target={env} version={version}{outcome_suffix}"
    );
    write_audit_line_raw(&line);
}

/// Append a `stage=fix action=SetOption` line for an `ebman lint
/// --fix` dispatch. `rule_id` correlates back to which lint rule
/// triggered the change so `ebman audit --rule EBL001` shows per-
/// rule history.
pub fn append_lint_fix(
    region: &str,
    env: &str,
    rule_id: &str,
    namespace: &str,
    name: &str,
    value: &str,
    err: Option<&str>,
) {
    let q_value = escape_value(value);
    let suffix = match err {
        None => " outcome=ok".to_string(),
        Some(e) => format!(" outcome=err err=\"{}\"", escape_value(e)),
    };
    let line = format!(
        "\tregion={region}\tstage=fix action=SetOption target={env} rule_id={rule_id} namespace={namespace} name={name} value=\"{q_value}\"{suffix}"
    );
    write_audit_line_raw(&line);
}

/// Append a `stage=skipped` line — an action that was deliberately not
/// dispatched (e.g. a batch member whose env vanished from the current
/// view mid-run). `reason` is quoted via [`escape_value`]. Same wire
/// shape the batch paths used to hand-roll; lifted here so the format
/// lives in one place alongside the other typed audit helpers.
pub fn append_action_skipped(
    account: Option<&str>,
    profile: Option<&str>,
    region: &str,
    action_label: &str,
    target: &str,
    reason: &str,
) {
    let detail = format!(
        "stage=skipped action={action_label} target={target} reason=\"{}\"",
        escape_value(reason)
    );
    write_audit_line(account, profile, region, &detail);
}

/// Append a `stage=undone` line — an operator-driven undo of a prior
/// action. No outcome (the undo dispatch logs its own completion via the
/// normal action path); this records that the undo was initiated.
pub fn append_action_undone(
    account: Option<&str>,
    profile: Option<&str>,
    region: &str,
    action_label: &str,
    target: &str,
) {
    let detail = format!("stage=undone action={action_label} target={target}");
    write_audit_line(account, profile, region, &detail);
}

/// Append a one-shot DLQ operation line (delete / resend / purge /
/// replay). These have no dispatched+completed pair — they're recorded
/// at fire time. `op` is the verb (`sqs-delete` / `dlq-resend` /
/// `dlq-purge` / `dlq-replay`); `extras` carry per-op context
/// (`msg_id`, `queue`, `count`) and are encoded like every other typed
/// helper. Centralizes the format the four DLQ spawn sites hand-rolled.
pub fn append_dlq_op(
    account: Option<&str>,
    profile: Option<&str>,
    region: &str,
    op: &str,
    env: &str,
    extras: &[(&str, &str)],
) {
    write_audit_line(account, profile, region, &dlq_op_detail(op, env, extras));
}

/// Pure detail-builder behind [`append_dlq_op`] — `"{op} env={env}"`
/// plus encoded extras. Split out so the wire shape is unit-testable
/// without the file-write side effect.
fn dlq_op_detail(op: &str, env: &str, extras: &[(&str, &str)]) -> String {
    let mut detail = format!("{op} env={env}");
    append_extras(&mut detail, extras);
    detail
}

/// Build the JSON body that goes to `notify_webhook`. Pure +
/// deterministic so the shape is unit-testable. Top-level `text`
/// gets the rendered audit line so the body is
/// Slack-incoming-webhook-compatible out of the box; the other
/// keys give consumers structured fields for routing / filtering.
pub fn build_webhook_body(
    account: Option<&str>,
    profile: Option<&str>,
    region: &str,
    detail: &str,
    when: &str,
) -> String {
    let text = format!(
        "[ebman] {} account={} profile={} region={} {}",
        when,
        account.unwrap_or("-"),
        profile.unwrap_or("-"),
        region,
        detail,
    );
    format!(
        "{{\"text\":\"{}\",\"at\":\"{}\",\"account\":\"{}\",\"profile\":\"{}\",\"region\":\"{}\",\"detail\":\"{}\"}}",
        json_escape(&text),
        json_escape(when),
        json_escape(account.unwrap_or("")),
        json_escape(profile.unwrap_or("")),
        json_escape(region),
        json_escape(detail),
    )
}

/// Append a raw audit-log line with a caller-built `detail` string.
/// Used by sites that emit non-action lines (red-transition events,
/// notifications, etc.) where the typed `append_action_*` APIs
/// don't fit. The `detail` string is appended verbatim after the
/// `account/profile/region` opener — caller is responsible for the
/// `key=value` shape + escaping.
pub fn append_raw(account: Option<&str>, profile: Option<&str>, region: &str, detail: &str) {
    write_audit_line(account, profile, region, detail);
}

fn write_audit_line(account: Option<&str>, profile: Option<&str>, region: &str, detail: &str) {
    let dir = crate::util::cache_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("audit.log");
    rotate_if_oversize(&path, AUDIT_LOG_MAX_BYTES);
    let when = chrono::Utc::now().to_rfc3339();
    let line = format!(
        "{when}\taccount={}\tprofile={}\tregion={}\t{detail}\n",
        account.unwrap_or("-"),
        profile.unwrap_or("-"),
        region,
    );
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(line.as_bytes());
    }
    // Webhook fan-out — same convention as before consolidation.
    if let Some(url) = NOTIFY_WEBHOOK_URL.get().and_then(|o| o.as_deref()) {
        fire_webhook(url, account, profile, region, detail, &when);
    }
}

/// Lower-level append: caller has already constructed the tab-prefixed
/// `\tkey=value\t...\tstage=... ...` tail (no leading timestamp). Used
/// by line shapes that don't follow the standard
/// `account=A\tprofile=P\tregion=R` opener (rollout uses
/// `rollout_id=...\tregion=...`; lint-fix uses just `region=...`).
fn write_audit_line_raw(tail: &str) {
    let dir = crate::util::cache_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("audit.log");
    rotate_if_oversize(&path, AUDIT_LOG_MAX_BYTES);
    let when = chrono::Utc::now().to_rfc3339();
    let line = format!("{when}{tail}\n");
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(line.as_bytes());
    }
    // Same webhook fan-out as `write_audit_line`. The body uses
    // `account=-, profile=-` because this shape doesn't carry them;
    // consumers route on `detail` / `region` instead.
    if let Some(url) = NOTIFY_WEBHOOK_URL.get().and_then(|o| o.as_deref()) {
        // Strip the leading tab so the detail string is the same
        // shape webhook consumers expect (key=value space-separated).
        let detail = tail.trim_start_matches('\t').replace('\t', " ");
        let region = detail
            .split(' ')
            .find_map(|tok| tok.strip_prefix("region="))
            .unwrap_or("-");
        fire_webhook(url, None, None, region, &detail, &when);
    }
}

/// Fire-and-forget webhook POST via `reqwest` (the same HTTP client
/// `llm.rs` already pulls in, so no extra dependency). 10s timeout so
/// a slow webhook can't accumulate hung requests. The caller must be
/// inside a tokio runtime — guarded below with `Handle::try_current`
/// so a non-runtime call path silently no-ops rather than panicking.
fn fire_webhook(
    url: &str,
    account: Option<&str>,
    profile: Option<&str>,
    region: &str,
    detail: &str,
    when: &str,
) {
    let body = build_webhook_body(account, profile, region, detail, when);
    let url = url.to_string();
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    target: "ebman::notify",
                    url = %url,
                    error = %e,
                    "audit webhook: could not build reqwest client"
                );
                return;
            }
        };
        match client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {}
            Ok(resp) => {
                tracing::warn!(
                    target: "ebman::notify",
                    url = %url,
                    status = %resp.status(),
                    "audit webhook returned non-success status"
                );
            }
            Err(e) => {
                tracing::warn!(
                    target: "ebman::notify",
                    url = %url,
                    error = %e,
                    "audit webhook request failed"
                );
            }
        }
    });
}

/// If `path` exists and is larger than `max_bytes`, move it to
/// `path.1` (overwriting any previous backup) so the next write
/// starts a fresh file. Best-effort: any I/O error is swallowed —
/// we don't want to lose the audit entry just because rotation
/// failed.
fn rotate_if_oversize(path: &std::path::Path, max_bytes: u64) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.len() <= max_bytes {
        return;
    }
    let backup = {
        let mut name = path
            .file_name()
            .map(|s| s.to_os_string())
            .unwrap_or_default();
        name.push(".1");
        path.with_file_name(name)
    };
    let _ = std::fs::rename(path, backup);
}

// Webhook body uses the canonical `crate::util::json_escape`; the
// previously-local `json_escape` is routed through there via the
// import at the top of the writers section.
use crate::util::json_escape;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dlq_op_detail_preserves_wire_shape() {
        // Format must match what the four DLQ spawn sites hand-rolled
        // before centralizing — `ebman audit` parses these lines.
        assert_eq!(
            dlq_op_detail("dlq-replay", "prod", &[("count", "12")]),
            "dlq-replay env=prod count=12"
        );
        assert_eq!(
            dlq_op_detail("dlq-purge", "prod", &[]),
            "dlq-purge env=prod"
        );
        assert_eq!(
            dlq_op_detail("sqs-delete", "prod", &[("queue", "DLQ"), ("msg_id", "abc")]),
            "sqs-delete env=prod queue=DLQ msg_id=abc"
        );
    }

    #[test]
    fn parse_kv_simple_pairs() {
        let pairs = parse_kv_pairs("account=A profile=B region=us-east-1");
        assert_eq!(
            pairs,
            vec![
                ("account".into(), "A".into()),
                ("profile".into(), "B".into()),
                ("region".into(), "us-east-1".into()),
            ]
        );
    }

    #[test]
    fn parse_kv_quoted_value() {
        let pairs = parse_kv_pairs("err=\"AccessDenied: not authorized\" stage=completed");
        assert_eq!(
            pairs,
            vec![
                ("err".into(), "AccessDenied: not authorized".into()),
                ("stage".into(), "completed".into()),
            ]
        );
    }

    #[test]
    fn parse_kv_unquoted_value_with_spaces() {
        // The naked space before "↔" should be preserved because the
        // next token isn't `key=`.
        let pairs = parse_kv_pairs("target=env-a ↔ env-b stage=dispatched");
        assert_eq!(
            pairs,
            vec![
                ("target".into(), "env-a ↔ env-b".into()),
                ("stage".into(), "dispatched".into()),
            ]
        );
    }

    #[test]
    fn parse_kv_tab_separator() {
        let pairs = parse_kv_pairs("account=A\tprofile=B\tregion=R");
        assert_eq!(
            pairs,
            vec![
                ("account".into(), "A".into()),
                ("profile".into(), "B".into()),
                ("region".into(), "R".into()),
            ]
        );
    }

    #[test]
    fn parse_audit_line_normal_dispatched() {
        let line = "2026-05-27T10:15:30Z\taccount=123\tprofile=prod\tregion=us-east-1\tstage=dispatched action=Restart target=my-env";
        let entry = parse_audit_line(line).expect("parses");
        assert_eq!(entry.when, "2026-05-27T10:15:30Z");
        assert_eq!(entry.account.as_deref(), Some("123"));
        assert_eq!(entry.profile.as_deref(), Some("prod"));
        assert_eq!(entry.region.as_deref(), Some("us-east-1"));
        assert_eq!(entry.stage.as_deref(), Some("dispatched"));
        assert_eq!(entry.action.as_deref(), Some("Restart"));
        assert_eq!(entry.target.as_deref(), Some("my-env"));
        assert!(entry.err.is_none());
        assert!(entry.outcome.is_none());
    }

    #[test]
    fn parse_audit_line_completed_with_outcome_ok() {
        // Modern shape: `outcome=ok` as an explicit key=value pair.
        // 0.14+ writers emit this so the parser doesn't have to
        // special-case bare trailing "ok".
        let line = "2026-05-27T10:15:31Z\taccount=123\tprofile=prod\tregion=us-east-1\tstage=completed action=Restart target=my-env outcome=ok";
        let entry = parse_audit_line(line).expect("parses");
        assert_eq!(entry.stage.as_deref(), Some("completed"));
        assert_eq!(entry.action.as_deref(), Some("Restart"));
        assert_eq!(entry.target.as_deref(), Some("my-env"));
        assert_eq!(entry.outcome.as_deref(), Some("ok"));
    }

    #[test]
    fn parse_audit_line_pre_0_14_bare_ok_lossy_but_parses() {
        // Pre-0.14 entries had bare `ok` after the detail. Parser
        // can't promote it; target value extends to include it. We
        // accept this as a soft regression on legacy log lines —
        // operators who care about historical analysis read the
        // `raw` field.
        let line = "2026-05-26T08:00:00Z\taccount=123\tprofile=prod\tregion=us-east-1\tstage=completed action=Restart target=my-env ok";
        let entry = parse_audit_line(line).expect("parses");
        assert_eq!(entry.outcome, None);
        assert_eq!(entry.target.as_deref(), Some("my-env ok"));
    }

    #[test]
    fn parse_audit_line_completed_with_outcome_err() {
        let line = "2026-05-27T10:16:00Z\taccount=123\tprofile=-\tregion=us-east-1\tstage=completed action=Deploy target=my-env err=\"UpdateEnvironment: throttled\"";
        let entry = parse_audit_line(line).expect("parses");
        assert_eq!(entry.profile, None); // "-" promoted to None
        assert_eq!(entry.err.as_deref(), Some("UpdateEnvironment: throttled"));
    }

    #[test]
    fn parse_audit_line_rollout_shape() {
        let line = "2026-05-27T10:20:00Z\trollout_id=rollout-20260527T102000Z\tregion=eu-west-1\tstage=dispatched action=Rollout target=prod-api version=build-900";
        let entry = parse_audit_line(line).expect("parses");
        assert_eq!(
            entry.rollout_id.as_deref(),
            Some("rollout-20260527T102000Z")
        );
        assert_eq!(entry.region.as_deref(), Some("eu-west-1"));
        assert_eq!(entry.action.as_deref(), Some("Rollout"));
        assert_eq!(entry.target.as_deref(), Some("prod-api"));
        assert_eq!(entry.version.as_deref(), Some("build-900"));
        // No account / profile in rollout shape — should stay None.
        assert!(entry.account.is_none());
        assert!(entry.profile.is_none());
    }

    #[test]
    fn parse_audit_line_blank_returns_none() {
        assert!(parse_audit_line("").is_none());
        assert!(parse_audit_line("   ").is_none());
        assert!(parse_audit_line("\n").is_none());
    }

    #[test]
    fn parse_audit_line_missing_timestamp_returns_none() {
        assert!(parse_audit_line("garbage line without rfc3339").is_none());
    }

    #[test]
    fn filter_by_since() {
        let entries = [
            parse_audit_line("2026-05-27T08:00:00Z\tregion=r\tstage=s action=A target=t").unwrap(),
            parse_audit_line("2026-05-27T11:00:00Z\tregion=r\tstage=s action=A target=t").unwrap(),
        ];
        let cutoff = chrono::DateTime::parse_from_rfc3339("2026-05-27T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let filter = AuditFilter {
            since: Some(cutoff),
            ..Default::default()
        };
        let kept: Vec<_> = entries.iter().filter(|e| filter.matches(e)).collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].when, "2026-05-27T11:00:00Z");
    }

    #[test]
    fn filter_by_env_target() {
        let entries = [
            parse_audit_line("2026-05-27T10:00:00Z\tregion=r\tstage=s action=Restart target=env-a")
                .unwrap(),
            parse_audit_line("2026-05-27T10:01:00Z\tregion=r\tstage=s action=Restart target=env-b")
                .unwrap(),
        ];
        let filter = AuditFilter {
            env: Some("env-b"),
            ..Default::default()
        };
        let kept: Vec<_> = entries.iter().filter(|e| filter.matches(e)).collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].target.as_deref(), Some("env-b"));
    }

    #[test]
    fn filter_by_rule_id() {
        let entries = [
            parse_audit_line("2026-05-27T10:00:00Z\tregion=r\tstage=fix action=SetOption target=env-a rule_id=EBL001")
                .unwrap(),
            parse_audit_line("2026-05-27T10:01:00Z\tregion=r\tstage=fix action=SetOption target=env-a rule_id=EBL004")
                .unwrap(),
        ];
        let filter = AuditFilter {
            rule: Some("EBL004"),
            ..Default::default()
        };
        let kept: Vec<_> = entries.iter().filter(|e| filter.matches(e)).collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].rule_id.as_deref(), Some("EBL004"));
    }

    #[test]
    fn rotate_if_oversize_renames_when_too_big() {
        let dir = std::env::temp_dir().join(format!("ebman-rotate-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("audit.log");
        let backup = dir.join("audit.log.1");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&backup);
        std::fs::write(&path, vec![b'x'; 100]).unwrap();
        rotate_if_oversize(&path, 50);
        assert!(!path.exists(), "current file should have been renamed");
        assert!(backup.exists(), "rotated backup should now exist");
        let _ = std::fs::remove_file(&backup);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn rotate_if_oversize_leaves_small_files_alone() {
        let dir = std::env::temp_dir().join(format!("ebman-rotate-small-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("audit.log");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, b"tiny").unwrap();
        rotate_if_oversize(&path, 1_000);
        assert!(path.exists());
        assert!(!dir.join("audit.log.1").exists());
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn build_webhook_body_has_slack_compatible_text_plus_structured_fields() {
        // Slack incoming webhooks consume a top-level `text` field;
        // anything else is metadata for other consumers. Both must
        // be present so one endpoint can serve both.
        let body = build_webhook_body(
            Some("123456789012"),
            Some("prod"),
            "us-east-1",
            "stage=request action=Deploy target=prod-api",
            "2026-05-25T12:00:00Z",
        );
        assert!(body.starts_with('{') && body.ends_with('}'));
        assert!(
            body.contains("\"text\":\"[ebman]"),
            "missing slack-shaped text field"
        );
        assert!(body.contains("\"at\":\"2026-05-25T12:00:00Z\""));
        assert!(body.contains("\"account\":\"123456789012\""));
        assert!(body.contains("\"profile\":\"prod\""));
        assert!(body.contains("\"region\":\"us-east-1\""));
        assert!(body.contains("\"detail\":\"stage=request action=Deploy target=prod-api\""));
    }

    #[test]
    fn build_webhook_body_dashes_missing_account_and_profile_in_text() {
        let body = build_webhook_body(
            None,
            None,
            "eu-west-1",
            "stage=event kind=red_transition env=prod-api",
            "2026-05-25T12:00:00Z",
        );
        assert!(
            body.contains("account=- profile=- region=eu-west-1"),
            "missing dash placeholders in text, got: {body}"
        );
        // Structured fields use empty strings, not "-", so consumers
        // can distinguish "unknown" from "literal dash".
        assert!(body.contains("\"account\":\"\""));
        assert!(body.contains("\"profile\":\"\""));
    }

    #[test]
    fn build_webhook_body_escapes_quotes_in_detail() {
        let body = build_webhook_body(
            None,
            None,
            "us-east-1",
            "stage=event message=\"deploy started\"",
            "2026-05-25T12:00:00Z",
        );
        // Escaped string appears in both `text` and `detail`.
        assert!(body.contains("\\\"deploy started\\\""));
        // Round-trip via serde_yml's JSON-tolerant path: must parse.
        let _: serde_yml::Value = serde_yml::from_str(&body)
            .expect("webhook body must be parseable JSON / YAML-superset");
    }

    #[test]
    fn escape_value_replaces_quotes_and_newlines() {
        assert_eq!(escape_value("plain"), "plain");
        assert_eq!(escape_value("with \"quotes\""), "with 'quotes'");
        assert_eq!(escape_value("line1\nline2"), "line1 line2");
        assert_eq!(escape_value("a\r\nb"), "a  b");
        assert_eq!(escape_value("a\tb"), "a b");
        assert_eq!(
            escape_value("AccessDenied: \"role\" not allowed\n  caused by: foo"),
            "AccessDenied: 'role' not allowed   caused by: foo"
        );
    }

    #[test]
    fn render_text_empty_says_no_entries() {
        let out = render_audit_entries_text(&[]);
        assert!(out.contains("no audit entries"));
    }

    #[test]
    fn render_text_columns_have_header_and_rows() {
        let entries = vec![parse_audit_line(
            "2026-05-27T10:15:30Z\taccount=A\tprofile=P\tregion=us-east-1\tstage=dispatched action=Restart target=my-env",
        )
        .unwrap()];
        let out = render_audit_entries_text(&entries);
        assert!(out.contains("TS"));
        assert!(out.contains("REGION"));
        assert!(out.contains("STAGE"));
        assert!(out.contains("ACTION"));
        assert!(out.contains("TARGET"));
        assert!(out.contains("OUTCOME"));
        assert!(out.contains("2026-05-27T10:15:30Z"));
        assert!(out.contains("us-east-1"));
        assert!(out.contains("dispatched"));
        assert!(out.contains("Restart"));
        assert!(out.contains("my-env"));
    }

    #[test]
    fn render_json_emits_jsonl() {
        let entries = vec![parse_audit_line(
            "2026-05-27T10:15:30Z\taccount=A\tprofile=P\tregion=us-east-1\tstage=dispatched action=Restart target=my-env",
        )
        .unwrap()];
        let out = render_audit_entries_json(&entries);
        // One JSON object per line.
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 1);
        let line = lines[0];
        assert!(line.starts_with('{') && line.ends_with('}'));
        assert!(line.contains("\"when\":\"2026-05-27T10:15:30Z\""));
        assert!(line.contains("\"action\":\"Restart\""));
        assert!(line.contains("\"target\":\"my-env\""));
        // Absent fields (err, version, rule_id) should not appear.
        assert!(!line.contains("\"err\""));
        assert!(!line.contains("\"version\""));
    }

    #[test]
    fn render_json_escapes_quotes_and_control_chars() {
        let entries = vec![parse_audit_line(
            "2026-05-27T10:15:30Z\taccount=A\tprofile=P\tregion=r\tstage=completed action=Deploy target=env err=\"line1\\nline2 with \\\"quotes\\\"\"",
        )
        .unwrap()];
        let _out = render_audit_entries_json(&entries);
        // Just assert the function doesn't panic on tricky values.
        // (Round-trip semantics not in scope for v1; raw is the
        // source-of-truth log.)
    }

    /// **Golden pin** for `append_extras` — pins the exact wire shape
    /// of the `key=value` / `key="..."` encoding so a future quoting-
    /// policy change becomes a deliberate decision. Audit-log
    /// consumers (incident reviewers running `awk '$5 == "stage=…"'`)
    /// depend on this shape; silent changes invalidate their tooling.
    ///
    /// If this test fails: the change to `append_extras` is a wire-
    /// breaking change. Document the new format in the CHANGELOG,
    /// bump audit-shape version notes, and update this golden — or
    /// revert the change.
    ///
    /// Pinned in 0.19 (was a 0.18 review item).
    #[test]
    fn append_extras_golden_wire_shape() {
        let mut detail = String::from("stage=dispatched action=Demo target=env-1");
        append_extras(
            &mut detail,
            &[
                ("simple", "abc"),      // unquoted: no whitespace / quote / equals
                ("with_space", "a b"),  // quoted: contains whitespace
                ("with_quote", "a\"b"), // quoted + escaped
                ("with_equals", "a=b"), // quoted: contains '='
                ("empty", ""),          // quoted: empty value (distinguishable from omitted)
            ],
        );
        assert_eq!(
            detail,
            r#"stage=dispatched action=Demo target=env-1 simple=abc with_space="a b" with_quote="a'b" with_equals="a=b" empty="""#,
            "append_extras wire format changed — see test docstring before updating this constant"
        );
    }
}
