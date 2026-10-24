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
pub mod freeze;
pub mod help;
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
    "freeze",
    "thaw",
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

/// The cross-process fleet freeze (0.28), for CLI write paths that
/// read it themselves: a live TUI session's `:freeze-deploys` /
/// `:incident START`, or an `ebman freeze` in force. These paths had the
/// same blind spot the MCP write tools would have had: a fleet frozen
/// mid-incident could still be written from a second terminal.
///
/// A marker, not a refusal: `lint --fix` hands it to the shared gate
/// per env, which audits and words the refusal as for any other write.
pub(crate) fn active_freeze() -> Option<crate::freeze::FreezeMarker> {
    crate::freeze::FreezeStore::real().active()
}

/// The `stage=refused` line for a write the fleet freeze stopped: the
/// freeze's rule and remedy, as the gate would give them.
pub(crate) fn audit_frozen(
    profile: Option<&str>,
    region: &str,
    action_label: &str,
    target: &str,
    extras: &[(&str, &str)],
) {
    let refusal = crate::write_gate::Refusal::Frozen;
    crate::audit::append_refused(
        None,
        profile,
        region,
        &crate::audit::Refused {
            action_label,
            target,
            rule: refusal.rule(),
            remedy: &refusal.remedy(),
            extras,
        },
    );
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
/// testing it hermetically; [`real_refusal`] below reads the world for
/// the CLI's write bodies, whose shells drain before exiting 3.
pub(crate) fn write_refusal(
    safety_cfg: &crate::config::Config,
    env: &str,
    profile: &Option<String>,
    active_freeze: Option<crate::freeze::FreezeMarker>,
    region: Option<&str>,
    action_label: &str,
) -> Option<String> {
    let (refusal, message, pin_profile) =
        write_refusal_unaudited(safety_cfg, env, profile, active_freeze)?;
    // Record the attempt. Every CLI and MCP write path funnels through
    // here, so this is the one place that sees a refusal on this side.
    //
    // The region is whatever the caller could honestly resolve. These
    // refusals happen BEFORE any AWS client is built, and the rules
    // (freeze, env pin, account pin) are region-independent anyway, so
    // an unknown region is recorded as unknown rather than guessed at
    // as home — a line filed against the wrong region is worse than one
    // that admits it does not know.
    crate::audit::append_action_refused(
        None,
        pin_profile.as_deref(),
        region.unwrap_or("-"),
        action_label,
        env,
        refusal.rule(),
        &refusal.remedy(),
    );
    Some(message)
}

/// Decide and render, with NO side effect — **no audit line**.
///
/// Named for what it omits. This is the half a new enforcement path
/// must NOT reach for: a refusal that leaves no `stage=refused` line is
/// a silent regression to the pre-0.37 blind spot, where a blocked
/// write and no attempt at all looked identical. `write_refusal_paths_are_audited`
/// pins the two callers that are legitimately audit-free.
///
/// Split out because `--demo` must reach the same verdict while writing
/// nothing: a demo MCP server still reads the real cross-process freeze
/// marker, so a demo write attempt during a live incident was appending
/// a real line to the real audit log. Demo's contract is that it touches
/// nothing real, and the refusal being genuine does not change that.
///
/// Returns the refusal, its rendered message, and the profile the pin
/// was resolved against.
pub(crate) fn write_refusal_unaudited(
    safety_cfg: &crate::config::Config,
    env: &str,
    profile: &Option<String>,
    active_freeze: Option<crate::freeze::FreezeMarker>,
) -> Option<(crate::write_gate::Refusal, String, Option<String>)> {
    // The profile fallback is resolved HERE rather than inside the
    // decision, which used to read `AWS_PROFILE` itself — an ambient
    // read that made it impossible to test without touching the process
    // environment. `write_gate::decide` now takes values only.
    let pin_profile = profile
        .clone()
        .or_else(|| std::env::var("AWS_PROFILE").ok());

    let refusal = crate::write_gate::decide(&crate::write_gate::WriteContext {
        env,
        profile: pin_profile.as_deref(),
        // The CLI has no session toggle, but it does honour the
        // STANDING one. This used to pass `false` unconditionally,
        // which was correct while the rung was TUI-only and became a
        // hole the moment `safety.read_only` existed: an operator who
        // forbade writes in config would still have had them from
        // `ebman action`.
        safety_parse_errors: &safety_cfg.safety_parse_errors,
        global_read_only: safety_cfg.safety_read_only,
        frozen: active_freeze.is_some(),
        safety_envs: &safety_cfg.safety_envs,
        safety_accounts: &safety_cfg.safety_accounts,
    })?;

    // Wording stays the CLI's own — see `write_gate`'s module docs.
    let message = match &refusal {
        crate::write_gate::Refusal::SafetyConfigUnreadable { problem } => {
            format!("refusing {env} — safety config unreadable: {problem}")
        }
        crate::write_gate::Refusal::Frozen => match active_freeze.as_ref() {
            // Named like every other arm: `lint --fix` refuses env by
            // env, and a bare freeze sentence repeated per env said
            // nothing about which ones were skipped.
            Some(m) => format!("refusing {env} — {}", crate::freeze::refusal_message(m)),
            // Unreachable — `frozen` was set from this very `Option` —
            // but `?` here would propagate `None`, and `None` from this
            // function means ALLOW. A fail-open branch in a write gate
            // is not worth the brevity.
            None => format!("refusing {env} — deploys frozen"),
        },
        crate::write_gate::Refusal::EnvPinned { env: e } => {
            format!("refusing {env} — pinned by safety.envs.{e}.read_only")
        }
        crate::write_gate::Refusal::AccountPinned { profile: p } => {
            format!("refusing {env} — pinned by safety.accounts.{p}.read_only")
        }
        // Reachable since `safety.read_only` shipped. It was not
        // before, and the comment here said so — which made this the
        // one arm whose message never had to name a control, because
        // nothing could reach it. On this path the source is
        // unambiguous: the CLI has no session toggle, so it can only
        // be the config key.
        crate::write_gate::Refusal::GlobalReadOnly => {
            format!("refusing {env} — safety.read_only is set in config.toml")
        }
    };
    Some((refusal, message, pin_profile))
}

/// One question to a CLI write gate: may `verb` (the audit label) write
/// to `env` under `profile`, in `region`? Named fields, because profile
/// and region are both `Option<&str>` and swapped positionally they
/// would check the pins against the wrong thing and still compile.
pub(crate) struct GateQuery<'a> {
    pub env: &'a str,
    pub profile: Option<&'a str>,
    pub region: Option<&'a str>,
    pub verb: &'a str,
}

/// What decides whether a CLI write may go: the refusal, if any. A
/// testable body takes one, so a test sees a refusal without the real
/// config or freeze; production passes [`real_refusal`]. (Not
/// `write_gate::Refusal`, the reason a gate refused; this answers with
/// the message.)
pub(crate) trait WriteGate: Fn(&GateQuery<'_>) -> Option<String> {}
impl<T: Fn(&GateQuery<'_>) -> Option<String>> WriteGate for T {}

/// [`write_refusal`] against the operator's config and the live freeze:
/// the audited funnel every CLI write goes through.
pub(crate) fn real_refusal(q: &GateQuery<'_>) -> Option<String> {
    write_refusal(
        &crate::config::load(),
        q.env,
        &q.profile.map(str::to_string),
        crate::freeze::FreezeStore::real().active(),
        q.region,
        q.verb,
    )
}

/// A refusal from [`write_refusal`] naming `subject` rather than the
/// bare env: it phrases the pin case as "refusing ENV — …", and a
/// caller dispatching something richer ("Restart on ENV") says that.
pub(crate) fn refusal_naming(reason: String, env: &str, subject: &str) -> String {
    reason
        .strip_prefix(&format!("refusing {env} — "))
        .map(|r| format!("refusing {subject} — {r}"))
        .unwrap_or(reason)
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
    /// What counts as reaching past the gate: the raw pin maps, or the
    /// decision function directly (which would skip this module's
    /// wording and its freeze composition).
    ///
    /// Extracted so the guard can DEMONSTRATE that it detects, rather
    /// than passing because the tree happens to be clean. Disabling the
    /// scan used to leave the suite green — the guard only ever fired
    /// if someone introduced a violation, which is a guard you are
    /// trusting on assertion.
    fn reaches_past_the_gate(code: &str) -> bool {
        code.contains("safety_envs")
            || code.contains("safety_accounts")
            || code.contains("write_gate::decide")
    }

    #[test]
    fn the_gate_guard_detects_what_it_is_looking_for() {
        // The canary. Runs on every invocation, so a scan that has gone
        // blind fails here rather than passing quietly over a clean
        // tree.
        assert!(reaches_past_the_gate("if cfg.safety_envs.get(env) {"));
        assert!(reaches_past_the_gate("cfg.safety_accounts.contains_key(p)"));
        assert!(reaches_past_the_gate("crate::write_gate::decide(&ctx)"));
        // And does not flag the legitimate route.
        assert!(!reaches_past_the_gate(
            "if let Some(r) = write_refusal(&cfg, env, &p, f) {"
        ));
        assert!(!reaches_past_the_gate(
            "real_refusal(&GateQuery { env, profile, region, verb })"
        ));
    }

    /// The non-auditing half of the gate has exactly two callers.
    ///
    /// Splitting `write_refusal` into a pure half and an auditing funnel
    /// solved a real problem (demo mode was writing real audit lines),
    /// and opened a new one: `cli_write_paths_do_not_reach_past_the_shared_gate`
    /// scans for the pin maps and `decide`, none of which the pure half
    /// mentions — so a future enforcement path could call it, refuse
    /// correctly, and leave no trace, passing every guard. That is the
    /// "nothing made the fifth path do it" shape the sibling guard
    /// exists for, one level down.
    ///
    /// Pinned by COUNT, not just by file, so a second call appearing in
    /// an allowed file still fails. Both legitimate callers are
    /// non-dispatching by construction: a `--fix` preview and a demo
    /// server. If a third appears, the question to answer is why it
    /// refuses without recording, not how to make this list longer.
    #[test]
    fn write_refusal_paths_are_audited() {
        const ALLOWED: &[(&str, usize, &str)] = &[
            (
                "src/cli/lint.rs",
                1,
                "a --fix dry run dispatched nothing; recording refusals \
                 of writes that were never going to happen is noise",
            ),
            (
                "src/cli/mcp/writes.rs",
                1,
                "`Server::gate_refusal` uses the unaudited half for BOTH \
                 backends and records through `Audited::record`, so this \
                 module has exactly one place that writes a refusal line \
                 and a `WriteError::Refused` is proof it ran. Demo still \
                 writes nothing real — the refusal is genuine, the fleet \
                 is not",
            ),
        ];

        let mut found: Vec<(String, usize)> = Vec::new();
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
                // `mod.rs` declares it.
                if path.file_name().and_then(|f| f.to_str()) == Some("mod.rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("read");
                let prod = crate::app::tests::scan::production_half(&text);
                let n = prod.matches("write_refusal_unaudited(").count();
                if n > 0 {
                    found.push((path.display().to_string(), n));
                }
            }
        }
        found.sort();

        let mut expected: Vec<(String, usize)> = ALLOWED
            .iter()
            .map(|(p, n, _)| ((*p).to_string(), *n))
            .collect();
        expected.sort();

        assert_eq!(
            found, expected,
            "the non-auditing gate gained or lost a caller. A refusal that \
             writes no `stage=refused` line is invisible — the exact blind \
             spot 0.37 closed. Justify the new site before listing it."
        );
    }

    /// The canary: this guard must be able to fail.
    ///
    /// It counts occurrences in a clean tree, so without this it would
    /// pass identically if the needle stopped matching anything.
    #[test]
    fn the_unaudited_gate_guard_can_see_its_needle() {
        let sample = "let r = crate::cli::write_refusal_unaudited(&cfg, env, &p, None);";
        assert_eq!(sample.matches("write_refusal_unaudited(").count(), 1);
        assert_eq!(
            "crate::cli::write_refusal(&cfg, env, &p, None, None, \"X\")"
                .matches("write_refusal_unaudited(")
                .count(),
            0,
            "the auditing funnel must not be counted as the unaudited one"
        );
    }

    #[test]
    fn cli_write_paths_do_not_reach_past_the_shared_gate() {
        let mut offenders: Vec<String> = Vec::new();
        // Through `scan::source_files`, not a hand-rolled walk. This
        // one skipped inline test modules via `production_half` but had
        // no notion of a test FILE, so the moment the mcp tests moved
        // into `cli/mcp/tests/` it started reporting fixtures as
        // offenders. `is_test_path` is where that question is answered
        // once for every guard.
        let mut scanned = 0usize;
        for (path, text) in crate::app::tests::scan::source_files() {
            if !path.starts_with("src/cli") {
                continue;
            }
            if crate::app::tests::scan::is_test_path(&path) {
                continue;
            }
            scanned += 1;
            // THIS file defines the shared gate; it is allowed to
            // reach the safety config because it IS the composition.
            //
            // Exactly this file. The exemption used to be any
            // `mod.rs`, which is broader than the sentence above and
            // silently excused all 1,715 lines of `cli/mcp/mod.rs` —
            // CLI-write-adjacent code, and the largest single file the
            // guard covers. Clean today; exempted for no stated
            // reason.
            if path == "src/cli/mod.rs" {
                continue;
            }
            // Stop at the inline test module — fixtures legitimately
            // exercise `pin_reason` directly.
            let prod = crate::app::tests::scan::production_half(&text);
            for (n, line) in prod.lines().enumerate() {
                let code = crate::app::tests::scan::strip_line_comment(line);
                // Widened when `pin_reason` was folded into
                // `write_gate::decide`. The old form scanned for a
                // single method name, which would have become
                // decorative the moment that method was deleted —
                // a guard that cannot fire reads as coverage.
                //
                // These are what a CLI write path must not touch:
                // the raw pin maps, or the decision function
                // directly (which would skip this module's wording
                // and its freeze composition).
                if reaches_past_the_gate(code) {
                    offenders.push(format!("{path}:{}", n + 1));
                }
            }
        }
        // A floor, because filtering a list has no bottom of its own.
        // The hand-rolled walk this replaced opened with
        // `read_dir("src/cli").expect(..)` and PANICKED if the tree
        // moved; a filter that matches nothing just leaves `offenders`
        // empty and reports clean. That is the wrong failure for a
        // guard whose whole job is stopping a CLI write path from
        // bypassing `write_refusal`.
        assert!(
            scanned > 10,
            "only {scanned} CLI sources scanned — the filter has stopped \
             matching and this guard is passing over nothing"
        );
        assert!(
            offenders.is_empty(),
            "these CLI paths reach the safety config directly instead of going \
             through `cli::write_refusal`, which also checks the freeze — \
             the exact half-composition 0.14.1 shipped: {offenders:?}"
        );
    }
}

#[cfg(test)]
mod write_refusal_tests {
    use super::write_refusal;
    use crate::config::Config;

    #[test]
    fn an_account_pin_is_resolved_against_the_profile_passed_in() {
        // The hole this pins: `ebman action rollout --profile prod-admin`
        // dispatched under `prod-admin` while the gate was handed the
        // ambient `AWS_PROFILE`. With that unset or different, the pin
        // on `prod-admin` was never consulted — on a multi-region deploy
        // fan-out, the biggest write the CLI has.
        //
        // The gate was correct all along; it was being fed the wrong
        // input, which is why the convergence guard could not see it —
        // that guard detects a path *bypassing* the gate, not one
        // calling it with the wrong account.
        let mut cfg = Config::default();
        cfg.safety_accounts.insert("prod-admin".into(), true);

        let refused = write_refusal(
            &cfg,
            "api-prod",
            &Some("prod-admin".into()),
            None,
            None,
            "Test",
        );
        assert!(
            refused.is_some_and(|r| r.contains("prod-admin")),
            "a pinned account must refuse when it is the profile the write runs under"
        );

        // A different profile is not pinned, and must not be refused —
        // over-refusing would be its own bug.
        assert_eq!(
            write_refusal(&cfg, "api-prod", &Some("dev".into()), None, None, "Test"),
            None
        );
    }
}

#[cfg(test)]
mod write_gate_input_guard {
    /// Every write shell hands its body the real gate and the ambient
    /// profile.
    ///
    /// The bodies take both as parameters so a test can stand them in,
    /// which means nothing the tests drive can see what production
    /// passes: a shell handing `|_, _, _, _| None` gates nothing, and one
    /// handing `None` for `AWS_PROFILE` checks every account pin against
    /// no account — the defect the deleted rollout `--profile` scan
    /// existed for. Pinned by source because the shells exit the process.
    #[test]
    fn write_shells_pass_the_real_gate_and_the_ambient_profile() {
        for (module, f, gate) in [
            ("cli/action.rs", "run", "crate::cli::real_refusal,"),
            ("cli/action.rs", "run_rollout", "crate::cli::real_refusal,"),
            (
                "cli/audit_replay.rs",
                "run_replay",
                "crate::cli::real_refusal,",
            ),
            ("cli/lint.rs", "run", "crate::cli::active_freeze,"),
        ] {
            let src = crate::app::tests::scan::production_source(module);
            let body = crate::app::tests::scan::fn_bodies(&src)
                .into_iter()
                .find(|(name, _)| name == f)
                .unwrap_or_else(|| panic!("{module}: `{f}` is defined"))
                .1;
            // The body call's own arguments, not anywhere in the shell: a
            // shell reading AWS_PROFILE for something else while passing
            // `None` must not pass.
            let call = &body[body
                .find("_body(")
                .unwrap_or_else(|| panic!("{f} calls a body"))..];
            let args = &call[..call.find("\n    )").unwrap_or(call.len())];
            assert!(
                args.contains(gate),
                "{module}: `{f}` must pass `{gate}` to its body: {args}"
            );
            assert!(
                args.contains("std::env::var(\"AWS_PROFILE\").ok()"),
                "{module}: `{f}` must pass the ambient AWS_PROFILE: {args}"
            );
        }
    }

    /// Every process-exiting refusal path must drain webhooks first.
    ///
    /// `fire_webhook` is spawned fire-and-forget and `process::exit`
    /// cancels spawned tasks, so the notification that a write was
    /// STOPPED was the one an integration never received — while the
    /// notification that one SUCCEEDED arrived, because the success
    /// paths already drain. The write bodies return their refusal's 3,
    /// so the exit is their shell's; pinned by source because the
    /// alternative is exiting the test process.
    #[test]
    fn refusal_exits_drain_webhooks_first() {
        for (module, f, calls) in [
            ("cli/lint.rs", "run", "lint_body("),
            ("cli/action.rs", "run", "action_body("),
            ("cli/action.rs", "run_rollout", "rollout_body("),
            ("cli/audit_replay.rs", "run_replay", "audit_replay_body("),
            ("cli/freeze.rs", "run_freeze", "freeze_body("),
            ("cli/freeze.rs", "run_thaw", "thaw_body("),
        ] {
            let src = crate::app::tests::scan::production_source(module);
            let body = crate::app::tests::scan::fn_bodies(&src)
                .into_iter()
                .find(|(name, _)| name == f)
                .unwrap_or_else(|| panic!("{module}: `{f}` is defined"))
                .1;
            assert!(
                body.contains("exit_after_drain("),
                "{f} must drain before exiting, or the refusal's webhook \
                 dies with the process: {body}"
            );
            assert!(
                !body.contains("std::process::exit("),
                "{f} must not bypass the draining exit: {body}"
            );
            // And the success path drains too: a write that went through
            // has its webhook POST in flight as surely as a refusal.
            assert!(
                body.contains("drain_before_return()"),
                "{f} must drain before a successful return: {body}"
            );
            // Canary: prove the body extraction found the real one.
            assert!(
                body.contains(calls),
                "{module}: `{f}` was not found: {body}"
            );
            // Replay is the one write `main` wires no webhook for (its
            // `audit` arm reads), so its shell does — before the body,
            // or the body's refusal line is written with none to reach.
            if f == "run_replay" {
                let init = body
                    .find("init_from_config_disk(")
                    .expect("run_replay must wire the webhook its audit lines fire");
                assert!(
                    init < body.find(calls).unwrap(),
                    "{module}: `{f}` wires the webhook after its body has run"
                );
            }
        }
    }

    /// `safety.read_only` reaches the CLI, not just the TUI.
    ///
    /// This path passed `global_read_only: false` unconditionally,
    /// which was correct while the rung was TUI-only and became a hole
    /// the moment the config key existed: an operator who forbade
    /// writes in `config.toml` would still have had them from `ebman
    /// action`. Closed, and pinned here because a mutation showed the
    /// fix was unpinned — the suite stayed green with the old `false`
    /// restored.
    #[test]
    fn a_standing_read_only_refuses_cli_writes() {
        let cfg = crate::config::Config {
            safety_read_only: true,
            ..crate::config::Config::default()
        };

        let refusal = super::write_refusal_unaudited(&cfg, "any-env", &None, None);
        let (rule, message, _) = refusal.expect("a standing read-only must refuse");
        assert_eq!(rule.rule(), "global_read_only", "{message}");

        // The control: without it, this env is writeable. Without this
        // the test would pass against a gate that refuses everything.
        let open = crate::config::Config::default();
        assert!(
            super::write_refusal_unaudited(&open, "any-env", &None, None).is_none(),
            "no standing restriction means no refusal"
        );
    }
}

/// What a CLI write body wrote to the (test-sandboxed) audit log.
#[cfg(test)]
pub(crate) mod audit_delta {
    fn log() -> String {
        std::fs::read_to_string(crate::util::cache_dir().join("audit.log")).unwrap_or_default()
    }

    /// The log as it stands, to compare against after a run.
    pub(crate) fn mark() -> String {
        log()
    }

    /// The lines written since `before` that `keep` accepts, for a line
    /// with no target of its own to filter on.
    pub(crate) fn lines_matching(before: &str, keep: impl Fn(&str) -> bool) -> Vec<String> {
        let after = log();
        let delta = after
            .strip_prefix(before)
            .expect("the audit log is append-only; a non-prefix means it was rotated mid-test");
        delta
            .lines()
            .filter(|l| keep(l))
            .map(String::from)
            .collect()
    }

    /// The lines written since `before` whose target is `target`: give
    /// each test a target no other test writes, as tests run at once.
    pub(crate) fn lines_for(before: &str, target: &str) -> Vec<String> {
        let after = log();
        // Strict: the log is append-only, so `before` must be a prefix.
        let delta = after
            .strip_prefix(before)
            .expect("the audit log is append-only; a non-prefix means it was rotated mid-test");
        let needle = format!("target={target}");
        delta
            .lines()
            .filter(|l| l.split_whitespace().any(|w| w == needle))
            .map(String::from)
            .collect()
    }
}
