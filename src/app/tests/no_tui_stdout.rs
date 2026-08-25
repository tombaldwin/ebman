//! ARCHITECTURE rule 5: nothing reachable from the running TUI may print to
//! stdout or stderr.
//!
//! The alternate screen swallows `println!` / `eprintln!` and they corrupt the
//! display — the text lands in the middle of whatever ratatui drew and stays
//! there until the next full redraw. `tracing::*` is the way out; output goes
//! to `~/.cache/ebman/ebman.log`.
//!
//! The rule is about the *TUI*, not the crate. `src/cli/` and `src/main.rs`
//! print by design — that is the whole point of the headless subcommands, and
//! a guard that forbade printing there would be wrong. So this checks the
//! three trees the alternate screen can reach: `app`, `ui`, `aws`.
//!
//! Rule 5 was the last of the five with nothing behind it. It is cheap to
//! check and, unlike rule 4, a line scan is the right tool: a print macro is a
//! print macro wherever it sits in the file, so there is no ordering or scope
//! question to get wrong.

/// The trees the alternate screen can reach.
const TUI_AREAS: &[&str] = &["src/app", "src/ui", "src/aws"];

/// The macros that write to the terminal directly.
const PRINT_MACROS: &[&str] = &["println!", "eprintln!", "print!(", "eprint!("];

fn print_sites_under(areas: &[&str]) -> Vec<String> {
    let mut hits = Vec::new();
    for m in PRINT_MACROS {
        for site in super::scan::find_in_production(m) {
            if areas.iter().any(|a| site.starts_with(a)) {
                hits.push(format!("{site} — {m}"));
            }
        }
    }
    hits.sort();
    hits
}

#[test]
fn the_tui_never_prints_to_the_terminal() {
    let sites = print_sites_under(TUI_AREAS);
    assert!(
        sites.is_empty(),
        "ARCHITECTURE rule 5: the alternate screen swallows these and they \
         corrupt the display. Use `tracing::*` instead — output goes to \
         ~/.cache/ebman/ebman.log.\n{}",
        sites.join("\n")
    );
}

/// The guard above passes on an empty set, so on its own it cannot tell "the
/// TUI is clean" from "the detector is broken". Point the same detector at the
/// CLI, which prints by design, and require it to find plenty.
///
/// This is the specific failure this codebase keeps meeting: a guard that is
/// correct, commented, and incapable of failing.
#[test]
fn the_print_detector_actually_detects_prints() {
    let cli = print_sites_under(&["src/cli"]);
    assert!(
        cli.len() > 20,
        "the CLI prints by design, so a detector that finds {} sites there is \
         not working — which would make the rule-5 guard above vacuous",
        cli.len()
    );
}

/// And the area filter has to be a filter. If `TUI_AREAS` silently matched
/// nothing, the guard would also pass forever.
#[test]
fn the_tui_areas_exist_and_hold_source() {
    let files = super::scan::source_files();
    for area in TUI_AREAS {
        let n = files.iter().filter(|(p, _)| p.starts_with(area)).count();
        assert!(
            n > 0,
            "{area} matched no source files — the rule-5 guard is looking at \
             nothing. Did a module move?"
        );
    }
}
