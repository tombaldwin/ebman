//! Help text for the subcommands: the SUBCOMMANDS section of
//! `ebman --help`, and `ebman SUB --help`, which prints that
//! subcommand's blocks from it. One text, so the two cannot disagree.
//!
//! Every subcommand answered `--help` with "unknown flag" and exit 2, and
//! `ebman ctl --help` tried the control socket; a script or a person
//! asking a subcommand what it takes got an error instead.

/// The SUBCOMMANDS section. Each block starts on a line indented four
/// spaces with the subcommand's name; its continuation lines are
/// indented further.
pub const SUBCOMMANDS_HELP: &str = r#"    envs [--json]                                List environments in current profile / region.
    lint [--env NAME] [--regions r1,r2,r3] [--json] [--severity LVL] [--rules ID1,ID2] [--quiet]
         [--fix (--yes | --dry-run)] [--watch [--interval 60s] [--webhook URL]] [--probe-live]
         [--baseline FILE | --against-baseline FILE]
                                                  Run the diagnostic rule engine against one env
                                                  (or every env in the context) and emit findings
                                                  as text or JSON. Non-zero exit when issues found.
                                                  Useful for git hooks, CI gates, monitoring loops.
                                                  Exit codes: 0 clean, 1 aws err, 2 usage, 3 issues
                                                  (or, with --fix --yes, a fix refused).
                                                  Operator disables via `lint.disable` in
                                                  config.toml and project-local .ebman/ebman.toml.
                                                  --regions fans out across regions; rows are
                                                  prefixed with the region.
                                                  --fix dispatches each rule's auto-remediation
                                                  (DeploymentPolicy → Rolling for EBL001, etc.).
                                                  Requires --yes to write; --dry-run prints the
                                                  plan without dispatching. Per-rule opt-out via
                                                  `lint.fix_disable`. Manual fixes printed as
                                                  instructions when the right answer is operator-
                                                  context-dependent.
                                                  --watch loops at --interval (default 60s) until
                                                  Ctrl-C; exit code reflects the LAST cycle.
                                                  Canonical monitoring shape:
                                                  `ebman lint --watch --interval 5m --json > alerts.jsonl`.
                                                  --watch and --fix are mutually exclusive.
                                                  --webhook URL (watch only) POSTs findings to
                                                  the URL when the issue set CHANGES between
                                                  cycles (Slack-shaped body; includes the
                                                  all-clear on dirty → clean).
                                                  --probe-live enables EBL016: one live HTTP HEAD
                                                  of each env's health-check URL (2s cap; off by
                                                  default to keep lint fast).
                                                  --baseline FILE snapshots current issues to JSON
                                                  (CI adoption: grandfather existing warnings).
                                                  --against-baseline FILE diffs vs the snapshot;
                                                  exits 3 only on NEW issues, cleared ones are
                                                  informational. Identity is (rule_id, env_name,
                                                  fields); title / detail drift doesn't churn.
    drift [--env NAME] [--regions r1,r2,r3] [--tfstate PATH] [--tfdir PATH] [--json] [--quiet] [--no-redact]
                                                  Terraform drift report. Discovers tfstate via
                                                  walk-up from cwd (or --tfdir / --tfstate
                                                  overrides). Compares tf-declared option settings
                                                  + version_label against live EB state. Drifted
                                                  env-var values + DBPassword are redacted by
                                                  default; --no-redact shows them verbatim.
                                                  Exit codes: 0 no drift, 1 aws err, 2 usage,
                                                  3 drift detected. CI-friendly default exit code.
                                                  --regions fans out across regions against a
                                                  single tfstate (multi-region tf projects).
    action ACTION --env NAME [--yes]             Run an action (rebuild|restart|terminate|deploy|rollout) on an env.
                                                  Terminate requires --yes to confirm.
                                                  Deploy requires --version LABEL; supports
                                                  --wait-for-green Nm and --auto-rollback Nm.
                                                  Rollout: --version LABEL --regions r1,r2,r3 --env NAME
                                                  --yes [--wait-for-green Nm] [--json] [--profile P]
                                                  [--parallel [--max-concurrency N]]
                                                  [--continue-on-fail] [--staggered Nm].
                                                  Default: sequential, halt on first failure.
                                                  --parallel fans out concurrently (implies
                                                  --continue-on-fail since in-flight regions
                                                  can't be cancelled). --staggered delays
                                                  between regions in sequential mode (requires
                                                  --wait-for-green). Single rollout_id
                                                  correlation across audit lines.
                                                  Exit codes: 0 ok, 1 aws err, 2 usage, 3 refused,
                                                  halted or partly failed, 4 wait-timeout,
                                                  5 rolled-back.
    ctl <screen|key|cmd|state|reload> [args]     Talk to a running ebman via --control-socket.
                                                  `reload` re-execs the binary (rebuild first via
                                                  `cargo build --release`). Use --socket PATH to
                                                  override the default location.
    audit [--tail] [--since DUR] [--env NAME] [--rule ID] [--action NAME] [--json]
                                                  Read ~/.cache/ebman/audit.log — surface the local
                                                  audit trail for scripting / Slack-bot routing /
                                                  CI gating. Default text mode renders columns
                                                  (TS / REGION / STAGE / ACTION / TARGET / OUTCOME);
                                                  --json emits JSONL one entry per line. --tail
                                                  polls 1s for new entries (until Ctrl-C). --since
                                                  filters to entries within a duration (5m/1h/2d).
                                                  Exit codes: 0 ok, 1 io err, 2 usage.
    mcp serve [--demo] [--no-redact] [--allow-writes[=VERBS] | --read-only]
                                                  Stdio MCP server for Claude Code etc.: the read
                                                  surface (envs / lint / option settings / drift /
                                                  audit / events / versions / cost) as agent tools.
                                                  WRITES: on a client that can ask you (Claude Code
                                                  can), EVERY write verb is available — deploy,
                                                  restart, rebuild, terminate, set_option and the
                                                  dlq verbs — each two-phase and put to you as a
                                                  confirmation you can decline. On a client that
                                                  cannot ask, reads only. --read-only keeps it to
                                                  reads whatever the client supports;
                                                  --allow-writes=VERBS names the only verbs allowed.
                                                  Pins + freeze + read-only are enforced either way.
                                                  get_option_settings redacts env-var values +
                                                  DBPassword (--no-redact opts out). --demo serves
                                                  the synthetic fleet with zero AWS calls.
                                                  Register: claude mcp add ebman -- ebman mcp serve
    mcp setup [--allow-writes[=VERBS]]           Print the MCP registration commands (claude mcp add
                                                  + a .mcp.json snippet for other clients) from the
                                                  installed binary — no network, no remote fetch.
                                                  --allow-writes shows the write-enabled form.
    audit replay LINE_ID [--yes]                 Re-dispatch a previously-audited action. LINE_ID
                                                  is a prefix of the line's RFC3339 timestamp (the
                                                  first `ebman audit` column); ambiguous prefixes
                                                  are refused with candidates listed. Supported
                                                  actions: Rebuild / Restart / Terminate / Deploy
                                                  (others refuse — the line doesn't carry enough to
                                                  reconstruct them). Honours safety.envs.* /
                                                  safety.accounts.* pins; Terminate needs --yes.
                                                  Exit codes: 0 ok, 1 aws err, 2 usage/no-match/
                                                  ambiguous, 3 pin-refused or destructive-gate.
    explain EBL### [--env NAME] [--json] [--dry-run] [--no-cache]
                                                  LLM-backed explanation of a lint issue. Routes to
                                                  the configured Provider (Anthropic API or local
                                                  Ollama) and prints an operator-readable summary
                                                  of why the issue matters and what to do next.
                                                  Requires `explain.enabled = true` in
                                                  config.toml + an exported ANTHROPIC_API_KEY
                                                  (Anthropic) or a running Ollama server. Responses
                                                  cached to ~/.cache/ebman/explain/; --no-cache
                                                  forces a fresh call. --dry-run prints the prompt
                                                  without sending. Exit codes: 0 ok, 1 provider err,
                                                  2 usage, 3 issue not found.
    versions --env NAME [--json]                 List application versions for the env's app,
                                                  newest-first. CLI mirror of the TUI `:versions`
                                                  overlay. Useful for CI scripts that want to
                                                  validate a candidate label exists before
                                                  `ebman action deploy`, or to surface the
                                                  candidate's description / age in a Slack notify.
                                                  --json emits one object per version with
                                                  {label, deployed (bool), created (RFC3339),
                                                  description}. Default text mode marks the
                                                  currently-deployed label with `*`. Exit codes:
                                                  0 ok, 1 aws err, 2 usage.
    freeze [--incident] [REASON...]              Freeze every ebman write fleet-wide (TUI, CLI, MCP)
                                                  until `ebman thaw`, whatever is running: unlike
                                                  :freeze-deploys, it does not end with a TUI
                                                  session. --status says what is in force, and any
                                                  TUI freeze that ended with its session in the last
                                                  day. A reason starting with `-` goes after `--`.
                                                  Exit codes: 0 ok, 1 not written (NOT frozen),
                                                  2 usage.
    thaw                                         Lift a freeze declared with `ebman freeze`. Refuses
                                                  (exit 3) a live TUI session's freeze, which that
                                                  session lifts. Exit codes: 0 lifted or nothing to
                                                  lift, 2 usage, 3 still frozen.
    completions <bash|zsh|fish>                  Emit a shell completion script to stdout. Static:
                                                  subcommands + flags, not live env names. Install:
                                                  zsh  -> ebman completions zsh  > "${fpath[1]}/_ebman"
                                                  bash -> ebman completions bash > ~/.local/share/bash-completion/completions/ebman
                                                  fish -> ebman completions fish > ~/.config/fish/completions/ebman.fish"#;

/// `name`'s blocks, from [`SUBCOMMANDS_HELP`]: every block whose header
/// starts with it (`audit` and `audit replay`; `mcp serve` and
/// `mcp setup`). `None` for a name with none.
pub fn subcommand_help(name: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    let mut inside = false;
    for line in SUBCOMMANDS_HELP.lines() {
        let header = line.strip_prefix("    ").filter(|r| !r.starts_with(' '));
        if let Some(rest) = header {
            inside = rest.split_whitespace().next() == Some(name);
        }
        if inside {
            out.push(line);
        }
    }
    (!out.is_empty()).then(|| out.join("\n"))
}

/// The words that name a form of `sub` (`audit replay`, `mcp serve`,
/// `ctl state`), read from its help headers: a header's second word, or
/// each choice in a `<a|b|c>` there. Derived, so a new form cannot be
/// left out, and per subcommand, so `ebman freeze state --help` declares
/// a freeze rather than printing help.
fn forms_of(sub: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in SUBCOMMANDS_HELP.lines() {
        let Some(rest) = line.strip_prefix("    ").filter(|r| !r.starts_with(' ')) else {
            continue;
        };
        let mut words = rest.split_whitespace();
        if words.next() != Some(sub) {
            continue;
        }
        let Some(second) = words.next() else {
            continue;
        };
        if let Some(choices) = second.strip_prefix('<').and_then(|c| c.strip_suffix('>')) {
            out.extend(choices.split('|').map(str::to_string));
        } else if second.chars().all(|c| c.is_ascii_lowercase()) {
            out.push(second.to_string());
        }
    }
    out
}

/// Does `args` (the subcommand's own, `args[0]` being its name) ask for
/// help? Only in the flag position: first, or second after one of that
/// subcommand's form words. So a value that happens to be `--help` (a
/// freeze reason after another word) is never read as one.
pub fn asks_for_help(args: &[String]) -> bool {
    let is_help = |a: Option<&String>| matches!(a.map(String::as_str), Some("-h" | "--help"));
    let form = |w: &String| {
        args.first()
            .is_some_and(|sub| forms_of(sub).iter().any(|f| f == w))
    };
    is_help(args.get(1)) || (args.get(1).is_some_and(form) && is_help(args.get(2)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_subcommand_has_help() {
        for sub in crate::cli::SUBCOMMANDS {
            let text = subcommand_help(sub).unwrap_or_else(|| panic!("no help for `{sub}`"));
            assert!(text.contains(sub), "{sub}: {text}");
        }
    }

    /// The other direction from `every_subcommand_has_help`: every block
    /// is a subcommand's, so a removed one's help cannot linger.
    #[test]
    fn every_help_block_is_a_subcommands() {
        for line in SUBCOMMANDS_HELP.lines() {
            let Some(rest) = line.strip_prefix("    ").filter(|r| !r.starts_with(' ')) else {
                continue;
            };
            let name = rest.split_whitespace().next().unwrap_or_default();
            assert!(
                crate::cli::SUBCOMMANDS.contains(&name),
                "help block for `{name}`, which is not a subcommand"
            );
        }
    }

    #[test]
    fn a_subcommand_with_two_forms_prints_both() {
        let audit = subcommand_help("audit").unwrap();
        assert!(
            audit.contains("audit [--tail]") && audit.contains("audit replay"),
            "{audit}"
        );
        let mcp = subcommand_help("mcp").unwrap();
        assert!(
            mcp.contains("mcp serve") && mcp.contains("mcp setup"),
            "{mcp}"
        );
        // And not the next subcommand's block.
        assert!(!audit.contains("    explain"), "{audit}");
        assert_eq!(subcommand_help("nope"), None);
    }

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn help_is_asked_for_only_in_the_flag_position() {
        assert!(asks_for_help(&argv(&["lint", "--help"])));
        assert!(asks_for_help(&argv(&["versions", "-h"])));
        assert!(asks_for_help(&argv(&["audit", "replay", "--help"])));
        assert!(asks_for_help(&argv(&["mcp", "serve", "--help"])));
        assert!(asks_for_help(&argv(&["ctl", "state", "--help"])));
        // A reason that happens to be the word, after another word, even
        // one that is another subcommand's form.
        assert!(!asks_for_help(&argv(&["freeze", "release", "--help"])));
        assert!(!asks_for_help(&argv(&["freeze", "state", "--help"])));
        assert!(asks_for_help(&argv(&["ctl", "reload", "--help"])));
        assert!(!asks_for_help(&argv(&["lint", "--env", "x"])));
        assert!(!asks_for_help(&argv(&["envs"])));
    }
}
