//! `ebman <verb>` non-interactive subcommands.
//!
//! Pre-0.15 every `run_*_cli` lived as an inline `async fn` in
//! `src/main.rs`, which ballooned to 2,600+ lines as the CLI surface
//! grew (audit/explain/lint --fix all landed in 0.14). The 0.14
//! architecture review's #1 finding was the resulting grab-bag.
//!
//! Each verb now lives in its own file under `src/cli/`, exposing
//! `pub async fn run(args: &[String]) -> Result<()>`. `main.rs`
//! dispatches by `argv[1]` and calls the matching `cli::<verb>::run`.
//! Shared CLI-only helpers (the `decide_poll` state machine, the
//! `--fix` dispatch-failure flag, the JSON-string escaper, the
//! cli-arg escaper) live here in `mod.rs`.
//!
//! Convention:
//! - Each module is named after the subcommand (`audit.rs`,
//!   `explain.rs`, ...) and exports exactly one public function:
//!   `pub async fn run(args: &[String]) -> Result<()>`. `args` is
//!   the full `std::env::args()` vector so callers can index from
//!   `args[1]` onwards uniformly.
//! - Exit codes follow the 0.13 CLI charter (locked in
//!   `BACKLOG.md`): 0 ok, 1 aws err, 2 usage err, 3 issues / drift,
//!   4 wait-for-green timeout, 5 auto-rollback fired.
//! - No `println!` inside the TUI alternate screen — these
//!   subcommands run before / outside TUI lifecycle, so plain
//!   stdout/stderr is fine.

pub mod action;
pub mod audit;
pub mod audit_replay;
pub mod completions;
pub mod ctl;
pub mod drift;
pub mod envs;
pub mod explain;
pub mod lint;
pub mod mcp;
pub mod versions;

/// The canonical list of top-level `ebman <subcommand>` names — the
/// single source of truth for the CLI-subcommand *name* axis. `main.rs`
/// dispatches these (and lists them on an unknown-subcommand error);
/// `cli::completions` renders them, and a test pins its `SUBS` to this
/// list so the shell-completion subcommand set can't drift from the real
/// CLI. Per-subcommand flags / sub-verbs aren't mechanically derivable
/// and stay hand-maintained in `completions::SUBS`.
pub const SUBCOMMANDS: &[&str] = &[
    "envs",
    "action",
    "ctl",
    "lint",
    "drift",
    "audit",
    "mcp",
    "explain",
    "versions",
    "completions",
];

/// Re-exports from the shared deploy-poll module. CLI subcommand
/// modules import via `crate::cli::{decide_poll, PollDecision}`;
/// the actual implementations live in `src/deploy_poll.rs` and are
/// shared with the TUI's `spawn_rollout_dispatch`.
pub(crate) use crate::deploy_poll::{decide_poll, PollDecision};

/// Re-exports of the canonical JSON helpers from `crate::util`. CLI
/// subcommand modules import these via `crate::cli::{json_string,
/// cli_esc}` so call-site rewrites are unnecessary; the actual
/// implementations live in `util.rs` and are shared across the
/// crate (lib + bin).
pub(crate) use crate::util::{json_escape as cli_esc, json_string};

/// Cross-process freeze gate for CLI write paths (0.28): refuse when
/// a live TUI session holds `:freeze-deploys` / `:incident START`
/// (pid-scoped marker — see `crate::freeze`). Exit 3, same class as
/// the pin refusal. These paths had the same blind spot the MCP
/// write tools would have had: a fleet frozen mid-incident could
/// still be written from a second terminal.
pub(crate) fn refuse_if_frozen(prog: &str) {
    if let Some(m) = crate::freeze::read_active() {
        eprintln!("{prog}: refusing — {}", crate::freeze::refusal_message(&m));
        std::process::exit(3);
    }
}

/// The write gate: freeze first, then the config pin. Returns the
/// refusal message when the write must not proceed, `None` when clear.
///
/// This began life inside the MCP server, which had the right shape
/// already — a verdict over data passed in, "so the gate stays pure +
/// hermetically testable" — while the three other CLI write paths each
/// composed `refuse_if_frozen` and `pin_reason` by hand. All four DID
/// compose both, so there was no live hole; the problem was that
/// nothing made a fifth path do it. 0.14.1 was a same-day patch for
/// exactly that, `lint --fix` checking one and not the other.
///
/// Promoting the best of the four rather than writing a fifth. It takes
/// its inputs rather than reading them so the MCP server can keep
/// testing it hermetically; `refuse_write` below is the CLI's
/// read-the-world-and-exit wrapper.
pub(crate) fn write_refusal(
    safety_cfg: &crate::config::Config,
    env: &str,
    profile: &Option<String>,
    active_freeze: Option<crate::freeze::FreezeMarker>,
) -> Option<String> {
    // Cross-process freeze (the pid-scoped marker a live TUI session
    // persists for :freeze-deploys / :incident).
    if let Some(m) = active_freeze {
        return Some(crate::freeze::refusal_message(&m));
    }
    let pin_profile = profile
        .clone()
        .or_else(|| std::env::var("AWS_PROFILE").ok());
    if let Some(pin) = safety_cfg.pin_reason(env, pin_profile.as_deref()) {
        return Some(format!("refusing {env} — pinned by {pin}"));
    }
    None
}

/// CLI wrapper over [`write_refusal`]: read the world, print, exit 3.
///
/// `subject` is what the message names — usually the env, but
/// `audit replay` says "restart on api-prod", which is more useful and
/// worth keeping.
pub(crate) fn refuse_write(prog: &str, subject: &str, env: &str, profile: Option<&str>) {
    let profile = profile.map(str::to_string);
    if let Some(reason) = write_refusal(
        &crate::config::load(),
        env,
        &profile,
        crate::freeze::read_active(),
    ) {
        // `write_refusal` phrases the pin case as "refusing ENV — …";
        // for a caller naming something richer, say that instead.
        let reason = reason
            .strip_prefix(&format!("refusing {env} — "))
            .map(|r| format!("refusing {subject} — {r}"))
            .unwrap_or(reason);
        eprintln!("{prog}: {reason}");
        std::process::exit(3);
    }
}

/// Shared value-flag guard: reject a missing value or a following
/// flag consumed as one. Class fix from the 0.26 max-review — a
/// swallowed value silently changed semantics (`lint --fix --yes
/// --env` widened to the whole fleet; `--rules --json` disabled a CI
/// gate and ate the JSON flag).
pub(crate) fn take_value<'a, I: Iterator<Item = &'a String>>(
    iter: &mut I,
    prog: &str,
    flag: &str,
    what: &str,
) -> Result<String, String> {
    let Some(v) = iter.next() else {
        return Err(format!("{prog}: {flag} expects {what}"));
    };
    if v.starts_with("--") {
        return Err(format!("{prog}: {flag} expects {what}, got flag '{v}'"));
    }
    Ok(v.clone())
}

/// Exit a CLI command after draining in-flight audit-webhook POSTs —
/// `std::process::exit` (and returning from `#[tokio::main]`) cancels
/// spawned tasks, so a fire-and-forget outcome POST written just
/// before exit usually never left the machine. No-op when nothing is
/// in flight; bounded at slightly over the POST timeout.
pub(crate) async fn exit_after_drain(code: i32) -> ! {
    crate::audit::drain_webhooks(std::time::Duration::from_secs(12)).await;
    std::process::exit(code);
}

/// Drain in-flight webhook POSTs before a CLI command's Ok return —
/// same rationale as [`exit_after_drain`], for the success paths.
pub(crate) async fn drain_before_return() {
    crate::audit::drain_webhooks(std::time::Duration::from_secs(12)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    // decide_poll matrix tests live in `src/deploy_poll.rs`
    // alongside the function itself (0.16 move).

    #[test]
    fn cli_esc_escapes_quotes_and_backslashes() {
        assert_eq!(cli_esc("hello"), "hello");
        assert_eq!(cli_esc("a\"b"), "a\\\"b");
        assert_eq!(cli_esc("a\\b"), "a\\\\b");
        // Newlines + tabs (added in 0.15) are also escaped so the
        // value can land in any JSON context safely.
        assert_eq!(cli_esc("a\nb"), "a\\nb");
        assert_eq!(cli_esc("a\tb"), "a\\tb");
    }

    #[test]
    fn json_string_wraps_in_quotes_and_escapes() {
        assert_eq!(json_string(""), "\"\"");
        assert_eq!(json_string("hello"), "\"hello\"");
        assert_eq!(json_string("a\"b"), "\"a\\\"b\"");
        // Round-trip via the YAML-superset parser.
        let s = "line1\nline2 \"with quotes\"";
        let escaped = json_string(s);
        let parsed: String =
            serde_json::from_str(&escaped).expect("hand-rolled JSON must be valid JSON");
        assert_eq!(parsed, s);
    }
}

#[cfg(test)]
mod write_gate_guard {
    /// No CLI write path may reach for `pin_reason` directly.
    ///
    /// The freeze check and the pin check both existed and all four
    /// write paths called both — but each composed them by hand, and
    /// nothing made the fifth path do it. 0.14.1 was a same-day patch
    /// for exactly that: `lint --fix` checking one and not the other.
    ///
    /// Converging them on `write_refusal` only helps while they stay
    /// converged, and "everyone remembered" is what failed last time.
    /// This is the part that can't be forgotten.
    #[test]
    fn cli_write_paths_do_not_reach_past_the_shared_gate() {
        let mut offenders: Vec<String> = Vec::new();
        let mut stack = vec![std::path::PathBuf::from("src/cli")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("src/cli") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                // `mod.rs` defines the shared gate; it is allowed to
                // call `pin_reason` because it IS the composition.
                if path.file_name().and_then(|f| f.to_str()) == Some("mod.rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("read");
                // Stop at the inline test module — fixtures legitimately
                // exercise `pin_reason` directly.
                let prod = text.split("#[cfg(test)]").next().unwrap_or("");
                for (n, line) in prod.lines().enumerate() {
                    let code = line.split("//").next().unwrap_or("");
                    if code.contains(".pin_reason(") {
                        offenders.push(format!("{}:{}", path.display(), n + 1));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "these CLI paths call `pin_reason` directly instead of going \
             through `cli::write_refusal`, which also checks the freeze — \
             the exact half-composition 0.14.1 shipped: {offenders:?}"
        );
    }
}
