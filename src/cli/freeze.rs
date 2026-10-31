//! `ebman freeze [--incident] [REASON…]`, `ebman freeze --status` and
//! `ebman thaw`: a fleet freeze that belongs to no TUI session.
//!
//! `:freeze-deploys` / `:incident START` freeze writes for as long as the
//! TUI that declared them runs, so quitting it, or losing the SSH
//! connection, lifted a freeze mid-incident. One declared here holds
//! until an explicit thaw. See `docs/design/persistent-freeze.md`.
//!
//! Exit codes:
//! - `freeze`: 0 declared (or status printed), 1 the marker could not be
//!   written, so the freeze is NOT in force, 2 usage.
//! - `thaw`: 0 lifted, or nothing to lift; 3 a live TUI session holds the
//!   freeze, and only it may lift it; 2 usage.

use std::io::Write;

use color_eyre::eyre::Result;

#[derive(Debug, PartialEq, Eq)]
enum FreezeCmd {
    Declare { reason: String, incident: bool },
    Status,
}

/// Pure parse of `ebman freeze …`. `args[0]` is the subcommand.
fn parse_freeze_args(args: &[String]) -> Result<FreezeCmd, String> {
    let mut incident = false;
    let mut status = false;
    let mut words: Vec<&str> = Vec::new();
    let mut flags_done = false;
    for arg in args.iter().skip(1) {
        match arg.as_str() {
            other if flags_done => words.push(other),
            // A reason that starts with `-` follows a bare `--`.
            "--" => flags_done = true,
            "--incident" => incident = true,
            "--status" => status = true,
            // Any dash, not just `--`: a mistyped `-x` became the reason
            // and froze the fleet with it.
            other if other.starts_with('-') => {
                return Err(format!(
                    "ebman freeze: unknown flag '{other}' (a reason starting with '-' goes after `--`)"
                ));
            }
            other => words.push(other),
        }
    }
    if status {
        if incident || !words.is_empty() {
            return Err("usage: ebman freeze --status (takes nothing else)".into());
        }
        return Ok(FreezeCmd::Status);
    }
    Ok(FreezeCmd::Declare {
        reason: words.join(" ").trim().to_string(),
        incident,
    })
}

/// `ebman freeze`'s whole behaviour against `write_env`'s store and
/// profile, writers injected. Returns the exit code.
pub(crate) fn freeze_body<G>(
    args: &[String],
    write_env: crate::cli::WriteEnv<G>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    match parse_freeze_args(args) {
        Err(msg) => {
            let _ = writeln!(err, "{msg}");
            2
        }
        Ok(FreezeCmd::Status) => {
            let _ = writeln!(out, "{}", status_text(&write_env.freeze));
            0
        }
        Ok(FreezeCmd::Declare { reason, incident }) => {
            if let Err(bad) = crate::freeze::check_reason(&reason) {
                let _ = writeln!(err, "ebman freeze: the reason {}", bad.what());
                return 2;
            }
            if let Err(e) = write_env.freeze.declare_persistent(&reason, incident) {
                let _ = writeln!(
                    err,
                    "ebman freeze: could not write the freeze marker ({e}) — the fleet is NOT frozen"
                );
                return 1;
            }
            let audit_reason = if reason.is_empty() {
                "no-reason"
            } else {
                reason.as_str()
            };
            crate::audit::append_action_dispatched(
                None,
                write_env.ambient_profile.as_deref(),
                "-",
                crate::freeze::declare_label(incident),
                "",
                &[
                    ("reason", audit_reason),
                    ("persistent", "true"),
                    ("via", "cli"),
                ],
            );
            let _ = writeln!(
                out,
                "fleet frozen{} — every ebman write refuses until `ebman thaw`",
                if reason.is_empty() {
                    String::new()
                } else {
                    format!(" ({reason})")
                }
            );
            0
        }
    }
}

/// What is in force, and any freeze that ended with its session.
fn status_text(store: &crate::freeze::FreezeStore) -> String {
    let holders = store.all();
    let mut lines: Vec<String> = if holders.is_empty() {
        vec!["not frozen".to_string()]
    } else {
        holders
            .iter()
            .map(|m| {
                format!(
                    "frozen ({}) — {}, since {}; lift with `{}`",
                    m.reason_or_default(),
                    m.holder(),
                    m.at,
                    m.remedy()
                )
            })
            .collect()
    };
    if holders.is_empty() {
        if let Some(ended) = store.ended() {
            lines.push(format!("note: {}", ended.message()));
        }
        if let Some(thawed) = store.recently_thawed() {
            lines.push(format!("note: {}", thawed.message()));
        }
    }
    lines.join("\n")
}

/// `ebman thaw`'s whole behaviour against `write_env`'s store and
/// profile, writers injected. Returns the exit code.
pub(crate) fn thaw_body<G>(
    args: &[String],
    write_env: crate::cli::WriteEnv<G>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    if let Some(extra) = args.get(1) {
        let _ = writeln!(err, "ebman thaw: takes no arguments (got '{extra}')");
        return 2;
    }
    match write_env.freeze.thaw() {
        crate::freeze::Thaw::Lifted {
            marker: m,
            still_held,
        } => {
            let label = if m.incident {
                "IncidentEnd"
            } else {
                "ThawDeploys"
            };
            crate::audit::append_action_dispatched(
                None,
                write_env.ambient_profile.as_deref(),
                "-",
                label,
                "",
                &[
                    ("was_frozen", "true"),
                    ("persistent", "true"),
                    ("via", "cli"),
                ],
            );
            match still_held {
                // Lifted, but writes are still refused: a script must
                // not read "lifted" as all clear, so not 0 either — 3,
                // as when the session's freeze is all there is.
                Some(s) => {
                    let _ = writeln!(
                        out,
                        "the freeze {} is lifted, but the fleet is still frozen: \
                         it is {}; lift it there with `{}`",
                        m.holder(),
                        s.holder(),
                        s.remedy()
                    );
                    3
                }
                // Who declared it, when it was not the operator: an
                // agent's freeze lifted without a word looks like their
                // own.
                // And that it cannot simply freeze again: the thaw holds.
                None if m.agent.is_some() => {
                    let until = write_env
                        .freeze
                        .recently_thawed()
                        .map(|t| {
                            format!(
                                "; `freeze_fleet` is refused until {}",
                                t.holds_until().format("%H:%M UTC")
                            )
                        })
                        .unwrap_or_default();
                    let _ = writeln!(out, "fleet freeze lifted (it was {}){until}", m.holder());
                    0
                }
                None => {
                    let _ = writeln!(out, "fleet freeze lifted");
                    0
                }
            }
        }
        crate::freeze::Thaw::HeldBySession(m) => {
            let _ = writeln!(
                err,
                "ebman thaw: refusing — the freeze is {}; lift it there with `{}`",
                m.holder(),
                m.remedy()
            );
            3
        }
        crate::freeze::Thaw::Nothing => {
            let _ = writeln!(out, "not frozen — nothing to thaw");
            0
        }
    }
}

/// `ebman freeze`: the process entry, exiting with [`freeze_body`]'s code.
pub async fn run_freeze(args: &[String]) -> Result<()> {
    let code = freeze_body(
        args,
        crate::cli::WriteEnv::real(),
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    );
    // Draining: the declare / thaw writes an audit line whose webhook
    // POST a bare `process::exit` cancels.
    if code != 0 {
        crate::cli::exit_after_drain(code).await;
    }
    crate::cli::drain_before_return().await;
    Ok(())
}

/// `ebman thaw`: the process entry, exiting with [`thaw_body`]'s code.
pub async fn run_thaw(args: &[String]) -> Result<()> {
    let code = thaw_body(
        args,
        crate::cli::WriteEnv::real(),
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    );
    // Draining: the declare / thaw writes an audit line whose webhook
    // POST a bare `process::exit` cancels.
    if code != 0 {
        crate::cli::exit_after_drain(code).await;
    }
    crate::cli::drain_before_return().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    type Body = fn(&[String], crate::cli::WriteEnv<()>, &mut dyn Write, &mut dyn Write) -> i32;

    /// `f` over `store`, as `ebman` with no `AWS_PROFILE` would run it.
    fn run(f: Body, store: &crate::freeze::FreezeStore, a: &[&str]) -> (i32, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let env = crate::cli::WriteEnv {
            ambient_profile: None,
            freeze: store.clone(),
            gate: (),
        };
        let code = f(&argv(a), env, &mut out, &mut err);
        let text = String::from_utf8(out).unwrap() + &String::from_utf8(err).unwrap();
        (code, text)
    }

    #[test]
    fn freeze_args_parse() {
        assert_eq!(
            parse_freeze_args(&argv(&["freeze", "--incident", "db", "failover"])),
            Ok(FreezeCmd::Declare {
                reason: "db failover".into(),
                incident: true
            })
        );
        assert_eq!(
            parse_freeze_args(&argv(&["freeze"])),
            Ok(FreezeCmd::Declare {
                reason: String::new(),
                incident: false
            })
        );
        assert_eq!(
            parse_freeze_args(&argv(&["freeze", "--status"])),
            Ok(FreezeCmd::Status)
        );
        assert!(parse_freeze_args(&argv(&["freeze", "--status", "x"])).is_err());
        assert!(
            parse_freeze_args(&argv(&["freeze", "-x", "db"])).is_err(),
            "a mistyped flag is no reason"
        );
        assert_eq!(
            parse_freeze_args(&argv(&["freeze", "--", "-5%", "traffic"])),
            Ok(FreezeCmd::Declare {
                reason: "-5% traffic".into(),
                incident: false
            })
        );
        assert!(parse_freeze_args(&argv(&["freeze", "--frozen"])).is_err());
    }

    /// A freeze an agent declared is named as one by `--status` and by
    /// the thaw that lifts it, so the operator does not take it for
    /// their own.
    #[test]
    fn status_and_thaw_name_an_agent_s_freeze() {
        let store = crate::freeze::FreezeStore::isolated();
        let agent = "declared by an agent (claude-code) with `freeze_fleet`";
        store
            .declare_persistent_by_agent("bad deploy", false, "claude-code")
            .unwrap();
        let (_, text) = run(freeze_body, &store, &["freeze", "--status"]);
        assert!(text.contains(agent), "{text}");
        let (code, text) = run(thaw_body, &store, &["thaw"]);
        assert_eq!(code, 0, "{text}");
        // And that the agent cannot freeze again until the thaw ends.
        let until = store
            .recently_thawed()
            .expect("the thaw holds")
            .holds_until()
            .format("%H:%M UTC")
            .to_string();
        assert_eq!(
            text.trim(),
            format!(
                "fleet freeze lifted (it was {agent}); `freeze_fleet` is refused until {until}"
            )
        );
        let (_, text) = run(freeze_body, &store, &["freeze", "--status"]);
        assert!(
            text.contains("not frozen")
                && text.contains("note: a fleet freeze (bad deploy) was lifted at ")
                && text.contains(&format!("refused until {until}")),
            "{text}"
        );

        // Lifted while a session's still holds: it names what it lifted.
        store
            .declare_persistent_by_agent("bad deploy", false, "claude-code")
            .unwrap();
        store
            .declare_session(std::process::id(), "tui freeze", false)
            .unwrap();
        let (code, text) = run(thaw_body, &store, &["thaw"]);
        assert_eq!(code, 3, "{text}");
        assert!(
            text.contains(&format!("the freeze {agent} is lifted")),
            "{text}"
        );
        store.clear_session(std::process::id());
    }

    /// `ebman freeze` follows the rule `freeze_fleet` does: a reason that
    /// is not one short line is refused, and nothing is frozen.
    #[test]
    fn freeze_refuses_a_reason_that_is_not_one_short_line() {
        let store = crate::freeze::FreezeStore::isolated();
        let long = "x".repeat(crate::freeze::REASON_MAX + 1);
        for reason in ["db\u{202E}failover", long.as_str()] {
            let (code, text) = run(freeze_body, &store, &["freeze", reason]);
            assert_eq!(code, 2, "{text}");
            assert!(text.starts_with("ebman freeze: the reason "), "{text}");
            assert!(store.active().is_none(), "nothing frozen: {text}");
        }
    }

    /// Declare, status, thaw, and the thaw refusing a live session's
    /// freeze, through a store of the test's own.
    #[test]
    fn freeze_then_thaw_round_trip() {
        let store = crate::freeze::FreezeStore::isolated();

        let (code, text) = run(freeze_body, &store, &["freeze", "release", "window"]);
        assert_eq!(code, 0, "{text}");
        let m = store.active().expect("frozen");
        assert!(m.persistent && m.reason == "release window");

        let (code, text) = run(freeze_body, &store, &["freeze", "--status"]);
        assert_eq!(code, 0);
        assert!(
            text.contains("release window") && text.contains("ebman thaw"),
            "{text}"
        );

        let (code, text) = run(thaw_body, &store, &["thaw"]);
        assert_eq!(code, 0, "{text}");
        assert_eq!(
            text.trim(),
            "fleet freeze lifted",
            "the operator's own: no holder"
        );
        assert!(store.active().is_none());

        // A session's own freeze (this process is alive) is not ours.
        store
            .declare_session(std::process::id(), "tui freeze", false)
            .unwrap();
        let (code, text) = run(thaw_body, &store, &["thaw"]);
        assert_eq!(code, 3, "{text}");
        assert!(text.contains(":thaw-deploys"), "{text}");
        assert!(store.active().is_some());

        // Both: the persistent one lifts, and the output says the session's
        // still holds rather than "lifted", which a script reads as clear.
        let (code, _) = run(freeze_body, &store, &["freeze", "change"]);
        assert_eq!(code, 0);
        let (code, text) = run(thaw_body, &store, &["thaw"]);
        assert_eq!(code, 3, "still frozen is not all clear: {text}");
        assert!(
            text.contains("still frozen") && !text.contains("fleet freeze lifted"),
            "{text}"
        );
        store.clear_session(std::process::id());

        let (code, _) = run(thaw_body, &store, &["thaw", "now"]);
        assert_eq!(code, 2);
    }
}
