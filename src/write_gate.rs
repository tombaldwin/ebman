// The one place that decides whether a write is allowed.
//
// There used to be two. `cli::write_refusal` covered the CLI, MCP and
// `lint --fix`; `App::read_only_reason` covered the ~25 TUI dispatch
// sites, with a different input set and its own precedence. The
// divergence was deliberate and documented — the TUI composes session
// gates (global read-only, an in-process freeze, demo mode) the CLI has
// no concept of — but two implementations means two places to keep
// honest, and the guard that existed caught half-composition rather
// than a path calling neither.
//
// So the DECISION converges here and the WORDING does not. Each surface
// renders a `Refusal` in its own voice: the TUI keeps its toast with the
// freeze age and the `:incident END` hint, the CLI keeps `refusing ENV —
// pinned by …`. That split is the point. Collapsing the messages too
// would have been a visible regression for no benefit.

use std::collections::HashMap;

/// Everything the decision reads, as VALUES.
///
/// No `App`, no `Config` method calls, no `std::env` reads, no clock —
/// the old CLI gate reached for `AWS_PROFILE` itself, which made it
/// untestable without touching the process environment. Callers
/// materialise the context; this module only decides.
pub(crate) struct WriteContext<'a> {
    pub env: &'a str,
    /// Resolved by the caller. The CLI falls back to `AWS_PROFILE`; the
    /// TUI uses its session context. Neither fallback belongs here.
    pub profile: Option<&'a str>,
    /// Session-wide read-only (`--read-only`, `:readonly on`). The CLI
    /// has no equivalent and passes `false`.
    pub global_read_only: bool,
    /// A freeze is in force. The TUI holds an in-process `DeployFreeze`
    /// and the CLI reads the cross-process marker; both normalise to a
    /// bool here because the *decision* is the same either way and only
    /// the message differs.
    pub frozen: bool,
    /// Lines under `safety.` the parser could not fully understand.
    /// Non-empty means the policy is only partially readable.
    pub safety_parse_errors: &'a [String],
    pub safety_envs: &'a HashMap<String, bool>,
    pub safety_accounts: &'a HashMap<String, bool>,
}

/// Which rule refused. Carries what a renderer needs to NAME the rule,
/// not the prose itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The safety config has a line this version cannot act on, so the
    /// policy is only partially known.
    SafetyConfigUnreadable {
        problem: String,
    },
    GlobalReadOnly,
    Frozen,
    EnvPinned {
        env: String,
    },
    AccountPinned {
        profile: String,
    },
}

impl Refusal {
    /// Stable machine token naming the rule that refused.
    ///
    /// Deliberately not the prose. The rendered messages differ per
    /// surface by design (see the module docs), so anything that has to
    /// *aggregate* refusals — the audit log, and whatever reads it —
    /// needs a name that does not move when a toast is reworded.
    pub(crate) fn rule(&self) -> &'static str {
        match self {
            Refusal::SafetyConfigUnreadable { .. } => "safety_config_unreadable",
            Refusal::GlobalReadOnly => "global_read_only",
            Refusal::Frozen => "frozen",
            Refusal::EnvPinned { .. } => "env_pinned",
            Refusal::AccountPinned { .. } => "account_pinned",
        }
    }

    /// What the operator would have to change to make this write legal.
    ///
    /// Names the control, and only the control. A refusal that says
    /// nothing leaves an agent to guess, and the guesses are worse than
    /// the truth: retry the same call, try a neighbouring env, or
    /// attempt to edit the config itself. Telling it which lever is
    /// down turns a dead end into something it can hand back to a
    /// human.
    pub(crate) fn remedy(&self) -> String {
        match self {
            Refusal::SafetyConfigUnreadable { problem } => {
                format!("fix config.toml — {problem}")
            }
            Refusal::GlobalReadOnly => {
                "clear read-only mode (:readonly off, or restart without --read-only)".into()
            }
            Refusal::Frozen => "end the deploy freeze (:thaw-deploys, or :incident END)".into(),
            Refusal::EnvPinned { env } => {
                format!("clear safety.envs.{env}.read_only in config.toml")
            }
            Refusal::AccountPinned { profile } => {
                format!("clear safety.accounts.{profile}.read_only in config.toml")
            }
        }
    }
}

/// Decide. `None` means the write may proceed.
///
/// Precedence is session-wide first, then the freeze, then the most
/// specific pin. It is the union of what the two gates did, and it
/// preserves both: the CLI never set `global_read_only`, so dropping
/// that rung leaves its old order (freeze, env pin, account pin)
/// untouched, and the TUI's order was already exactly this.
///
/// Ordering matters beyond message wording. The freeze outranks the
/// pins because it is the incident lever — an operator who froze the
/// fleet should be told that, not told about a pin they set last month.
pub(crate) fn decide(ctx: &WriteContext<'_>) -> Option<Refusal> {
    // FIRST, above every other rung. The others answer "does a rule
    // forbid this write"; this one answers "do we know what the rules
    // are". A parser that skipped what it could not read let
    // `safety.envs.prod = true` — a pin missing its field — leave prod
    // writeable, with nothing anywhere reporting it. An operator who
    // writes a line under `safety.` has stated an intent to restrict,
    // and the one reading that cannot be honoured is the one where a
    // mistake costs most.
    //
    // Refusing everything rather than guessing which env was meant: the
    // guess can be wrong, and `:settings` writes the config back, so a
    // guessed pin would be promoted to a durable one.
    if let Some(problem) = ctx.safety_parse_errors.first() {
        return Some(Refusal::SafetyConfigUnreadable {
            problem: problem.clone(),
        });
    }
    if ctx.global_read_only {
        return Some(Refusal::GlobalReadOnly);
    }
    if ctx.frozen {
        return Some(Refusal::Frozen);
    }
    if ctx.safety_envs.get(ctx.env).copied().unwrap_or(false) {
        return Some(Refusal::EnvPinned {
            env: ctx.env.to_string(),
        });
    }
    if let Some(p) = ctx.profile {
        if ctx.safety_accounts.get(p).copied().unwrap_or(false) {
            return Some(Refusal::AccountPinned {
                profile: p.to_string(),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, bool)]) -> HashMap<String, bool> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    struct Fixture {
        envs: HashMap<String, bool>,
        accounts: HashMap<String, bool>,
    }
    impl Fixture {
        fn new() -> Self {
            Self {
                envs: map(&[("pinned-env", true), ("open-env", false)]),
                // The empty-string key is deliberate: without it, a
                // bug that consults the account map with no profile
                // resolved is indistinguishable from correct code —
                // the lookup simply misses. Verified by mutation.
                //
                // It no longer models a real config line. It used to:
                // `safety.accounts..read_only = true` produced an
                // empty-string key, until the parser started refusing
                // lines it cannot act on. Kept because the defence is
                // still worth having and the map is `pub` — but it is a
                // synthetic case now, not a reachable one.
                accounts: map(&[("pinned-acct", true), ("open-acct", false), ("", true)]),
            }
        }
        fn ctx<'a>(&'a self, env: &'a str, profile: Option<&'a str>) -> WriteContext<'a> {
            WriteContext {
                env,
                profile,
                global_read_only: false,
                frozen: false,
                safety_parse_errors: &[],
                safety_envs: &self.envs,
                safety_accounts: &self.accounts,
            }
        }
    }

    #[test]
    fn an_unpinned_env_on_an_unpinned_account_is_allowed() {
        let f = Fixture::new();
        assert_eq!(decide(&f.ctx("open-env", Some("open-acct"))), None);
        // An env absent from the map entirely is not pinned either —
        // `unwrap_or(false)` is the safe direction only if it is also
        // the tested one.
        assert_eq!(decide(&f.ctx("never-heard-of-it", None)), None);
    }

    #[test]
    fn each_rule_refuses_on_its_own() {
        let f = Fixture::new();
        assert_eq!(
            decide(&f.ctx("pinned-env", None)),
            Some(Refusal::EnvPinned {
                env: "pinned-env".into()
            })
        );
        assert_eq!(
            decide(&f.ctx("open-env", Some("pinned-acct"))),
            Some(Refusal::AccountPinned {
                profile: "pinned-acct".into()
            })
        );
        let mut c = f.ctx("open-env", None);
        c.global_read_only = true;
        assert_eq!(decide(&c), Some(Refusal::GlobalReadOnly));
        let mut c = f.ctx("open-env", None);
        c.frozen = true;
        assert_eq!(decide(&c), Some(Refusal::Frozen));
    }

    #[test]
    fn precedence_is_global_then_freeze_then_env_then_account() {
        // Every rung fires at once. Each assertion removes the winner
        // and checks the next one takes over, which is the only way to
        // pin an ORDER rather than a set — a test that turns on one
        // rule at a time cannot tell any ordering from any other.
        let f = Fixture::new();
        let mut c = f.ctx("pinned-env", Some("pinned-acct"));
        c.global_read_only = true;
        c.frozen = true;
        assert_eq!(decide(&c), Some(Refusal::GlobalReadOnly));

        c.global_read_only = false;
        assert_eq!(decide(&c), Some(Refusal::Frozen));

        c.frozen = false;
        assert_eq!(
            decide(&c),
            Some(Refusal::EnvPinned {
                env: "pinned-env".into()
            })
        );

        let mut c = f.ctx("open-env", Some("pinned-acct"));
        c.frozen = false;
        assert_eq!(
            decide(&c),
            Some(Refusal::AccountPinned {
                profile: "pinned-acct".into()
            })
        );
    }

    #[test]
    fn the_freeze_outranks_a_pin_because_it_is_the_incident_lever() {
        // Not arbitrary: an operator who just froze the fleet must be
        // told about the freeze, not about a pin they set last month.
        // Getting this backwards is a correct refusal with a misleading
        // reason, which sends them to the wrong remedy.
        let f = Fixture::new();
        let mut c = f.ctx("pinned-env", None);
        c.frozen = true;
        assert_eq!(decide(&c), Some(Refusal::Frozen));
    }

    #[test]
    fn an_account_pin_does_not_apply_without_a_profile() {
        // The CLI resolves the profile itself (falling back to
        // AWS_PROFILE); with none resolved there is nothing to match,
        // and matching anyway would refuse every env on a machine with
        // one pinned account and no profile set.
        let f = Fixture::new();
        assert_eq!(decide(&f.ctx("open-env", None)), None);
        // And it must not fall back to an empty profile: the fixture
        // pins `""`, so a lookup that substitutes one would refuse here.
        assert_eq!(decide(&f.ctx("never-heard-of-it", None)), None);
    }

    #[test]
    fn an_env_pin_beats_an_account_pin_so_the_message_names_the_narrower_rule() {
        let f = Fixture::new();
        assert_eq!(
            decide(&f.ctx("pinned-env", Some("pinned-acct"))),
            Some(Refusal::EnvPinned {
                env: "pinned-env".into()
            })
        );
    }

    /// Every rule token must be distinct, and every remedy must name a
    /// control the operator can actually find.
    ///
    /// Distinctness is the load-bearing half: two variants sharing a
    /// token makes the audit log unable to tell a fleet-wide freeze from
    /// a single pinned env, which is the difference between an incident
    /// and a misconfiguration.
    #[test]
    fn every_refusal_names_a_distinct_rule_and_a_findable_remedy() {
        let all = [
            Refusal::GlobalReadOnly,
            Refusal::Frozen,
            Refusal::EnvPinned {
                env: "api-prod".into(),
            },
            Refusal::AccountPinned {
                profile: "prod-admin".into(),
            },
        ];

        let rules: std::collections::HashSet<&str> = all.iter().map(|r| r.rule()).collect();
        assert_eq!(
            rules.len(),
            all.len(),
            "rule tokens collide: {:?}",
            all.iter().map(|r| r.rule()).collect::<Vec<_>>()
        );

        for r in &all {
            let remedy = r.remedy();
            assert!(
                !remedy.is_empty(),
                "{:?} refuses without saying what would change it",
                r
            );
            // The pinned variants must name the SPECIFIC key, not the
            // family: "clear a safety pin" sends an operator hunting
            // through a config file for which one.
            match r {
                Refusal::EnvPinned { env } => assert!(
                    remedy.contains(&format!("safety.envs.{env}.read_only")),
                    "remedy must name the exact key: {remedy}"
                ),
                Refusal::AccountPinned { profile } => assert!(
                    remedy.contains(&format!("safety.accounts.{profile}.read_only")),
                    "remedy must name the exact key: {remedy}"
                ),
                _ => {}
            }
        }
    }

    /// An unreadable safety policy refuses every write, and outranks
    /// every other rung.
    ///
    /// The rung exists because the other four answer "does a rule forbid
    /// this write", and this one answers "do we know what the rules
    /// are". Ordering it below any of them would let a write through on
    /// an env whose pin is exactly the line that failed to parse.
    #[test]
    fn an_unreadable_safety_config_refuses_every_write() {
        let f = Fixture::new();
        let errors = vec!["safety.envs.prod is missing a field".to_string()];

        // Even an env with no pin at all, under no freeze, is refused.
        let mut ctx = f.ctx("open-env", Some("open-acct"));
        ctx.safety_parse_errors = &errors;
        let refusal = decide(&ctx).expect("an unreadable policy must refuse");
        assert_eq!(refusal.rule(), "safety_config_unreadable");
        assert!(
            refusal.remedy().contains("safety.envs.prod"),
            "the remedy must name the offending line: {}",
            refusal.remedy()
        );

        // And it outranks the freeze, which is otherwise the top rung.
        let mut ctx = f.ctx("open-env", None);
        ctx.safety_parse_errors = &errors;
        ctx.frozen = true;
        ctx.global_read_only = true;
        assert_eq!(
            decide(&ctx).map(|r| r.rule()),
            Some("safety_config_unreadable"),
            "a policy we cannot read outranks one we can"
        );

        // No errors → the other rungs behave exactly as before.
        assert!(
            decide(&f.ctx("open-env", Some("open-acct"))).is_none(),
            "a clean config must still allow writes"
        );
    }
}
