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
    for arg in args.iter().skip(1) {
        match arg.as_str() {
            "--incident" => incident = true,
            "--status" => status = true,
            other if other.starts_with("--") => {
                return Err(format!("ebman freeze: unknown flag '{other}'"));
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

/// `ebman freeze`'s whole behaviour, writers injected. Returns the exit
/// code.
pub(crate) fn freeze_body(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    match parse_freeze_args(args) {
        Err(msg) => {
            let _ = writeln!(err, "{msg}");
            2
        }
        Ok(FreezeCmd::Status) => {
            let _ = writeln!(out, "{}", status_text());
            0
        }
        Ok(FreezeCmd::Declare { reason, incident }) => {
            if let Err(e) = crate::freeze::write_persistent_marker(&reason, incident) {
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
                std::env::var("AWS_PROFILE").ok().as_deref(),
                "-",
                if incident {
                    "IncidentStart"
                } else {
                    "FreezeDeploys"
                },
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
fn status_text() -> String {
    let mut lines = vec![match crate::freeze::read_active() {
        Some(m) => {
            let reason = if m.reason.is_empty() {
                "no reason given".to_string()
            } else {
                m.reason.clone()
            };
            format!(
                "frozen ({reason}) — {}, since {}; lift with `{}`",
                m.holder(),
                m.at,
                m.remedy()
            )
        }
        None => "not frozen".to_string(),
    }];
    if let Some(ended) = crate::freeze::read_ended() {
        lines.push(format!("note: {}", ended.message()));
    }
    lines.join("\n")
}

/// `ebman thaw`'s whole behaviour, writers injected. Returns the exit
/// code.
pub(crate) fn thaw_body(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    if let Some(extra) = args.get(1) {
        let _ = writeln!(err, "ebman thaw: takes no arguments (got '{extra}')");
        return 2;
    }
    match crate::freeze::thaw_persistent() {
        crate::freeze::Thaw::Lifted(m) => {
            let label = if m.incident {
                "IncidentEnd"
            } else {
                "ThawDeploys"
            };
            crate::audit::append_action_dispatched(
                None,
                std::env::var("AWS_PROFILE").ok().as_deref(),
                "-",
                label,
                "",
                &[
                    ("was_frozen", "true"),
                    ("persistent", "true"),
                    ("via", "cli"),
                ],
            );
            let _ = writeln!(out, "fleet freeze lifted");
            0
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
    let code = freeze_body(args, &mut std::io::stdout(), &mut std::io::stderr());
    std::process::exit(code);
}

/// `ebman thaw`: the process entry, exiting with [`thaw_body`]'s code.
pub async fn run_thaw(args: &[String]) -> Result<()> {
    let code = thaw_body(args, &mut std::io::stdout(), &mut std::io::stderr());
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn run(f: fn(&[String], &mut dyn Write, &mut dyn Write) -> i32, a: &[&str]) -> (i32, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = f(&argv(a), &mut out, &mut err);
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
        assert!(parse_freeze_args(&argv(&["freeze", "--frozen"])).is_err());
    }

    /// Through the real marker path: declare, status, thaw, and the thaw
    /// refusing a live session's freeze.
    #[test]
    fn freeze_then_thaw_round_trip() {
        let _guard = crate::freeze::MARKER_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::freeze::clear_marker_if_own();
        let _ = crate::freeze::thaw_persistent();

        let (code, text) = run(freeze_body, &["freeze", "release", "window"]);
        assert_eq!(code, 0, "{text}");
        let m = crate::freeze::read_active().expect("frozen");
        assert!(m.persistent && m.reason == "release window");

        let (code, text) = run(freeze_body, &["freeze", "--status"]);
        assert_eq!(code, 0);
        assert!(
            text.contains("release window") && text.contains("ebman thaw"),
            "{text}"
        );

        let (code, text) = run(thaw_body, &["thaw"]);
        assert_eq!(code, 0, "{text}");
        assert!(crate::freeze::read_active().is_none());

        // A session's own freeze (this process is alive) is not ours.
        crate::freeze::write_marker("tui freeze", false).unwrap();
        let (code, text) = run(thaw_body, &["thaw"]);
        assert_eq!(code, 3, "{text}");
        assert!(text.contains(":thaw-deploys"), "{text}");
        assert!(crate::freeze::read_active().is_some());
        crate::freeze::clear_marker_if_own();

        let (code, _) = run(thaw_body, &["thaw", "now"]);
        assert_eq!(code, 2);
    }
}
