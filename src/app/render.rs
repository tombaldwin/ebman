//! Pure text renderers for overlay bodies.
//!
//! Everything here takes plain data and returns a `String` that some
//! `Overlay::*` variant displays. No `App`, no I/O, no ratatui — which
//! is what makes them straightforward to unit-test.

use super::*;
use crate::aws::DlqOrigin;

/// Render the `:changes` overlay — the env's deploy / config-change
/// events as a newest-first timeline. Routine health + scaling
/// events are filtered out by [`is_config_event`].
pub(crate) fn render_changes_overlay(env: &str, events: &[EbEvent]) -> String {
    let rows: Vec<&EbEvent> = events
        .iter()
        .filter(|e| is_config_event(&e.message))
        .collect();
    if rows.is_empty() {
        return format!(
            "Config change timeline — {env}\n\n\
             No deploy / config-change events in the recent window.\n\n\
             esc / q to close"
        );
    }
    let mut body = format!(
        "Config change timeline — {env}\n\
         {} change event(s), newest first.\n\n",
        rows.len()
    );
    for e in rows {
        let ts =
            e.at.map(|t| t.format("%Y-%m-%d %H:%M:%SZ").to_string())
                .unwrap_or_else(|| "—".into());
        let ver = e
            .version_label
            .as_deref()
            .map(|v| format!("  [{v}]"))
            .unwrap_or_default();
        body.push_str(&format!("{ts}{ver}\n    {}\n\n", e.message));
    }
    body.push_str("esc / q to close");
    body
}

/// One row in the `:lineage` overlay — a single deploy, identified
/// by its version label. The two timestamps bracket the deploy's
/// event group (earliest event = "deploy started", latest = "deploy
/// completed"); the gap to the *next-older* deploy is computed at
/// render time so the row stays cheap to compare.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LineageRow {
    pub label: String,
    pub first_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Pure: collapse the env's recent events into one row per distinct
/// deploy. Events come in newest-first; this walks them oldest-first
/// so consecutive same-label events fold into one row carrying the
/// span (first → last) of that deploy's event group, then reverses
/// the result so callers see newest-first. Events without a
/// `version_label` are dropped — `:lineage` is the deploy-only cut
/// of the event history.
pub(crate) fn build_lineage(events: &[EbEvent]) -> Vec<LineageRow> {
    let mut oldest_first: Vec<(&EbEvent, String)> = events
        .iter()
        .filter_map(|e| {
            let label = e.version_label.as_deref().filter(|v| !v.is_empty())?;
            Some((e, label.to_string()))
        })
        .collect();
    oldest_first.reverse();
    let mut rows: Vec<LineageRow> = Vec::new();
    for (e, label) in oldest_first {
        match rows.last_mut() {
            Some(last) if last.label == label => {
                if let Some(t) = e.at {
                    last.last_at = Some(t);
                }
            }
            _ => rows.push(LineageRow {
                label,
                first_at: e.at,
                last_at: e.at,
            }),
        }
    }
    rows.into_iter().rev().collect()
}

/// Render the `:lineage` overlay — one row per deploy, newest first,
/// with the deploy's span (`took`) and the gap to the next-older
/// deploy (`Δ since previous`). Empty event window produces a stub
/// matching the `:changes` style so the operator isn't left wondering
/// whether the fetch silently failed.
pub(crate) fn format_lineage(env: &str, events: &[EbEvent]) -> String {
    let rows = build_lineage(events);
    if rows.is_empty() {
        return format!(
            "Deploy lineage — {env}\n\n\
             No deploys in the recent event window.\n\n\
             esc / q to close"
        );
    }
    let mut body = format!(
        "Deploy lineage — {env}\n\
         {} deploy(s), newest first.  Δ = gap between deploy starts.\n\n",
        rows.len()
    );
    for (i, row) in rows.iter().enumerate() {
        let ts = row
            .first_at
            .map(|t| t.format("%Y-%m-%d %H:%M:%SZ").to_string())
            .unwrap_or_else(|| "—".into());
        body.push_str(&format!("  ▸ {ts}  {}\n", row.label));
        if let (Some(f), Some(l)) = (row.first_at, row.last_at) {
            let span = l - f;
            if span.num_seconds() > 0 {
                body.push_str(&format!(
                    "       took {}\n",
                    humanize_short_age(Duration::from_secs(span.num_seconds() as u64))
                ));
            }
        }
        if let Some(next) = rows.get(i + 1) {
            if let (Some(this), Some(prev)) = (row.first_at, next.first_at) {
                let gap = this - prev;
                if gap.num_seconds() > 0 {
                    body.push_str(&format!(
                        "       Δ {} since previous deploy\n",
                        humanize_short_age(Duration::from_secs(gap.num_seconds() as u64))
                    ));
                }
            }
        }
        body.push('\n');
    }
    body.push_str("esc / q to close");
    body
}

/// Pure: render the `:rollbacks-armed` overlay body — one row per
/// armed watchdog with env / target_label / armed_at age / time
/// remaining until deadline. Sorted by deadline so the soonest-
/// firing watchdog reads first.
pub(crate) fn format_armed_rollbacks(
    armed: &std::collections::HashMap<String, ArmedWatchdog>,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    if armed.is_empty() {
        return "(no auto-rollbacks armed)\n\nesc / q to close".to_string();
    }
    let mut rows: Vec<&ArmedWatchdog> = armed.values().collect();
    rows.sort_by_key(|w| w.deadline_at);
    let mut body = String::new();
    body.push_str("ENV                              TARGET            ARMED      DEADLINE IN\n");
    body.push_str("─────────────────────────────────────────────────────────────────────────\n");
    for w in rows {
        let armed_ago = (now - w.armed_at).num_seconds().max(0) as u64;
        let remaining_secs = (w.deadline_at - now).num_seconds();
        let armed_str = humanize_short_age(Duration::from_secs(armed_ago));
        let remaining_str = if remaining_secs <= 0 {
            "fired / expired".to_string()
        } else {
            humanize_short_age(Duration::from_secs(remaining_secs as u64))
        };
        body.push_str(&format!(
            "{:<32} {:<17} {:>5} ago  {}\n",
            truncate_armed_cell(&w.env_name, 32),
            truncate_armed_cell(&w.target_label, 17),
            armed_str,
            remaining_str,
        ));
    }
    body.push_str("\nesc / q to close");
    body
}

/// Soonest-firing armed watchdog's countdown — used by the header
/// pill chain to show "⏱ rollback prod-api in 4m22s". Returns
/// `None` when nothing is armed.
pub(crate) fn soonest_armed_rollback(
    armed: &std::collections::HashMap<String, ArmedWatchdog>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<(String, String)> {
    let next = armed.values().min_by_key(|w| w.deadline_at)?;
    let remaining_secs = (next.deadline_at - now).num_seconds();
    let remaining_str = if remaining_secs <= 0 {
        "now".to_string()
    } else {
        humanize_short_age(Duration::from_secs(remaining_secs as u64))
    };
    Some((next.env_name.clone(), remaining_str))
}

/// Soonest-resolving watching-deploy tracker's countdown — used by
/// the header pill so the operator sees "👁 watching prod-api in
/// 4m22s" for `:deploy --wait-for-green Nm`. Returns `None` when
/// nothing is being watched. Parallel to `soonest_armed_rollback`.
pub(crate) fn soonest_watching_deploy(
    watching: &std::collections::HashMap<String, WatchingDeploy>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<(String, String)> {
    let next = watching.values().min_by_key(|w| w.deadline_at)?;
    let remaining_secs = (next.deadline_at - now).num_seconds();
    let remaining_str = if remaining_secs <= 0 {
        "now".to_string()
    } else {
        humanize_short_age(Duration::from_secs(remaining_secs as u64))
    };
    Some((next.env_name.clone(), remaining_str))
}

/// Cell truncator local to `format_armed_rollbacks`. Trailing `…`
/// keeps the column alignment stable on long env names / version
/// labels.
fn truncate_armed_cell(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Render the `:versions` overlay body. Marks the currently-deployed
/// version with `◀ deployed`; trims the redundant
/// "Application version created from " prefix that every CI-pipeline
/// description tends to carry; shows "showing N of M (newest first)"
/// when the list was truncated. `limit` caps the visible rows.
/// Pure: render the `:deploy LABEL --preview` body. Highlights the
/// candidate version (label / age / description), the currently-deployed
/// version's age for context, and warns if the candidate predates the
/// current one (rolling back is intentional but worth flagging).
///
/// `versions` is the result of `list_application_versions` (already
/// sorted newest-first by the aws layer). Missing labels surface as
/// human-readable "not found" hints rather than blanks.
/// Pure: render the `:accounts` overlay body. Rows are sorted ACTIVE-first
/// then by name (the `list_org_accounts` helper does the sort on the AWS
/// side); this just formats one row per account with a `(:account NAME)`
/// hint when a matching `accounts.NAME` entry is configured in
/// config.toml. Without that entry, the row is informational only — the
/// operator must still configure a role_arn before AssumeRole works.
///
/// `configured` is the set of friendly names from `config.toml`'s
/// `accounts.*` section; matching is name-or-id-suffix so an operator
/// who names their entries by account-id still gets the hint.
pub fn format_org_accounts(
    accounts: &[crate::aws::OrgAccount],
    configured: &std::collections::HashMap<String, String>,
) -> String {
    if accounts.is_empty() {
        return "no accounts returned by organizations:ListAccounts\n\nesc / q to close".into();
    }
    let mut out = String::new();
    out.push_str(&format!(
        "Org accounts ({})\n────────────────────\n\n",
        accounts.len()
    ));
    let max_name = accounts
        .iter()
        .map(|a| a.name.len())
        .max()
        .unwrap_or(0)
        .min(28);
    for a in accounts {
        let switchable = configured
            .keys()
            .find(|n| {
                n.eq_ignore_ascii_case(&a.name)
                    || n.eq_ignore_ascii_case(&a.id)
                    || n.eq_ignore_ascii_case(&format!("acct-{}", a.id))
            })
            .cloned();
        let switch_hint = match switchable {
            Some(n) => format!(" :account {n}"),
            None => String::new(),
        };
        let status_marker = match a.status.as_str() {
            "ACTIVE" => "●",
            "SUSPENDED" => "⊘",
            _ => "○",
        };
        out.push_str(&format!(
            "  {status_marker} {name:<width$}  {id}  [{status}]{switch_hint}\n",
            name = a.name,
            width = max_name,
            id = a.id,
            status = a.status,
        ));
        if let Some(email) = a.email.as_ref() {
            out.push_str(&format!(
                "    {pad:<width$}  ↳ {email}\n",
                pad = "",
                width = max_name,
            ));
        }
    }
    out.push('\n');
    out.push_str(
        "To switch into an account, add `accounts.NAME.role_arn = …` to config.toml\n\
         then use `:account NAME`. esc / q to close.",
    );
    out
}

pub fn format_deploy_preview(
    env_name: &str,
    current_label: &str,
    candidate_label: &str,
    versions: &[crate::aws::AppVersion],
) -> String {
    let now = chrono::Utc::now();
    let humanize = |d: Option<chrono::DateTime<chrono::Utc>>| -> String {
        d.map(|t| {
            let dur = now.signed_duration_since(t);
            let secs = dur.num_seconds().max(0);
            if secs < 3600 {
                format!("{}m ago", secs / 60)
            } else if secs < 86_400 {
                format!("{}h ago", secs / 3600)
            } else {
                format!("{}d ago", secs / 86_400)
            }
        })
        .unwrap_or_else(|| "—".into())
    };
    let candidate = versions.iter().find(|v| v.label == candidate_label);
    let current = if current_label.is_empty() {
        None
    } else {
        versions.iter().find(|v| v.label == current_label)
    };
    let mut out = String::new();
    out.push_str(&format!("env:        {env_name}\n"));
    out.push_str(&format!(
        "current:    {}{}\n",
        if current_label.is_empty() {
            "(none deployed)".to_string()
        } else {
            current_label.to_string()
        },
        match current.and_then(|v| v.created) {
            Some(t) => format!("  ({})", humanize(Some(t))),
            None => String::new(),
        }
    ));
    out.push_str(&format!("candidate:  {candidate_label}"));
    match candidate {
        Some(v) => {
            out.push_str(&format!("  ({})\n", humanize(v.created)));
            if !v.description.is_empty() {
                out.push_str(&format!("description: {}\n", v.description));
            }
        }
        None => {
            out.push_str("\n\n");
            out.push_str(&format!(
                "⚠ candidate label '{candidate_label}' not found in this app's version list — \
                 deploy will fail. Run :versions to see available labels.\n"
            ));
            return out;
        }
    }
    // Rollback warning — only fires when both timestamps are known and
    // the candidate is older than current. Rolling back IS legitimate;
    // the warning just gives the operator a beat to confirm intent.
    if let (Some(cand), Some(curr)) = (
        candidate.and_then(|v| v.created),
        current.and_then(|v| v.created),
    ) {
        if cand < curr {
            let secs = curr.signed_duration_since(cand).num_seconds().max(0) as u32;
            let diff = if secs < 3600 {
                format!("{}m", secs / 60)
            } else if secs < 86_400 {
                format!("{}h", secs / 3600)
            } else {
                format!("{}d", secs / 86_400)
            };
            out.push('\n');
            out.push_str(&format!(
                "⚠ candidate is {diff} older than the currently-deployed version — \
                 looks like a rollback. Confirm intent.\n"
            ));
        }
    }
    out.push_str("\nrun :deploy without --preview to dispatch, or :versions for the full list.\n");
    out
}

pub fn format_app_versions(
    versions: &[crate::aws::AppVersion],
    deployed_label: Option<&str>,
    limit: usize,
    ascii: bool,
) -> String {
    let mut out = String::new();
    let total = versions.len();
    let shown = total.min(limit);
    if total > limit {
        out.push_str(&format!(
            "showing {shown} of {total} (newest first; deploy older with `:deploy LABEL`)\n\n",
        ));
    }
    for v in versions.iter().take(limit) {
        // Drop the standard EB CI-pipeline prefix. The rest (usually a
        // pipeline URL) still distinguishes versions but consumes much less
        // horizontal width.
        let desc = v
            .description
            .strip_prefix("Application version created from ")
            .unwrap_or(&v.description);
        let deployed = deployed_label == Some(v.label.as_str());
        // Ascii icon-mode fallbacks — this is a pure text builder, so
        // the icons setting arrives as a bool from the caller.
        let marker = match (deployed, ascii) {
            (true, false) => "▶ ",
            (true, true) => "> ",
            (false, _) => "  ",
        };
        let suffix = match (deployed, ascii) {
            (true, false) => "  ◀ deployed",
            (true, true) => "  < deployed",
            (false, _) => "",
        };
        if desc.is_empty() {
            out.push_str(&format!("{marker}{}{}\n", v.label, suffix));
        } else {
            out.push_str(&format!("{marker}{}  {desc}{}\n", v.label, suffix));
        }
    }
    out.push('\n');
    out.push_str("Use `:deploy <label>` to ship one to the selected env.");
    out
}

/// Render a sorted `(namespace, option_name, value)` list as an aligned
/// text block grouped by namespace. Empty values render as `""` so the
/// reader can distinguish "explicitly empty" from "not present".
pub fn format_template_settings(settings: &[(String, String, String)]) -> String {
    if settings.is_empty() {
        return "(no option settings)".into();
    }
    let key_width = settings
        .iter()
        .map(|(_, name, _)| name.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(16, 40);
    let mut out = String::new();
    let mut prev_ns: Option<&str> = None;
    for (ns, name, value) in settings {
        if Some(ns.as_str()) != prev_ns {
            if prev_ns.is_some() {
                out.push('\n');
            }
            out.push_str(&format!("[{ns}]\n"));
            prev_ns = Some(ns.as_str());
        }
        let rendered = if value.is_empty() {
            "\"\"".to_string()
        } else {
            value.clone()
        };
        out.push_str(&format!("  {name:<key_width$} = {rendered}\n"));
    }
    out
}

/// Flatten the per-application configuration_templates lists into a single
/// `(application, template)` vector, sorted by app then by template name so
/// the overlay's cursor order is stable across refreshes. Pure so the unit
/// tests don't need an AWS client.
pub fn collect_saved_configs(apps: &[Application]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = apps
        .iter()
        .flat_map(|a| a.templates.iter().map(|t| (a.name.clone(), t.clone())))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    out
}

pub(crate) fn format_saved_configs(apps: &[Application]) -> String {
    if apps.is_empty() {
        return "no applications loaded — wait for first refresh or :region NAME".into();
    }
    let mut out = String::new();
    out.push_str("EB saved configurations (templates per application)\n");
    out.push_str("──────────────────────────────────────────────────\n\n");
    let mut any = false;
    for a in apps {
        if a.templates.is_empty() {
            continue;
        }
        any = true;
        out.push_str(&format!("Application: {}\n", a.name));
        for t in &a.templates {
            out.push_str(&format!("  ▸ {t}\n"));
        }
        out.push('\n');
    }
    if !any {
        out.push_str("no saved configuration templates in any application\n");
    }
    out
}

/// Render the `:ssm-run` overlay — one section per instance with
/// status / exit-code header, then stdout, then stderr if present.
/// Long buffers are line-truncated at 50 lines per stream (operator
/// can rerun with `tail -n 200 logfile` or similar if they need
/// more); per-line truncation at 200 chars keeps the overlay legible
/// when a single line is huge. Empty rows produce a stub.
pub(crate) fn format_ssm_results(command: &str, rows: &[crate::aws::SsmRunResult]) -> String {
    if rows.is_empty() {
        return format!("ssm-run — `{command}`\n\nNo instances targeted.\n\nesc / q to close");
    }
    const MAX_LINES_PER_STREAM: usize = 50;
    const MAX_LINE_CHARS: usize = 200;
    let truncate_line = |line: &str| -> String {
        if line.chars().count() <= MAX_LINE_CHARS {
            line.to_string()
        } else {
            let mut out: String = line.chars().take(MAX_LINE_CHARS - 1).collect();
            out.push('…');
            out
        }
    };
    let truncate_block = |block: &str| -> String {
        let lines: Vec<&str> = block.lines().collect();
        if lines.len() <= MAX_LINES_PER_STREAM {
            return lines
                .iter()
                .map(|l| truncate_line(l))
                .collect::<Vec<_>>()
                .join("\n");
        }
        let head = lines
            .iter()
            .take(MAX_LINES_PER_STREAM)
            .map(|l| truncate_line(l))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "{head}\n… ({} more lines truncated)",
            lines.len() - MAX_LINES_PER_STREAM
        )
    };
    let mut body = format!(
        "ssm-run — `{command}`\n\
         {} instance(s)\n\n",
        rows.len()
    );
    for r in rows {
        body.push_str(&format!(
            "─── {} [{}, exit={}] ───\n",
            r.instance_id, r.status, r.exit_code
        ));
        if r.stdout.is_empty() && r.stderr.is_empty() {
            body.push_str("  (no output)\n");
        }
        if !r.stdout.is_empty() {
            body.push_str("stdout:\n");
            body.push_str(&truncate_block(&r.stdout));
            body.push('\n');
        }
        if !r.stderr.is_empty() {
            body.push_str("stderr:\n");
            body.push_str(&truncate_block(&r.stderr));
            body.push('\n');
        }
        body.push('\n');
    }
    body.push_str("esc / q to close");
    body
}

/// Render the `:alarm-history` overlay — one row per history entry,
/// newest first, with timestamp + kind + summary. Kind is the API's
/// HistoryItemType (StateUpdate / ConfigurationUpdate / Action) and is
/// shown verbatim so an operator scanning the timeline can spot e.g.
/// `ConfigurationUpdate` entries that explain a state change. Empty
/// result yields a stub body so the operator isn't left wondering
/// whether the fetch silently failed.
pub(crate) fn format_alarm_history(
    alarm_name: &str,
    entries: &[crate::aws::AlarmHistoryEntry],
) -> String {
    if entries.is_empty() {
        return format!(
            "Alarm history — {alarm_name}\n\n\
             No history items in the recent window.\n\
             (CloudWatch retains alarm history for 90 days.)\n\n\
             esc / q to close"
        );
    }
    let mut body = format!(
        "Alarm history — {alarm_name}\n\
         {} entries, newest first.\n\n",
        entries.len()
    );
    for e in entries {
        let ts =
            e.at.map(|t| t.format("%Y-%m-%d %H:%M:%SZ").to_string())
                .unwrap_or_else(|| "—".into());
        body.push_str(&format!("{ts}  [{}]\n    {}\n\n", e.kind, e.summary));
    }
    body.push_str("esc / q to close");
    body
}

pub(crate) fn format_alarms(result: Result<Vec<CwAlarm>, String>) -> String {
    match result {
        Err(e) => format!("error fetching alarms: {e}"),
        Ok(alarms) if alarms.is_empty() => "no CloudWatch alarms reference this env".into(),
        Ok(alarms) => {
            let mut out = String::new();
            out.push_str(&format!("CloudWatch alarms ({})\n", alarms.len()));
            out.push_str("──────────────────────────────────────────\n\n");
            for a in alarms {
                out.push_str(&format!(
                    "{:<10} {} ({}/{})\n",
                    a.state, a.name, a.namespace, a.metric_name,
                ));
                if !a.state_reason.is_empty() {
                    // Pre-wrap the reason at a conservative column width
                    // with a hanging indent so continuation lines stay
                    // aligned. Avoids ratatui's auto-wrap dropping to
                    // column 0 which looks broken.
                    let lead = "           ↳ ";
                    let cont = "             ";
                    out.push_str(&wrap_with_hanging_indent(&a.state_reason, 100, lead, cont));
                    out.push('\n');
                }
                out.push('\n');
            }
            out
        }
    }
}

/// Compute the rollup for one application. Iterates `envs` once and
/// counts Red / Updating envs (case-insensitive on the health + status
/// columns). Worker-DLQ alerts come from `dlq_depths` which the App
/// owns globally — passed in so this stays a free fn that test code
/// can call without a full `App`.
/// Pure: render the `:promotions` overlay body. Newest-first
/// ordering so the most-recent promotions sit at the top of the
/// overlay (operators scan top-down).
pub fn render_promotions(
    records: &[PromotionRecord],
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let mut out = String::new();
    out.push_str("Promotion history (this session)\n");
    out.push_str("────────────────────────────────\n\n");
    // Group by version_label so consecutive promotions of the same
    // build chain naturally (v1.4.2: staging → uat → prod).
    let mut sorted: Vec<&PromotionRecord> = records.iter().collect();
    sorted.sort_by_key(|r| std::cmp::Reverse(r.at));
    for rec in sorted {
        let age = now
            .signed_duration_since(rec.at)
            .to_std()
            .unwrap_or(std::time::Duration::ZERO);
        out.push_str(&format!("  {} → {}\n", rec.source, rec.target));
        out.push_str(&format!(
            "    version={}  ({} ago)\n\n",
            rec.version_label,
            humanize_short_age(age)
        ));
    }
    out.push_str("esc / q to close");
    out
}

/// Pure: render the result of an IAM `SimulatePrincipalPolicy`
/// call. One section per evaluated action, with the decision +
/// matched statements + SCP / boundary blockers + a concrete
/// suggestion of what policy statement to add when the decision
/// was implicitDeny.
pub(crate) fn render_explain_overlay(
    principal: &str,
    rows: &[crate::aws::IamSimResult],
    truncated: bool,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("IAM diagnosis for {principal}\n"));
    out.push_str("═══════════════════════════════════════════════════\n\n");
    if truncated {
        // Up top, not at the bottom: this changes how every row below
        // it should be read, and an absent action is the finding an
        // operator is most likely to draw from a short table.
        out.push_str(
            "⚠ SimulatePrincipalPolicy hit its page budget — this table is
               INCOMPLETE. An action missing below was not evaluated, not
               necessarily allowed. Re-run `:explain` with fewer actions.

",
        );
    }
    if rows.is_empty() {
        out.push_str("(no evaluation results returned)\n\nesc / q to close");
        return out;
    }
    for (idx, r) in rows.iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        out.push_str(&format!("Action:   {}\n", r.action));
        if !r.resource.is_empty() {
            out.push_str(&format!("Resource: {}\n", r.resource));
        }
        let (mark, label) = match r.decision.as_str() {
            "allowed" => ("✓", "allowed"),
            "explicitDeny" => ("✗", "explicitDeny — a policy *denies* this action"),
            "implicitDeny" => ("✗", "implicitDeny — no policy allows this action"),
            other => ("?", other),
        };
        out.push_str(&format!("Decision: {mark} {label}\n"));
        if r.blocked_by_scp {
            out.push_str("          ⚠ also blocked by an Organizations SCP at the org level\n");
        }
        if r.blocked_by_boundary {
            out.push_str("          ⚠ also blocked by the role's permission boundary\n");
        }
        if !r.matched_statements.is_empty() {
            out.push_str("Matched statements:\n");
            for s in &r.matched_statements {
                out.push_str(&format!("  ▸ {s}\n"));
            }
        }
        if !r.missing_context.is_empty() {
            out.push_str("Missing context keys (conditions unsatisfied):\n");
            for c in &r.missing_context {
                out.push_str(&format!("  ▸ {c}\n"));
            }
        }
        if r.decision == "implicitDeny" {
            out.push_str(&format!(
                "\nTo allow, add this statement to one of the role's policies:\n\
                 \n\
                 {{\n\
                 \x20\x20\"Effect\": \"Allow\",\n\
                 \x20\x20\"Action\": \"{}\",\n\
                 \x20\x20\"Resource\": \"*\"\n\
                 }}\n",
                r.action
            ));
        } else if r.decision == "explicitDeny" {
            out.push_str(
                "\nAn explicit Deny in the matched statement(s) above is\n\
                 overriding any Allow. Remove or scope down the Deny to\n\
                 unblock — explicit Deny always wins.\n",
            );
        }
    }
    out.push_str("\nesc / q to close");
    out
}

/// Pure: render the env's underlying AWS resources as a tree.
/// Replaces the previous flat-section dump. The hierarchy mirrors
/// the conceptual graph an operator builds in their head:
///
///   env  (Tier)
///   ├─ ASGs
///   │  └─ <asg-name>
///   │     ├─ <instance-id>
///   │     └─ <instance-id>
///   ├─ Launch template / config
///   ├─ Load balancers
///   ├─ Triggers
///   └─ Queues  (Worker only)
///      ├─ WorkerQueue
///      │     https://sqs.../...
///      └─ WorkerDeadLetterQueue
///            https://sqs.../...
///
/// Instances are nested under ASGs because EB envs typically have
/// one ASG that owns every instance. The first ASG in the list
/// carries the instance children; if the env has zero ASGs but
/// non-zero instances (rare; mid-launch maybe), those instances
/// surface as a separate "orphan" section.
pub(crate) fn render_env_resources_tree(
    res: &crate::aws::EnvResources,
    env_name: &str,
    tier: &str,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("Resources for {env_name}  ({tier})\n"));
    out.push_str("═══════════════════════════════════════\n\n");

    // Collect non-empty sections first. The last kept section
    // uses `└─`; the rest `├─`. Easier to track once we know how
    // many sections survive than to count inline.
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();

    if !res.asgs.is_empty() {
        let mut lines: Vec<String> = Vec::new();
        let n_asgs = res.asgs.len();
        for (asg_idx, asg) in res.asgs.iter().enumerate() {
            let last_asg = asg_idx + 1 == n_asgs;
            let asg_prefix = if last_asg { "└─" } else { "├─" };
            lines.push(format!("  {asg_prefix} {asg}"));
            // Only the first ASG carries the instance children
            // (typical case: one ASG per env).
            if asg_idx == 0 && !res.instances.is_empty() {
                let n_inst = res.instances.len();
                let cont = if last_asg { "  " } else { "│ " };
                for (i, id) in res.instances.iter().enumerate() {
                    let last_inst = i + 1 == n_inst;
                    let glyph = if last_inst { "└─" } else { "├─" };
                    lines.push(format!("  {cont}   {glyph} {id}"));
                }
            }
        }
        sections.push((format!("Auto-scaling groups ({})", res.asgs.len()), lines));
    } else if !res.instances.is_empty() {
        let mut lines: Vec<String> = Vec::new();
        let n = res.instances.len();
        for (i, id) in res.instances.iter().enumerate() {
            let last = i + 1 == n;
            let glyph = if last { "└─" } else { "├─" };
            lines.push(format!("  {glyph} {id}"));
        }
        sections.push((format!("Instances ({n}) — orphan (no ASG attached)"), lines));
    }

    if !res.launch_templates.is_empty() {
        let mut lines: Vec<String> = Vec::new();
        let n = res.launch_templates.len();
        for (i, t) in res.launch_templates.iter().enumerate() {
            let glyph = if i + 1 == n { "└─" } else { "├─" };
            lines.push(format!("  {glyph} {t}"));
        }
        sections.push((format!("Launch templates ({n})"), lines));
    }
    if !res.launch_configs.is_empty() {
        let mut lines: Vec<String> = Vec::new();
        let n = res.launch_configs.len();
        for (i, lc) in res.launch_configs.iter().enumerate() {
            let glyph = if i + 1 == n { "└─" } else { "├─" };
            lines.push(format!("  {glyph} {lc}"));
        }
        sections.push((format!("Launch configurations ({n})"), lines));
    }
    if !res.load_balancers.is_empty() {
        let mut lines: Vec<String> = Vec::new();
        let n = res.load_balancers.len();
        for (i, lb) in res.load_balancers.iter().enumerate() {
            let glyph = if i + 1 == n { "└─" } else { "├─" };
            lines.push(format!("  {glyph} {lb}"));
        }
        sections.push((format!("Load balancers ({n})"), lines));
    }
    if !res.triggers.is_empty() {
        let mut lines: Vec<String> = Vec::new();
        let n = res.triggers.len();
        for (i, t) in res.triggers.iter().enumerate() {
            let glyph = if i + 1 == n { "└─" } else { "├─" };
            lines.push(format!("  {glyph} {t}"));
        }
        sections.push((format!("Triggers ({n})"), lines));
    }
    if !res.queues.is_empty() {
        let mut lines: Vec<String> = Vec::new();
        let n = res.queues.len();
        for (i, q) in res.queues.iter().enumerate() {
            let last = i + 1 == n;
            let glyph = if last { "└─" } else { "├─" };
            lines.push(format!("  {glyph} {}", q.name));
            if !q.url.is_empty() {
                let url_prefix = if last { "       " } else { "  │    " };
                lines.push(format!("{url_prefix}{}", q.url));
            }
        }
        sections.push((format!("Queues ({n})"), lines));
    }

    if sections.is_empty() {
        out.push_str("  (no resources reported — env may still be launching)\n");
    } else {
        let n_sections = sections.len();
        for (idx, (label, lines)) in sections.iter().enumerate() {
            let last_section = idx + 1 == n_sections;
            let section_glyph = if last_section { "└─" } else { "├─" };
            out.push_str(&format!("{section_glyph} {label}\n"));
            let prefix = if last_section { "  " } else { "│ " };
            for line in lines {
                out.push_str(&format!("{prefix}{line}\n"));
            }
            if !last_section {
                out.push_str("│\n");
            }
        }
    }

    out.push_str("\nesc / q to close");
    out
}

/// Render the `:secrets` overlay — metadata only, never values.
/// Pure (takes the SDK rows + filter, returns the body string) so
/// the table layout / empty-state copy can be unit-tested without
/// hitting Secrets Manager.
pub(crate) fn render_secrets_overlay(
    rows: &[crate::aws::SecretSummary],
    filter: Option<&str>,
) -> String {
    if rows.is_empty() {
        return match filter {
            Some(f) => format!(
                "No secrets matching '{f}'.\n\n\
                 `:secrets` (no arg) to see everything in this region.\n\
                 Secrets Manager is region-scoped — switch with `:region` first if needed.\n\n\
                 esc / q to close"
            ),
            None => "No Secrets Manager secrets in this region.\n\n\
                 Either none have been created, or the caller is missing\n\
                 `secretsmanager:ListSecrets`. Try `:explain :secrets` to check.\n\n\
                 esc / q to close"
                .to_string(),
        };
    }
    let now = chrono::Utc::now();
    let mut body = String::new();
    body.push_str(&match filter {
        Some(f) => format!(
            "Secrets Manager — {n} matching '{f}'\n\
             Sorted by last-changed (newest first). Values not shown — use `:secret NAME`.\n\n",
            n = rows.len()
        ),
        None => format!(
            "Secrets Manager — {n} secrets\n\
             Sorted by last-changed (newest first). Values not shown — use `:secret NAME`.\n\n",
            n = rows.len()
        ),
    });
    for r in rows {
        body.push_str(&format!("▸ {}\n", r.name));
        if !r.arn.is_empty() {
            body.push_str(&format!("    arn: {}\n", r.arn));
        }
        if let Some(d) = &r.description {
            body.push_str(&format!("    desc: {d}\n"));
        }
        let changed = r.last_changed.map(|t| format_age(now, t));
        let rotated = r.last_rotated.map(|t| format_age(now, t));
        match (changed, rotated) {
            (Some(c), Some(r)) => {
                body.push_str(&format!("    changed: {c}    rotated: {r}\n"));
            }
            (Some(c), None) => {
                body.push_str(&format!("    changed: {c}    rotated: never\n"));
            }
            (None, Some(r)) => {
                body.push_str(&format!("    rotated: {r}\n"));
            }
            (None, None) => {}
        }
        if let Some(k) = &r.kms_key_id {
            body.push_str(&format!("    kms: {k}\n"));
        }
        body.push('\n');
    }
    body.push_str(
        "y to yank an ARN (select first) · `:secret NAME` to read the value\n\
         esc / q to close",
    );
    body
}

/// Render the `:secret NAME` overlay — the single-secret detail view.
/// Honours `redact` mode by replacing the value with a length + sha
/// hint, so an operator on a screen-share can confirm "yes I have
/// the right secret" without exposing it. JSON-shaped values are
/// pretty-printed for readability (Secrets Manager's common k/v
/// idiom is `{"USERNAME":"…","PASSWORD":"…"}`).
pub(crate) fn render_secret_value_overlay(name: &str, value: &str, redact: bool) -> String {
    let mut body = String::new();
    body.push_str(&format!("Secret — {name}\n\n"));
    if redact {
        body.push_str(&format!(
            "value: <redacted; {} chars, fingerprint {}>\n\
             Run `:redact off` then re-fetch if you need the cleartext.\n\n\
             esc / q to close",
            value.chars().count(),
            short_fingerprint(value),
        ));
        return body;
    }
    // Try to pretty-print JSON so k/v secrets are scannable.
    let pretty = try_pretty_json(value);
    body.push_str("value:\n");
    body.push_str(&pretty);
    if !pretty.ends_with('\n') {
        body.push('\n');
    }
    body.push_str("\ny to yank the value · esc / q to close");
    body
}

/// FNV-1a 32-bit fingerprint of the value, hex-encoded — short,
/// dependency-free, good enough to confirm "same secret as before"
/// without leaking the value itself. NOT a cryptographic hash and
/// not used for security decisions; only for the redact-mode
/// "is this the right one" eyeball check.
pub(crate) fn short_fingerprint(s: &str) -> String {
    let mut h: u32 = 0x811C_9DC5;
    for b in s.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{h:08x}")
}

/// If the value parses as JSON, return a pretty-printed form;
/// otherwise return the raw string. Uses a very minimal recursive
/// parser instead of pulling in `serde_json` for one render path.
pub(crate) fn try_pretty_json(s: &str) -> String {
    let trimmed = s.trim();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return s.to_string();
    }
    // Minimal pass: walk chars, indenting on { [ and dedenting on } ].
    // Quoted strings are preserved verbatim. This handles the
    // Secrets-Manager k/v idiom without taking a hard JSON dep.
    let mut out = String::with_capacity(s.len() + 32);
    let mut depth: usize = 0;
    let mut in_str = false;
    let mut escape = false;
    let mut chars = trimmed.chars().peekable();
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '{' | '[' => {
                out.push(c);
                // Empty container — emit `{}` / `[]` inline. Consume
                // the closing bracket here so the `}`/`]` arm (which
                // would add its own newline + indent) never sees it.
                if matches!(chars.peek(), Some('}') | Some(']')) {
                    if let Some(close) = chars.next() {
                        out.push(close);
                    }
                    continue;
                }
                depth += 1;
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
                out.push(c);
            }
            ',' => {
                out.push(c);
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
            }
            ':' => {
                out.push(c);
                out.push(' ');
            }
            ' ' | '\n' | '\t' | '\r' => {} // collapse whitespace outside strings
            _ => out.push(c),
        }
    }
    out
}

pub(crate) fn describe_env(e: &Environment) -> String {
    let updated = e
        .updated
        .map(|u| u.to_rfc3339())
        .unwrap_or_else(|| "null".into());
    format!(
        "{{\n  \"name\":            \"{}\",\n  \"application\":     \"{}\",\n  \"tier\":            \"{}\",\n  \"status\":          \"{}\",\n  \"health\":          \"{}\",\n  \"platform\":        \"{}\",\n  \"version_label\":   \"{}\",\n  \"cname\":           \"{}\",\n  \"updated\":         {}\n}}",
        json_escape(&e.name),
        json_escape(&e.application),
        json_escape(&e.tier),
        json_escape(&e.status),
        json_escape(&e.health),
        json_escape(&e.platform),
        json_escape(&e.version_label),
        json_escape(&e.cname),
        if updated == "null" { updated } else { format!("\"{updated}\"") },
    )
}

pub(crate) fn redact_block(value: &str) -> String {
    if value.is_empty() {
        return value.to_string();
    }
    "▓".repeat(value.chars().count())
}

/// What to say when a worker env has no DLQ depth to show.
///
/// `dlq_stats == None` has three quite different causes and they used to
/// render identically as "(queue URL not resolved)":
///
///  - EB named no dead-letter queue and the `<main>-dlq` convention
///    guess didn't exist either. The ordinary case for an env without
///    one — but the operator should know a guess was made, or they
///    can't tell this from a misnamed queue.
///  - EB *did* name one and it doesn't exist. That is an anomaly worth
///    surfacing: something deleted a queue EB still references.
///  - There was no URL to try at all.
pub(crate) fn dlq_absence_note(dlq_url: Option<&str>, origin: Option<DlqOrigin>) -> String {
    let name = |u: &str| u.rsplit('/').next().unwrap_or(u).to_string();
    match (dlq_url, origin) {
        (Some(u), Some(DlqOrigin::Derived)) => {
            format!("no DLQ found — guessed '{}' by naming convention", name(u))
        }
        (Some(u), Some(DlqOrigin::Reported)) => {
            format!("EB names DLQ '{}' but it does not exist", name(u))
        }
        // Unreachable by construction — `fetch_worker_queues` sets the
        // origin at every site that sets the url, and
        // `a_dlq_url_always_carries_its_origin` pins that. The arm
        // exists because the match must be exhaustive over `Option`, so
        // it says the neutral thing rather than asserting a state the
        // fetcher cannot produce. (Nothing caches `WorkerQueues`, so
        // there is no stale-value path here either — I checked rather
        // than assuming, having first written a comment that invented
        // one.)
        (Some(u), None) => format!("DLQ '{}' returned no stats", name(u)),
        (None, _) => "no dead-letter queue configured".to_string(),
    }
}
