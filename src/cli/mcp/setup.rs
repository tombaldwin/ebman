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

use super::WriteScope;
use color_eyre::eyre::Result;

const SETUP_USAGE: &str = "usage: ebman mcp setup [--allow-writes[=verb,verb]]";

/// Pure: the setup text. The scope swaps the headline command, the
/// `.mcp.json` args, and the note.
pub(super) fn render(scope: &WriteScope) -> String {
    let flag = match scope {
        WriteScope::None => String::new(),
        WriteScope::All => " --allow-writes".into(),
        WriteScope::Only(v) => format!(" --allow-writes={}", v.join(",")),
    };
    let serve = format!("ebman mcp serve{flag}");
    let json_args = match scope {
        WriteScope::None => "[\"mcp\", \"serve\"]".to_string(),
        WriteScope::All => "[\"mcp\", \"serve\", \"--allow-writes\"]".to_string(),
        WriteScope::Only(v) => format!("[\"mcp\", \"serve\", \"--allow-writes={}\"]", v.join(",")),
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
    match scope {
        WriteScope::All => {
            // The verb list is derived, not typed out. It was typed
            // out, and went stale the moment the three DLQ verbs
            // shipped: this text still offered five tools while the
            // server advertised eight.
            s.push_str(&format!(
                "Writes are ON for every verb ({}), each two-phase (a plan,\n",
                super::writes::write_verb_names().join(" / ")
            ));
            s.push_str("then an explicit confirm) and behind the same pins / read-only /\n");
            s.push_str("incident freeze as the TUI. Every dispatch is audit-logged.\n\n");
            s.push_str("To grant less, name the verbs you need:\n");
            s.push_str("  ebman mcp setup --allow-writes=dlq_resend,dlq_delete\n\n");
        }
        WriteScope::Only(v) => {
            s.push_str(&format!(
                "Writes are ON for {} ONLY. Every other write verb is neither\n",
                v.join(" / ")
            ));
            s.push_str("advertised nor dispatchable by this server. Each is two-phase (a\n");
            s.push_str("plan, then an explicit confirm), behind the same pins / read-only /\n");
            s.push_str("incident freeze as the TUI, and audit-logged.\n\n");
        }
        WriteScope::None => {
            s.push_str("Reads only by default (list_environments, lint, drift, cost, …).\n");
            s.push_str("Re-run with --allow-writes for the opt-in two-phase write tools,\n");
            s.push_str("or name just the ones you want:\n");
            s.push_str(&format!(
                "  ebman mcp setup --allow-writes={}\n",
                super::writes::write_verb_names().join(",")
            ));
            s.push_str("  ebman mcp setup --allow-writes=dlq_resend,dlq_delete\n\n");
        }
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
    let known: Vec<String> = super::writes::write_verb_names();
    let known_refs: Vec<&str> = known.iter().map(String::as_str).collect();
    let mut scope = WriteScope::None;
    let mut saw_write_flag = false;
    for arg in args.iter().skip(2) {
        if let Some(rest) = arg.strip_prefix("--allow-writes") {
            // Same reasoning as `serve`: last-wins would let a stray
            // second flag widen a narrow grant in silence, and this
            // command's output is pasted into a config and lived with.
            if saw_write_flag {
                eprintln!(
                    "ebman mcp setup: --allow-writes given more than once — a second \
                     one would silently widen the first. Name every verb in one flag: \
                     --allow-writes=a,b"
                );
                std::process::exit(2);
            }
            saw_write_flag = true;
            let value = match rest {
                "" => None,
                v => match v.strip_prefix('=') {
                    Some(v) => Some(v),
                    None => {
                        eprintln!("ebman mcp setup: unknown flag '{arg}' — {SETUP_USAGE}");
                        std::process::exit(2);
                    }
                },
            };
            // Same fail-closed parse as `serve`. A typo here is worse
            // than one there: this text gets pasted into a .mcp.json
            // and lived with, so a scope that silently meant something
            // else would be discovered weeks later.
            match super::parse_write_scope(value, &known_refs) {
                Ok(s) => scope = s,
                Err(e) => {
                    eprintln!("ebman mcp setup: {e}");
                    std::process::exit(2);
                }
            }
            continue;
        }
        eprintln!("ebman mcp setup: unknown flag '{arg}' — {SETUP_USAGE}");
        std::process::exit(2);
    }
    print!("{}", render(&scope));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_only_by_default() {
        let s = render(&WriteScope::None);
        assert!(s.contains("claude mcp add ebman -- ebman mcp serve\n"));
        assert!(s.contains("Reads only by default"));
        assert!(s.contains("\"args\": [\"mcp\", \"serve\"]"));
        // The read-only form must not pre-arm writes.
        assert!(!s.contains("serve --allow-writes"));
    }

    #[test]
    fn allow_writes_switches_command_json_and_note() {
        let s = render(&WriteScope::All);
        assert!(s.contains("claude mcp add ebman -- ebman mcp serve --allow-writes"));
        assert!(s.contains("\"args\": [\"mcp\", \"serve\", \"--allow-writes\"]"));
        assert!(s.contains("Writes are ON"));
    }

    /// Every shape `render` can produce. A guard that loops over two
    /// of three variants reads as covering the surface while leaving
    /// the newest one — the one most likely to be wrong — untested.
    fn all_scopes() -> Vec<WriteScope> {
        vec![
            WriteScope::None,
            WriteScope::All,
            WriteScope::Only(vec!["dlq_delete".into(), "dlq_resend".into()]),
        ]
    }

    #[test]
    fn never_instructs_a_remote_fetch_or_auto_execute() {
        // The whole reason this command exists: no "read this URL and do
        // what it says". Guard it so a future edit can't reintroduce it.
        for scope in all_scopes() {
            let lower = render(&scope).to_lowercase();
            assert!(!lower.contains("http"), "no URLs / remote fetch");
            assert!(!lower.contains("follow it"), "no fetch-and-obey framing");
            assert!(!lower.contains("curl"), "no piped-remote-script install");
        }
    }

    #[test]
    fn region_pinning_is_documented() {
        assert!(render(&WriteScope::None).contains("AWS_REGION=eu-west-1"));
    }

    /// `mcp setup` must never tell anyone to fetch and run something.
    ///
    /// The output's own promise is "every command below is local and
    /// inspectable — nothing is fetched or auto-executed", and an MCP
    /// user singled it out as the reason they could trust the setup
    /// path. A promise like that is worth exactly as much as whatever
    /// stops it quietly becoming false, and nothing did: the existing
    /// tests pin the read/write variants and the JSON shape, not this.
    ///
    /// Pins the PROPERTY rather than the sentence, so rewording the
    /// prose is fine and adding a `curl | sh` is not — which is the
    /// direction it would actually drift as the surface grows.
    #[test]
    fn setup_never_asks_anyone_to_fetch_and_run() {
        for scope in all_scopes() {
            let s = render(&scope);

            // A remote-fetch-and-execute, in the shapes it comes in.
            for pattern in [
                "curl",
                "wget",
                "| sh",
                "|sh",
                "| bash",
                "|bash",
                "iwr",
                "Invoke-WebRequest",
                "source <(",
                "eval $(",
                "http://",
                "https://",
            ] {
                assert!(
                    !s.contains(pattern),
                    "`mcp setup` output contains {pattern:?}, which breaks its own \
                     promise that nothing is fetched or auto-executed \
                     (scope={scope:?}):\n{s}"
                );
            }

            // And the promise itself must still be made — a guard that
            // only forbids patterns would pass on an empty string.
            assert!(
                s.contains("nothing is fetched or auto-executed"),
                "the promise must be stated, not merely kept: {s}"
            );
            assert!(
                s.contains("already installed locally"),
                "and the reason it holds — the binary is already here: {s}"
            );
        }
    }

    /// The canary: the scan above must be able to see an offender.
    #[test]
    fn the_fetch_and_run_scan_can_see_one() {
        let bad = "install with: curl https://example.test/i.sh | sh";
        assert!(bad.contains("curl"), "detector sees the fetcher");
        assert!(bad.contains("https://"), "detector sees the URL");
        assert!(bad.contains("| sh"), "detector sees the pipe-to-shell");
        let good = "claude mcp add ebman -- ebman mcp serve";
        for pattern in ["curl", "wget", "| sh", "https://"] {
            assert!(
                !good.contains(pattern),
                "the legitimate form must not trip the detector"
            );
        }
    }

    /// A narrow grant produces a `.mcp.json` that IS narrow.
    ///
    /// The output of this command is pasted into a config and lived
    /// with, so a scope that printed the bare `--allow-writes` flag
    /// would hand the operator every verb while telling them they had
    /// two. Pins the flag in both the headline and the JSON args —
    /// they are built separately and only one of them is read by the
    /// agent's client.
    #[test]
    fn a_narrow_grant_is_narrow_in_both_the_command_and_the_json() {
        let scope = WriteScope::Only(vec!["dlq_resend".into(), "dlq_delete".into()]);
        let s = render(&scope);
        assert!(
            s.contains("ebman mcp serve --allow-writes=dlq_resend,dlq_delete"),
            "the headline command must carry the scope: {s}"
        );
        assert!(
            s.contains("\"--allow-writes=dlq_resend,dlq_delete\""),
            "and so must the .mcp.json args, which is what actually runs: {s}"
        );
        assert!(
            !s.contains("\"--allow-writes\""),
            "the bare flag would silently widen the grant to everything: {s}"
        );
        assert!(
            s.contains("ONLY"),
            "and the prose must say the rest is unavailable: {s}"
        );
        for ungranted in ["terminate", "set_option"] {
            assert!(
                !s.contains(ungranted),
                "`{ungranted}` was not granted and must not be offered: {s}"
            );
        }
    }

    /// The write-verb list in the prose is derived, never typed.
    ///
    /// It was typed, and went stale the moment `dlq_resend` /
    /// `dlq_delete` / `dlq_purge` shipped — this text kept offering
    /// five verbs while the server advertised eight, so an operator
    /// reading it had no way to learn the DLQ tools existed.
    #[test]
    fn the_full_grant_lists_every_verb_the_server_advertises() {
        let s = render(&WriteScope::All);
        for verb in super::super::writes::write_verb_names() {
            assert!(
                s.contains(&verb),
                "`{verb}` is advertised by the server but missing from setup: {s}"
            );
        }
    }
}
