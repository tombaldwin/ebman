//! Unit tests for `cli::mcp::tools` — the tool registry and dispatch.
//!
//! Moved verbatim out of `tools.rs`. Attached with `#[path]` so the
//! module keeps its parent — and with it `use super::*` and access to
//! `tools`'s private items — while the file lives beside the other
//! MCP tests. Same reason as `tests/writes.rs`: re-parenting would
//! have cost `pub(crate)` on items that have no business being
//! crate-visible.
//!
//! Pairs with `tests/tools_renderer.rs`, which was the sibling
//! `renderer_tests` module in the same file.

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
    // From the crate root, not relative to this file — moving these
    // tests into `tests/` made `include_str!("tools.rs")` a read of
    // THIS file, which still compiles and still finds `call_tool`
    // mentioned in its own assertions.
    let src = crate::app::tests::scan::production_source("cli/mcp/tools.rs");
    let src = src.as_str();
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
    let advertised = names_in(&tool_table(&crate::cli::mcp::WriteScope::All, true));

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
    let read_only = names_in(&tool_table(&crate::cli::mcp::WriteScope::None, true));
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
    let enabled = names_in(&tool_table(&crate::cli::mcp::WriteScope::All, true));
    for w in &writes {
        assert!(enabled.contains(w), "{w} missing under --allow-writes");
    }
}
