//! The gate every CLI write goes through: the [`WriteEnv`] a body is
//! handed (its ambient profile, freeze store and [`WriteGate`]), the
//! refusal it renders and audits ([`write_refusal`]), and the check that
//! stops a whole run under a freeze ([`freeze_refuses_run`]). Moved out
//! of `cli/mod.rs`, which it had grown from 786 to over 1,000 lines.

/// The refusal for a whole run that `freeze` stops before it reads the
/// fleet, audited — or `None` when there is no freeze, or when the gate
/// ranks another rung above it.
///
/// Decided by [`crate::write_gate::decide`], not assumed: the check this
/// replaced hard-coded "frozen", so with an unreadable safety config
/// AND a freeze, `lint --fix` reported the freeze where every other
/// write path reports the config. A rung that outranks the freeze is
/// left to the per-env gate, which words and audits it for each env.
///
/// Up front at all so a frozen run refuses (exit 3) without AWS: a
/// freeze is declared in an incident, which is when AWS may not answer.
pub(crate) fn freeze_refuses_run(
    safety_cfg: &crate::config::Config,
    profile: &Option<String>,
    freeze: Option<crate::freeze::FreezeMarker>,
    action_label: &str,
) -> Option<String> {
    let m = freeze?;
    // Resolved by the caller, from its `WriteEnv`.
    let pin_profile = profile.clone();
    let verdict = crate::write_gate::decide(&crate::write_gate::WriteContext {
        // No env: the rungs above the freeze are all run-wide, and it
        // outranks every per-env pin, so the verdict is one of those or
        // the freeze.
        env: "",
        profile: pin_profile.as_deref(),
        safety_parse_errors: &safety_cfg.safety_parse_errors,
        global_read_only: safety_cfg.safety_read_only,
        frozen: true,
        safety_envs: &safety_cfg.safety_envs,
        safety_accounts: &safety_cfg.safety_accounts,
        // A run-wide freeze check, before any verb: no role rung.
        role: &crate::config::RoleCheck::NotAsked,
    });
    if verdict != Some(crate::write_gate::Refusal::Frozen) {
        return None;
    }
    audit_frozen(pin_profile.as_deref(), "-", action_label, "-", &[]);
    Some(crate::freeze::refusal_message(&m))
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
        // The CLI acts for the person at the keyboard: roles are MCP's.
        write_refusal_unaudited(
            safety_cfg,
            env,
            profile,
            active_freeze,
            &crate::config::RoleCheck::NotAsked,
        )?;
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
    role: &crate::config::RoleCheck,
) -> Option<(crate::write_gate::Refusal, String, Option<String>)> {
    // The profile is resolved by the caller: the CLI from its
    // `WriteEnv`, MCP from the profile its `Server` read at startup.
    // This read `AWS_PROFILE` itself as a fallback, an ambient read that
    // a test's `WriteEnv` could not stand in, so a body test's account
    // pins depended on the developer's shell.
    let pin_profile = profile.clone();

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
        role,
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
        crate::write_gate::Refusal::RoleConfigUnreadable { problem } => {
            format!("refusing {env} — MCP role config unreadable: {problem}")
        }
        crate::write_gate::Refusal::NoRole => {
            format!("refusing {env} — mcp.require_role is set and no mcp.role lists this verb")
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

/// What decides whether a CLI write may go, given the freeze as it
/// stands: the refusal, if any. A testable body takes one in its
/// [`WriteEnv`], so a test sees a refusal without the real config;
/// production's is [`real_refusal`]. (Not `write_gate::Refusal`, the
/// reason a gate refused; this answers with the message.)
pub(crate) trait WriteGate:
    Fn(&GateQuery<'_>, Option<crate::freeze::FreezeMarker>) -> Option<String>
{
}
impl<T: Fn(&GateQuery<'_>, Option<crate::freeze::FreezeMarker>) -> Option<String>> WriteGate for T {}

/// [`write_refusal`] against the operator's config and `freeze`: the
/// audited funnel every CLI write goes through.
pub(crate) fn real_refusal(
    q: &GateQuery<'_>,
    freeze: Option<crate::freeze::FreezeMarker>,
) -> Option<String> {
    write_refusal(
        &crate::config::load(),
        q.env,
        &q.profile.map(str::to_string),
        freeze,
        q.region,
        q.verb,
    )
}

/// What a CLI write takes from the operator's machine: the account its
/// pins are checked against, the fleet freeze, and the gate that decides
/// each write. Every write shell passes [`WriteEnv::real`], so a body
/// test stands in all three at once and one guard covers every shell.
/// See `docs/design/write-env.md`.
pub(crate) struct WriteEnv<G> {
    /// `AWS_PROFILE`: the account each account pin is checked against
    /// when the command names no `--profile`.
    pub ambient_profile: Option<String>,
    /// The fleet freeze: read for each write, between rollout regions
    /// and before each `lint --fix` env, and declared and lifted by
    /// `ebman freeze` and `ebman thaw`.
    pub freeze: crate::freeze::FreezeStore,
    /// Decides each write, given the freeze as [`freeze`](Self::freeze)
    /// reads it.
    pub gate: G,
}

/// The production gate's type, so [`WriteEnv::real`] can name it.
pub(crate) type RealGate =
    fn(&GateQuery<'_>, Option<crate::freeze::FreezeMarker>) -> Option<String>;

impl WriteEnv<RealGate> {
    /// The operator's: `AWS_PROFILE`, the store every process shares,
    /// and [`real_refusal`].
    pub(crate) fn real() -> Self {
        WriteEnv {
            ambient_profile: std::env::var("AWS_PROFILE").ok(),
            freeze: crate::freeze::FreezeStore::real(),
            gate: real_refusal,
        }
    }
}

impl<G: WriteGate> WriteEnv<G> {
    /// The refusal for this write, if any, with the freeze read now: a
    /// freeze declared mid-run refuses what comes after it.
    pub(crate) fn refusal(&self, q: &GateQuery<'_>) -> Option<String> {
        (self.gate)(q, self.freeze.active())
    }
}

impl<G> WriteEnv<G> {
    /// The freeze in force, as its refusal names it: re-read for a rollout
    /// already in flight.
    ///
    /// A write that has not started is refused and exits 3. A rollout
    /// that is part-way through has regions already dispatched and a
    /// report to emit, so it needs the reason rather than an exit — it
    /// halts through the same path a failed region takes, and the
    /// untouched regions come out as `skipped`.
    ///
    /// Re-read per region on purpose. The gate at the top of `rollout_body`
    /// fires once, before pre-flight; with no cap on `--wait-for-green` and
    /// none on region count, a sequential rollout can dispatch its last
    /// region hours later. A freeze is not static config like a pin — it is
    /// a live incident signal, and it arrives mid-flight by definition.
    pub(crate) fn freeze_halt(&self) -> Option<String> {
        self.freeze
            .active()
            .map(|m| crate::freeze::refusal_message(&m))
    }
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
        for (path, text) in crate::app::tests::scan::source_files() {
            // `write_env.rs` declares it. Exactly that file: the
            // exemption was any `mod.rs`, `cli/mcp/mod.rs` included.
            if !path.starts_with("src/cli/") || path == "src/cli/write_env.rs" {
                continue;
            }
            let prod = crate::app::tests::scan::production_half(&text);
            let n = prod.matches("write_refusal_unaudited(").count();
            if n > 0 {
                found.push((path, n));
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
            if path == "src/cli/write_env.rs" {
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
    /// Every write shell hands its body the operator's [`WriteEnv`].
    ///
    /// The bodies take it as a parameter so a test can stand it in, which
    /// means nothing the tests drive can see what production passes: a
    /// shell handing a gate of `|_, _| None` gates nothing, one handing
    /// `None` for `AWS_PROFILE` checks every account pin against no
    /// account (the defect the deleted rollout `--profile` scan existed
    /// for), and one handing another store misses every freeze. Pinned
    /// by source because the shells exit the process. One pattern for
    /// every shell, which the separate parameters this replaced could
    /// not give. For `lint`, `freeze` and `thaw` it pins the profile and
    /// the store only: none of them calls the gate (lint decides with
    /// its setup's config), which is a BACKLOG item.
    #[test]
    fn write_shells_pass_the_real_write_env() {
        for (module, f) in [
            ("cli/action.rs", "run"),
            ("cli/action.rs", "run_rollout"),
            ("cli/audit_replay.rs", "run_replay"),
            ("cli/lint.rs", "run"),
            ("cli/freeze.rs", "run_freeze"),
            ("cli/freeze.rs", "run_thaw"),
        ] {
            let src = crate::app::tests::scan::production_source(module);
            let body = crate::app::tests::scan::fn_bodies(&src)
                .into_iter()
                .find(|(name, _)| name == f)
                .unwrap_or_else(|| panic!("{module}: `{f}` is defined"))
                .1;
            // The body call's own arguments, not anywhere in the shell: a
            // shell building the real env for something else while passing
            // a test one must not pass.
            let call = &body[body
                .find("_body(")
                .unwrap_or_else(|| panic!("{f} calls a body"))..];
            let args = &call[..call.find("\n    )").unwrap_or(call.len())];
            assert!(
                // Built, so it is not a read of the shared store to
                // `no_other_test_reads_the_shared_store`.
                args.contains(concat!("crate::cli::WriteEnv::", "real(),")),
                "{module}: `{f}` must pass the real write env to its body: {args}"
            );
        }
    }

    /// Production reads `AWS_PROFILE` in these places only. A write's
    /// account pins are checked against the profile its `WriteEnv` (CLI)
    /// or `Server` (MCP) carries; a gate that read the process for itself
    /// was invisible to every test's stand-in, so pin results depended on
    /// the developer's shell. Two such fallbacks survived the `WriteEnv`
    /// change and were found by its release review.
    #[test]
    fn ambient_profile_reads_stay_where_they_are_injected() {
        // Each entry names why it may read the process.
        const ALLOWED: &[(&str, &str)] = &[
            ("src/cli/write_env.rs", "WriteEnv::real, the CLI's one read"),
            ("src/cli/mcp/mod.rs", "own_ambient_profile, MCP's one read"),
            ("src/aws.rs", "which profile the SDK client is built from"),
            (
                "src/cli/mcp/tools.rs",
                "credential_fix, naming the profile in a hint",
            ),
        ];
        let needle = concat!("std::env::var(\"AWS_", "PROFILE\")");
        let mut found: Vec<(String, usize)> = Vec::new();
        for (path, src) in crate::app::tests::scan::source_files() {
            if crate::app::tests::scan::is_test_path(&path) {
                continue;
            }
            let production = crate::app::tests::scan::production_half(&src);
            let n = production
                .lines()
                .map(|l| {
                    crate::app::tests::scan::strip_line_comment(l)
                        .matches(needle)
                        .count()
                })
                .sum();
            if n > 0 {
                found.push((path, n));
            }
        }
        let unexpected: Vec<_> = found
            .iter()
            .filter(|(p, n)| *n != 1 || !ALLOWED.iter().any(|(a, _)| a == p))
            .collect();
        assert!(
            unexpected.is_empty(),
            "a production read of AWS_PROFILE outside the injected places; take it \
             from the WriteEnv or the Server: {unexpected:?}"
        );
        assert!(
            found.iter().any(|(p, _)| p == "src/cli/write_env.rs"),
            "canary: the scan finds WriteEnv::real's read: {found:?}"
        );
    }

    /// And the real env is the operator's: their `AWS_PROFILE`, the store
    /// every process shares, and the audited gate.
    #[test]
    fn the_real_write_env_reads_the_operators_machine() {
        let src = crate::app::tests::scan::production_source("cli/write_env.rs");
        let body = crate::app::tests::scan::fn_bodies(&src)
            .into_iter()
            .find(|(name, _)| name == "real")
            .expect("`WriteEnv::real` is defined")
            .1;
        for part in [
            "ambient_profile: std::env::var(\"AWS_PROFILE\").ok(),",
            "freeze: crate::freeze::FreezeStore::real(),",
            "gate: real_refusal,",
        ] {
            assert!(
                body.contains(part),
                "`WriteEnv::real` must set `{part}`: {body}"
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

        let refusal = super::write_refusal_unaudited(
            &cfg,
            "any-env",
            &None,
            None,
            &crate::config::RoleCheck::NotAsked,
        );
        let (rule, message, _) = refusal.expect("a standing read-only must refuse");
        assert_eq!(rule.rule().as_str(), "global_read_only", "{message}");

        // The control: without it, this env is writeable. Without this
        // the test would pass against a gate that refuses everything.
        let open = crate::config::Config::default();
        assert!(
            super::write_refusal_unaudited(
                &open,
                "any-env",
                &None,
                None,
                &crate::config::RoleCheck::NotAsked
            )
            .is_none(),
            "no standing restriction means no refusal"
        );
    }
}

#[cfg(test)]
impl<G> WriteEnv<G> {
    /// `gate` and `ambient_profile`, with a freeze store of its own.
    pub(crate) fn for_tests(ambient_profile: Option<&str>, gate: G) -> Self {
        WriteEnv {
            ambient_profile: ambient_profile.map(String::from),
            freeze: crate::freeze::FreezeStore::isolated(),
            gate,
        }
    }
}

#[cfg(test)]
mod write_env_tests {
    use super::*;

    /// Production's gate refuses under a standing freeze, with the
    /// freeze's own sentence, and lets a write through with none (the
    /// test config pins nothing). The real shells pass it, so this is
    /// the one place it is called directly.
    #[test]
    fn the_real_gate_refuses_under_a_freeze() {
        let q = GateQuery {
            env: "poly-real-gate-probe",
            profile: None,
            region: Some("poly-gate-1"),
            verb: "Restart",
        };
        let marker =
            crate::freeze::parse_marker(r#"{"pid":1,"reason":"bad deploy","persistent":true}"#)
                .unwrap();
        let refused = real_refusal(&q, Some(marker)).expect("a freeze refuses");
        assert!(
            refused.contains("fleet freeze active (bad deploy)"),
            "{refused}"
        );
    }

    /// A freeze declared *during* a rollout stops the regions that have
    /// not dispatched yet: both dispatch loops read `freeze_halt`
    /// between regions, and it reads the env's store each time.
    #[test]
    fn freeze_halt_reads_the_store_each_time() {
        let env = WriteEnv::for_tests(None, ());
        assert!(
            env.freeze_halt().is_none(),
            "no freeze declared — a rollout must be free to continue, or \
             this check would halt every rollout it is asked about"
        );
        env.freeze
            .declare_persistent("incident #4321", true)
            .expect("declared");
        let reason = env
            .freeze_halt()
            .expect("a freeze declared mid-rollout halts it");
        assert!(reason.contains("incident #4321"), "{reason}");
        let _ = env.freeze.thaw();
        assert!(
            env.freeze_halt().is_none(),
            "lifting the freeze releases the halt"
        );
    }

    /// The gate is handed the freeze as the env's store reads it at the
    /// time of the write, not a copy taken when the env was built.
    #[test]
    fn the_gate_sees_the_freeze_at_the_time_of_the_write() {
        let env = WriteEnv::for_tests(
            None,
            |q: &GateQuery<'_>, f: Option<crate::freeze::FreezeMarker>| {
                f.map(|m| format!("refusing {} — {}", q.env, m.reason))
            },
        );
        let q = GateQuery {
            env: "api",
            profile: None,
            region: None,
            verb: "Restart",
        };
        assert_eq!(env.refusal(&q), None);
        env.freeze
            .declare_persistent("incident #9", true)
            .expect("declared");
        assert_eq!(
            env.refusal(&q).as_deref(),
            Some("refusing api — incident #9")
        );
    }
}
