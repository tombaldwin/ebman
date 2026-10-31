//! `freeze_fleet`: the MCP tool that declares an `ebman freeze`. Not a
//! write verb (it only takes writes away, so every server offers it and
//! no grant covers it), which is why it is not in `writes.rs`.

use super::writes::*;
use super::*;

/// The tool that declares a fleet freeze. Not a verb: it only takes
/// writes away, so it is offered on every server and is not grantable.
pub(super) const FREEZE_TOOL: &str = "freeze_fleet";

/// `freeze_fleet`'s descriptor. Not among the write tools: it is offered
/// on every server, a `--read-only` one included, since it only takes
/// writes away (see the annotations tests' `ONLY_RESTRICTS`).
pub(super) fn freeze_tool_descriptor() -> Value {
    json!({
        "name": "freeze_fleet",
        "description": "Declare a fleet freeze: every ebman write refuses — the operator's TUI and CLI as well as this server — until the OPERATOR runs `ebman thaw`. Use it when you see a reason no one should change the fleet right now (a bad deploy rolling out, an incident starting) and say so to the operator. One step: no plan and no confirm, and on every connection, `--read-only` included, because it only takes writes away. CAVEATS: you cannot lift it, and no tool here can; it does not stop AWS changes made outside ebman (the console, the AWS CLI, CI). Persistent: it outlives this connection. Audited as `FreezeDeploys` (or `IncidentStart`) with `via=mcp`.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "reason": {"type": "string", "description": "Why, in a line (required, at most 200 characters): shown wherever the freeze refuses a write"},
                "incident": {"type": "boolean", "description": "Declare it as an incident (audited `IncidentStart`)"}
            },
            "required": ["reason"]
        }
    })
}

/// Longest client name a freeze marker records: the rest is cut, as
/// the name is whatever the client sent at initialize.
const FREEZE_AGENT_MAX: usize = 64;

impl Server {
    /// `freeze_fleet`: declare an `ebman freeze`, which every ebman
    /// process honours until the operator runs `ebman thaw`.
    ///
    /// One step, no plan and no ask, on every server, `--read-only`
    /// included: declaring a freeze only takes writes away, so it is the
    /// one change an agent may make that the operator need not approve
    /// (ruled 2026-10-29). Lifting is not offered here at all. Persistent, not
    /// tied to this server's process: a freeze that lifted when the
    /// agent disconnected would fail open mid-incident.
    pub(super) fn tool_freeze_fleet(&self, args: &Value) -> Result<String, String> {
        let reason = args
            .get("reason")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if reason.is_empty() {
            return Err(format!(
                "{FREEZE_TOOL}: `reason` is required — the operator who lifts the freeze \
                 needs to know why it is there"
            ));
        }
        match crate::freeze::check_reason(reason) {
            Ok(()) => {}
            Err(bad @ crate::freeze::BadReason::BreaksTheLine) => {
                return Err(format!("{FREEZE_TOOL}: `reason` {}", bad.what()));
            }
            Err(bad @ crate::freeze::BadReason::TooLong) => {
                return Err(format!(
                    "{FREEZE_TOOL}: `reason` {} — say why in a line; the detail belongs in \
                     your reply to the operator",
                    bad.what()
                ));
            }
        }
        let incident = args
            .get("incident")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if matches!(self.backend, Backend::Demo) {
            return Ok(json!({
                "frozen": false,
                "demo": true,
                "message": "DEMO server: nothing was frozen and no other process is affected.",
            })
            .to_string());
        }
        let client = self
            .client_name
            .lock()
            .map(|c| c.clone())
            .unwrap_or_default();
        // The operator just lifted a freeze: re-declaring it reverses
        // their decision, and a freeze refuses their own writes too,
        // the rollback that fixes what the agent saw included. Refused
        // for an `incident` too: the agent sets that flag. Audited, so
        // a loop shows in the log and the webhook channel.
        if let Some(t) = self.freeze.recently_thawed() {
            self.ensure_audit_wired();
            crate::audit::append_refused(
                None,
                self.ambient_profile.as_deref(),
                "-",
                &crate::audit::Refused {
                    action_label: crate::freeze::declare_label(incident),
                    target: "",
                    rule: "thawed_recently",
                    remedy: "the operator declares it with `ebman freeze`",
                    extras: &[("via", "mcp"), ("client", client.as_str())],
                },
            );
            let ago = (chrono::Utc::now() - t.thawed_at).num_minutes();
            return Err(format!(
                "{FREEZE_TOOL}: the operator lifted a fleet freeze ({}, {}) at {}, {ago} min \
                 ago; that decision holds until {} and this tool will not reverse it. If the \
                 fleet should be frozen, tell the operator now (`ebman freeze` is one step \
                 for them) and do not retry before then.",
                t.marker.reason_or_default(),
                t.marker.holder(),
                t.thawed_at.format("%H:%M UTC"),
                t.holds_until().format("%H:%M UTC"),
            ));
        }
        // One persistent freeze, and declaring writes it whole: an
        // agent's call would replace the operator's reason, incident
        // flag and time. A freeze already standing is left as declared.
        let already = |m: crate::freeze::FreezeMarker| {
            json!({
                "frozen": true,
                "already": true,
                "reason": m.reason,
                "incident": m.incident,
                "lift": "ebman thaw",
                "message": format!(
                    "The fleet is already frozen ({}), {} at {}; nothing changed. Every \
                     ebman write refuses until the operator runs `ebman thaw`. Tell the \
                     operator why you would have frozen it.",
                    m.reason_or_default(),
                    m.holder(),
                    m.at
                ),
            })
            .to_string()
        };
        // Declaring clears the note that an earlier freeze lifted with
        // its session, which is the operator's to see: hand it to the
        // agent, so it reaches them.
        let ended = self.freeze.ended().map(|e| e.message());
        // The marker names the client, so a refusal and `ebman thaw` say
        // an agent declared it. The name is the client's own claim, and
        // lands in a one-line status bar: sanitised and capped.
        let agent = match sanitize_capped(client.trim(), FREEZE_AGENT_MAX) {
            named if !named.is_empty() => named,
            _ => "an unnamed client".to_string(),
        };
        // One step: the declare is exclusive, so a standing freeze, one
        // an `ebman freeze` lands a moment before this, is kept and
        // answered. A check-then-declare let that one be rewritten.
        match self
            .freeze
            .declare_persistent_by_agent(reason, incident, &agent)
        {
            Ok(crate::freeze::AgentDeclare::Declared) => {}
            Ok(crate::freeze::AgentDeclare::Held(m)) => return Ok(already(m)),
            Err(e) => {
                return Err(format!(
                    "{FREEZE_TOOL}: could not write the freeze marker ({e}) — the fleet is NOT \
                     frozen"
                ));
            }
        }
        // A read-only server has not wired the webhook: this is the one
        // line it writes, and a freeze is what a team's channel most
        // needs to hear about.
        self.ensure_audit_wired();
        let mut extras = self.write_extras(&client, None, 0, None, None);
        extras.push(("reason", reason.to_string()));
        extras.push(("persistent", "true".to_string()));
        let extras: Vec<(&str, &str)> = extras.iter().map(|(k, v)| (*k, v.as_str())).collect();
        crate::audit::append_action_dispatched(
            None,
            self.ambient_profile.as_deref(),
            "-",
            crate::freeze::declare_label(incident),
            "",
            &extras,
        );
        Ok(json!({
            "frozen": true,
            "reason": reason,
            "incident": incident,
            "lift": "ebman thaw",
            "ended_before": ended,
            "message": format!(
                "Fleet frozen ({reason}): every ebman write now refuses — the TUI's, the \
                 CLI's and this server's — until the operator runs `ebman thaw`. You cannot \
                 lift it. Tell the operator you declared it, and why."
            ),
        })
        .to_string())
    }
}
