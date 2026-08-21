//! `ebman mcp setup` — print the MCP registration instructions.
//!
//! The secure alternative to "point your agent at a remote file and run
//! whatever it says". ebman is already installed from a signed source
//! (Homebrew / crates.io), so the commands this prints are trusted local
//! output — the same trust boundary as any installed CLI's `--help`.
//! It makes **no** network calls and writes **no** files (a `--client`
//! auto-writer that edits a client's config is a separate opt-in, tracked
//! in BACKLOG.md), so there's nothing to fetch, tamper with, or
//! auto-execute. `render` is pure so the wording stays unit-tested.

use color_eyre::eyre::Result;

const SETUP_USAGE: &str = "usage: ebman mcp setup [--allow-writes]";

/// Pure: the setup text. `allow_writes` swaps the headline command to the
/// write-enabled form, flips the `.mcp.json` args, and swaps the note.
pub(super) fn render(allow_writes: bool) -> String {
    let serve = if allow_writes {
        "ebman mcp serve --allow-writes"
    } else {
        "ebman mcp serve"
    };
    let json_args = if allow_writes {
        "[\"mcp\", \"serve\", \"--allow-writes\"]"
    } else {
        "[\"mcp\", \"serve\"]"
    };
    let mut s = String::new();
    s.push_str("Wire ebman into your coding agent over MCP.\n");
    s.push_str("ebman is already installed locally, so every command below is\n");
    s.push_str("local and inspectable — nothing is fetched or auto-executed.\n\n");
    s.push_str("Claude Code:\n");
    s.push_str(&format!("  claude mcp add ebman -- {serve}\n\n"));
    s.push_str("Any other MCP client — register a stdio server that runs the\n");
    s.push_str("command below. As a project-scoped .mcp.json:\n\n");
    s.push_str("  {\n");
    s.push_str("    \"mcpServers\": {\n");
    s.push_str(&format!(
        "      \"ebman\": {{ \"command\": \"ebman\", \"args\": {json_args} }}\n"
    ));
    s.push_str("    }\n");
    s.push_str("  }\n\n");
    if allow_writes {
        s.push_str("Writes are ON (--allow-writes): deploy / restart / rebuild /\n");
        s.push_str("terminate / set_option, each two-phase (a plan, then an explicit\n");
        s.push_str("confirm) and behind the same pins / read-only / incident freeze\n");
        s.push_str("as the TUI. Every dispatch is audit-logged.\n\n");
    } else {
        s.push_str("Reads only by default (list_environments, lint, drift, cost, …).\n");
        s.push_str("Re-run `ebman mcp setup --allow-writes` for the opt-in two-phase\n");
        s.push_str("write tools (deploy / restart / rebuild / terminate / set_option).\n\n");
    }
    s.push_str("If your shell exports AWS_REGION, pin it at registration — the\n");
    s.push_str("server takes the environment's region, not any project's:\n");
    s.push_str(&format!(
        "  claude mcp add ebman --env AWS_REGION=eu-west-1 -- {serve}\n\n"
    ));
    s.push_str("Full tool list and the writes contract: docs/headless.md (MCP section).\n");
    s
}

/// `args[0]` = `"mcp"`, `args[1]` = `"setup"`; the only flag is
/// `--allow-writes`. Prints to stdout and returns.
pub(super) fn run(args: &[String]) -> Result<()> {
    let mut allow_writes = false;
    for arg in args.iter().skip(2) {
        match arg.as_str() {
            "--allow-writes" => allow_writes = true,
            other => {
                eprintln!("ebman mcp setup: unknown flag '{other}' — {SETUP_USAGE}");
                std::process::exit(2);
            }
        }
    }
    print!("{}", render(allow_writes));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_only_by_default() {
        let s = render(false);
        assert!(s.contains("claude mcp add ebman -- ebman mcp serve\n"));
        assert!(s.contains("Reads only by default"));
        assert!(s.contains("\"args\": [\"mcp\", \"serve\"]"));
        // The read-only form must not pre-arm writes.
        assert!(!s.contains("serve --allow-writes"));
    }

    #[test]
    fn allow_writes_switches_command_json_and_note() {
        let s = render(true);
        assert!(s.contains("claude mcp add ebman -- ebman mcp serve --allow-writes"));
        assert!(s.contains("\"args\": [\"mcp\", \"serve\", \"--allow-writes\"]"));
        assert!(s.contains("Writes are ON"));
    }

    #[test]
    fn never_instructs_a_remote_fetch_or_auto_execute() {
        // The whole reason this command exists: no "read this URL and do
        // what it says". Guard it so a future edit can't reintroduce it.
        for aw in [false, true] {
            let lower = render(aw).to_lowercase();
            assert!(!lower.contains("http"), "no URLs / remote fetch");
            assert!(!lower.contains("follow it"), "no fetch-and-obey framing");
            assert!(!lower.contains("curl"), "no piped-remote-script install");
        }
    }

    #[test]
    fn region_pinning_is_documented() {
        assert!(render(false).contains("AWS_REGION=eu-west-1"));
    }
}
