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

/// Print macros are not the only way to reach the terminal. `writeln!` into
/// `std::io::stdout()` does the same damage and would sail past the sweep
/// above, so the handles themselves are checked too — with an allowlist,
/// because there is one legitimate use and forbidding it outright would be
/// wrong.
const TERMINAL_HANDLES: &[&str] = &["stdout()", "stderr()"];

/// `(path, how many sites, why they're allowed)`.
///
/// The count is part of the pin. File-level granularity would let a second,
/// unjustified write hide behind the first one's reason — which is the shape
/// `every_spawn_declares_whether_it_is_per_env` exists to prevent, and the
/// same rule applies here: widening this list to quiet the guard is a stop
/// condition, not a fix.
const HANDLE_EXCEPTIONS: &[(&str, usize, &str)] = &[(
    "src/app/spawn_refresh.rs",
    1,
    "BEL (0x07), written to ring the terminal bell when a new env goes red. \
     A control character rather than display text, so it cannot corrupt the \
     alternate screen — which is the whole of what rule 5 protects.",
)];

fn handle_sites() -> std::collections::BTreeMap<String, usize> {
    let mut per_file: std::collections::BTreeMap<String, usize> = Default::default();
    for h in TERMINAL_HANDLES {
        for site in super::scan::find_in_production(h) {
            let path = site
                .rsplit_once(':')
                .map_or(site.clone(), |(p, _)| p.to_string());
            if TUI_AREAS.iter().any(|a| path.starts_with(a)) {
                *per_file.entry(path).or_default() += 1;
            }
        }
    }
    per_file
}

#[test]
fn every_direct_terminal_write_in_the_tui_is_justified() {
    let sites = handle_sites();
    let mut problems = Vec::new();

    for (path, found) in &sites {
        match HANDLE_EXCEPTIONS.iter().find(|(p, _, _)| p == path) {
            None => problems.push(format!(
                "{path}: {found} direct terminal write(s) with no justification. \
                 Rule 5 — use `tracing::*`, or add an entry here saying why this \
                 one cannot corrupt the alternate screen."
            )),
            Some((_, expected, why)) if expected != found => problems.push(format!(
                "{path}: {found} direct terminal write(s), but the allowlist \
                 justifies {expected}. The justification on record is: {why}"
            )),
            Some(_) => {}
        }
    }

    // A justification for a site that no longer exists is a lie the next
    // reader will believe.
    for (path, _, _) in HANDLE_EXCEPTIONS {
        if !sites.contains_key(*path) {
            problems.push(format!(
                "{path} is allowlisted for a direct terminal write but has none \
                 — drop the entry."
            ));
        }
    }

    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
