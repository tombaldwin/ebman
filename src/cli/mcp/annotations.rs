// MCP tool annotations — and, deliberately, the start of the action
// vocabulary the protection-levels design needs.
//
// Two jobs in one table. For MCP clients these are the standard
// `ToolAnnotations` hints, so any client can tell a read from a
// destructive write without parsing prose and can ask for consent
// accordingly. For `docs/design/protection-levels.md` they are the
// per-action attributes a levels engine evaluates over — "reversible
// writes on low-stakes resources" is only expressible if actions carry
// attributes. Doing it once serves both.
//
// The spec is explicit that these are HINTS and must not be relied on
// for security. That matches the design note's first principle: IAM is
// the boundary, this is a guardrail against mistakes.

use serde_json::{json, Value};

/// How one tool behaves, in the terms MCP defines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ToolAttrs {
    /// Does not modify its environment.
    pub read_only: bool,
    /// May perform destructive updates. Meaningful only when
    /// `read_only` is false; the spec says to treat it as false for
    /// read-only tools.
    pub destructive: bool,
    /// Calling twice with the same arguments has the same effect as
    /// calling once.
    pub idempotent: bool,
    /// Interacts with entities the server does not control.
    pub open_world: bool,
}

impl ToolAttrs {
    const fn read() -> Self {
        Self {
            read_only: true,
            destructive: false,
            idempotent: true,
            // True for every tool here: they observe an AWS account
            // whose contents change without ebman's involvement. A
            // closed domain would be something like a scratchpad the
            // server owns outright.
            open_world: true,
        }
    }
    const fn write(destructive: bool, idempotent: bool) -> Self {
        Self {
            read_only: false,
            destructive,
            idempotent,
            open_world: true,
        }
    }
}

/// Every tool, classified. The table is the point: a tool absent from
/// it fails `every_tool_is_classified`, so a new one cannot ship
/// without someone deciding what it does.
pub(super) const TOOL_ATTRS: &[(&str, ToolAttrs)] = &[
    // ── reads ──
    ("list_environments", ToolAttrs::read()),
    ("recent_events", ToolAttrs::read()),
    ("get_option_settings", ToolAttrs::read()),
    ("list_versions", ToolAttrs::read()),
    ("lint", ToolAttrs::read()),
    ("drift", ToolAttrs::read()),
    ("fleet_cost", ToolAttrs::read()),
    ("audit_log", ToolAttrs::read()),
    // ── writes ──
    //
    // `restart` bounces the app servers: downtime, but nothing is
    // destroyed and the end state after two restarts is the state after
    // one.
    ("restart", ToolAttrs::write(false, true)),
    // `rebuild` terminates and recreates every instance. The
    // environment survives, but the instances do not — the confirm
    // modal says so in as many words, and an operator who expected
    // `restart` would be unpleasantly surprised.
    ("rebuild", ToolAttrs::write(true, true)),
    // Deploying a version replaces what is running; the previous
    // version is still deployable, so this is reversible rather than
    // destructive. Deploying the same label twice lands in the same
    // place.
    ("deploy", ToolAttrs::write(false, true)),
    // Same reasoning: a setting can be set back.
    ("set_option", ToolAttrs::write(false, true)),
    // The environment is gone and is not coming back.
    ("terminate", ToolAttrs::write(true, true)),
    //
    // `confirm_action` is the second half of the two-phase write flow,
    // and it dispatches whatever is pending — which may be a
    // terminate. So it is annotated at its WORST case, not its average:
    // a client that reads `destructive: false` here and skips the
    // consent prompt would skip it for the one call that most needs it.
    //
    // Not idempotent, and this is the one place that differs from the
    // others: the token is single-use, so confirming twice is not the
    // same as confirming once.
    ("confirm_action", ToolAttrs::write(true, false)),
];

/// Look up a tool's attributes.
pub(super) fn attrs_for(name: &str) -> Option<ToolAttrs> {
    TOOL_ATTRS.iter().find(|(n, _)| *n == name).map(|(_, a)| *a)
}

/// The `annotations` object for a tool descriptor, or `None` if the
/// tool is unclassified — in which case emitting nothing is right. A
/// wrong hint is worse than an absent one: a client that trusts
/// `readOnlyHint: true` on a write will not ask before dispatching it.
pub(super) fn annotations_for(name: &str) -> Option<Value> {
    let a = attrs_for(name)?;
    Some(json!({
        "readOnlyHint": a.read_only,
        "destructiveHint": a.destructive,
        "idempotentHint": a.idempotent,
        "openWorldHint": a.open_world,
    }))
}

/// Attach `annotations` to every descriptor in a tool array that has a
/// classification.
pub(super) fn annotate(tools: &mut Value) {
    let Some(arr) = tools.as_array_mut() else {
        return;
    };
    for tool in arr {
        let Some(name) = tool.get("name").and_then(Value::as_str).map(str::to_string) else {
            continue;
        };
        if let Some(ann) = annotations_for(&name) {
            if let Some(obj) = tool.as_object_mut() {
                obj.insert("annotations".into(), ann);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every advertised tool carries annotations.
    ///
    /// This is the guard that makes the table worth having: a new tool
    /// cannot ship without someone deciding whether it reads or writes
    /// and whether it destroys anything. Checked against the REAL
    /// `tools/list` output — a table that agrees only with itself would
    /// prove nothing.
    #[test]
    fn every_advertised_tool_is_classified() {
        for allow_writes in [false, true] {
            let table = super::super::tools::tool_table(allow_writes);
            let arr = table.as_array().expect("tools/list is an array");
            assert!(
                arr.len() >= 8,
                "only {} tools — the table failed to build and this guard \
                 would pass on an empty result",
                arr.len()
            );
            for tool in arr {
                let name = tool["name"].as_str().expect("tool has a name");
                let ann = tool.get("annotations").unwrap_or_else(|| {
                    panic!(
                        "`{name}` is advertised with no annotations — add it \
                         to TOOL_ATTRS and decide what it does"
                    )
                });
                // All four hints present. A partial object is worse than
                // none: a client reading only `readOnlyHint` and finding
                // it absent may assume the default.
                for key in [
                    "readOnlyHint",
                    "destructiveHint",
                    "idempotentHint",
                    "openWorldHint",
                ] {
                    assert!(ann.get(key).is_some(), "`{name}` is missing {key}");
                }
            }
        }
    }

    #[test]
    fn reads_are_marked_read_only_and_writes_are_not() {
        // The distinction the whole feature exists for. Asserted over
        // the real tables in both modes, so a tool moving between them
        // cannot keep a stale hint.
        let reads = super::super::tools::tool_table(false);
        for tool in reads.as_array().expect("array") {
            let name = tool["name"].as_str().expect("name");
            assert_eq!(
                tool["annotations"]["readOnlyHint"], true,
                "`{name}` is in the read-only table but is not marked read-only"
            );
        }

        let with_writes = super::super::tools::tool_table(true);
        let write_names: Vec<&str> = with_writes
            .as_array()
            .expect("array")
            .iter()
            .filter(|t| t["annotations"]["readOnlyHint"] == false)
            .map(|t| t["name"].as_str().expect("name"))
            .collect();
        assert!(
            write_names.len() >= 5,
            "expected the write surface to be marked; got {write_names:?}"
        );
        // And enabling writes only ADDS: no read tool changed its hint.
        assert_eq!(
            with_writes.as_array().expect("array").len(),
            reads.as_array().expect("array").len() + write_names.len()
        );
    }

    #[test]
    fn the_destructive_tools_are_the_ones_that_destroy_something() {
        // Named individually rather than counted. `terminate` and
        // `rebuild` destroy; `restart`, `deploy` and `set_option` are
        // reversible and must NOT be flagged, or the hint means nothing
        // and a client learns to ignore it.
        for t in ["terminate", "rebuild"] {
            assert!(
                attrs_for(t).expect(t).destructive,
                "`{t}` destroys something and must say so"
            );
        }
        for t in ["restart", "deploy", "set_option"] {
            assert!(
                !attrs_for(t).expect(t).destructive,
                "`{t}` is reversible; flagging it teaches clients to ignore \
                 the hint on the ones that are not"
            );
        }
    }

    #[test]
    fn confirm_action_is_annotated_at_its_worst_case() {
        // It dispatches whatever is pending, which may be a terminate.
        // A client trusting `destructive: false` here would skip the
        // consent prompt on precisely the call that most needs one.
        let a = attrs_for("confirm_action").expect("classified");
        assert!(!a.read_only, "it dispatches a write");
        assert!(a.destructive, "it may dispatch a terminate");
        // Single-use token: confirming twice is not confirming once.
        assert!(!a.idempotent);
    }

    #[test]
    fn an_unclassified_tool_gets_no_annotations_rather_than_wrong_ones() {
        // A wrong hint is worse than an absent one — a client that
        // trusts `readOnlyHint: true` on a write will not ask before
        // dispatching it. So the lookup returns None rather than a
        // default.
        assert!(annotations_for("no_such_tool").is_none());
        assert!(attrs_for("no_such_tool").is_none());
    }
}
