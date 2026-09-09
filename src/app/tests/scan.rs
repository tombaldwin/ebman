//! Shared source-scanning primitives for the drift guards.
//!
//! Twelve guards in this crate walk `src/` looking for a pattern in
//! production code, and eight of them stripped comments with the same
//! line:
//!
//! ```ignore
//! let code = line.split("//").next().unwrap_or("");
//! ```
//!
//! which truncates at the first `//` **anywhere on the line, including
//! inside a string literal**. `src/util.rs:744` is
//! `Some(format!("https://{}", host.replace(…)))` — everything after
//! `https:` was invisible to every one of those guards. That is not
//! theoretical: it was demonstrated by planting an `arboard::Clipboard`
//! call after a URL literal on one line, which
//! `the_clipboard_is_only_reached_through_yank` — a guard whose entire
//! job is stopping tests from touching the developer's machine — passed
//! clean. Moving the same call *before* the URL made it fail, which
//! isolates the cause exactly.
//!
//! The lesson had already been learned once here and not generalised:
//! `literals_with_embedded_newlines` lexes with `proc_macro2` precisely
//! because five hand-rolled literal scanners were each wrong
//! differently. This is that fix applied to the other eight.

/// Strip line comments without cutting inside a string or char literal.
///
/// Walks the line tracking whether we are inside `"…"`, `'…'` or a raw
/// string, and only treats `//` as a comment when outside all of them.
/// Returns the code portion, which may be the whole line.
pub(crate) fn strip_line_comment(line: &str) -> &str {
    let b = line.as_bytes();
    let mut i = 0;
    let mut in_str = false;
    let mut in_char = false;
    while i < b.len() {
        match b[i] {
            b'\\' if in_str || in_char => {
                i += 2;
                continue;
            }
            b'"' if !in_char => in_str = !in_str,
            b'\'' if !in_str => {
                // A lifetime (`'a`) is not a char literal. Only flip on
                // something that looks like one: `'x'` or `'\n'`.
                let looks_like_char = b.get(i + 1) == Some(&b'\\')
                    || (b.get(i + 2) == Some(&b'\'') && b.get(i + 1).is_some());
                if in_char || looks_like_char {
                    in_char = !in_char;
                }
            }
            b'/' if !in_str && !in_char && b.get(i + 1) == Some(&b'/') => {
                return &line[..i];
            }
            _ => {}
        }
        i += 1;
    }
    line
}

/// Every `.rs` file under `src/`, as (path, contents).
///
/// Carries its own sanity floor: a walk that finds almost nothing is a
/// broken walk, and a guard over zero files passes vacuously.
pub(crate) fn source_files() -> Vec<(String, String)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                if let Ok(s) = std::fs::read_to_string(&p) {
                    out.push((p.to_string_lossy().into_owned(), s));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(std::path::Path::new("src"), &mut out);
    assert!(
        out.len() > 50,
        "source walk found only {} files — the walk is broken, and a guard \
         over nothing passes vacuously",
        out.len()
    );
    out
}

/// Find every production line matching `needle`, as `path:line`.
///
/// The shape ten guards in this crate open-code: walk `src/`, skip test
/// sources, strip comments, look for a string. Extracted so the
/// *detector* can be tested independently of the tree it walks —
/// previously the logic lived inside each `#[test]` body, so there was
/// no way to ask "does this detector detect anything?", and for at least
/// one of them the answer was "not always".
///
/// That is `CLAUDE.md`'s own rule turned on the guards: a guard is
/// production code for the invariant it holds, so it needs a test of its
/// own, not just the tree-walk that consumes it.
pub(crate) fn find_in_production(needle: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for (path, text) in source_files() {
        if is_test_path(&path) {
            continue;
        }
        for (n, line) in text.lines().enumerate() {
            if strip_line_comment(line).contains(needle) {
                hits.push(format!("{path}:{}", n + 1));
            }
        }
    }
    hits
}

/// Is this path test-only source? Kept beside the scan so every guard
/// agrees on the answer.
pub(crate) fn is_test_path(path: &str) -> bool {
    path.contains("/tests/") || path.ends_with("/tests.rs") || path.ends_with("tests.rs")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact shape that defeated eight guards.
    #[test]
    fn a_url_literal_does_not_truncate_the_line() {
        let line = r#"    Some(format!("https://{}", host)) "#;
        assert_eq!(
            strip_line_comment(line),
            line,
            "a `//` inside a string literal is not a comment"
        );
    }

    #[test]
    fn a_real_comment_is_stripped() {
        assert_eq!(strip_line_comment("let x = 1; // set x"), "let x = 1; ");
        assert_eq!(strip_line_comment("// whole line"), "");
    }

    /// The case that matters: code AFTER a url literal must survive, or
    /// a violation can hide behind one.
    #[test]
    fn code_after_a_url_literal_survives() {
        let line = r#"let u = "https://x"; let c = arboard::Clipboard::new();"#;
        assert!(
            strip_line_comment(line).contains("arboard::Clipboard"),
            "code after a URL literal must remain visible to the guards"
        );
    }

    #[test]
    fn a_comment_after_a_literal_is_still_stripped() {
        let line = r#"let u = "https://x"; // trailing note"#;
        let code = strip_line_comment(line);
        assert!(code.contains("https://x"));
        assert!(!code.contains("trailing note"));
    }

    #[test]
    fn escaped_quotes_and_lifetimes_do_not_confuse_it() {
        let line = r#"let s = "a \" b"; // c"#;
        assert!(!strip_line_comment(line).contains("c"));
        let lt = "fn f<'a>(x: &'a str) -> &'a str { x } // note";
        let code = strip_line_comment(lt);
        assert!(code.contains("&'a str"), "a lifetime is not a char literal");
        assert!(!code.contains("note"));
    }

    #[test]
    fn the_walk_finds_the_tree() {
        let files = source_files();
        assert!(files.iter().any(|(p, _)| p.ends_with("util.rs")));
    }

    /// The detector must find a planted violation. Without this, a guard
    /// that finds nothing is indistinguishable from a clean tree — which
    /// is the exact failure the `split("//")` bug produced.
    #[test]
    fn the_detector_finds_something_that_is_there() {
        // `arboard::` genuinely occurs once in production (inside `yank`).
        let hits = find_in_production("arboard::");
        assert!(
            !hits.is_empty(),
            "the detector found no `arboard::` anywhere — it is not detecting"
        );
        assert!(
            hits.iter().all(|h| !h.contains("/tests/")),
            "test sources must be excluded: {hits:?}"
        );
    }

    /// And must find nothing for something that genuinely is not there,
    /// or every guard built on it fires constantly and gets ignored.
    #[test]
    fn the_detector_finds_nothing_that_is_not_there() {
        assert!(find_in_production("ThisIdentifierDoesNotExistAnywhere").is_empty());
    }

    #[test]
    fn test_paths_are_recognised() {
        assert!(is_test_path("src/app/tests/safety.rs"));
        assert!(is_test_path("src/app/tests.rs"));
        assert!(!is_test_path("src/app/safety.rs"));
        assert!(!is_test_path("src/util.rs"));
    }
}

#[cfg(test)]
mod packaging {
    /// Nothing that is not source, docs or config may reach the crate.
    ///
    /// 0.34.1 was YANKED because its published tarball carried 22
    /// `mutants.out/` files, one of which held the build machine's
    /// hostname and username. `Cargo.toml`'s `exclude` was tightened
    /// then — and it excludes by NAME, so it stops the thing that
    /// happened and not the class.
    ///
    /// It happened again: `sed -i.bak` writes its backup IN PLACE, a
    /// `git add -A` swept it up, and a 55KB copy of a test file was
    /// committed and would have shipped in 0.36.0. Caught by review,
    /// not by any check.
    ///
    /// So this asserts on the tracked file list by EXTENSION rather
    /// than by name. `.gitignore` stops the accident; this stops it
    /// being committed deliberately or by a tool that ignores
    /// `.gitignore`.
    #[test]
    fn no_backup_or_scratch_files_are_tracked() {
        let out = std::process::Command::new("git")
            .args(["ls-files"])
            .output()
            .expect("git ls-files");
        assert!(out.status.success(), "git ls-files failed");
        let files = String::from_utf8_lossy(&out.stdout);
        assert!(
            files.lines().count() > 50,
            "only {} tracked files — the listing failed and this guard \
             would pass on an empty result",
            files.lines().count()
        );

        let bad: Vec<&str> = files
            .lines()
            .filter(|f| {
                f.ends_with(".bak")
                    || f.ends_with(".orig")
                    || f.ends_with(".rej")
                    || f.ends_with('~')
                    || f.contains("mutants.out/")
            })
            .collect();
        assert!(
            bad.is_empty(),
            "these are tracked and would be published in the crate \
             tarball: {bad:?}"
        );
    }
}

#[cfg(test)]
mod write_gate_convergence {
    /// Only the two gates may consult the shared write decision.
    ///
    /// The point of stage 1 was collapsing two gates into one decision.
    /// Nothing stops a future path calling `write_gate::decide` itself
    /// and rendering its own wording — precisely how the divergence
    /// arose the first time, and it would pass every other test.
    ///
    /// The CLI side already had a guard of this shape; this is its
    /// missing twin, found reviewing stage 1 rather than by anything
    /// failing. Goes through `scan::find_in_production` rather than a
    /// hand-rolled walk, per CLAUDE.md — eight guards once each carried
    /// their own comment stripper and all eight shared a blind spot.
    /// The two gates, plus the module itself. Anything else touching
    /// the shared decision is re-diverging.
    const ALLOWED: &[&str] = &["src/write_gate.rs", "src/app/safety.rs", "src/cli/mod.rs"];

    /// Extracted so the FILTER can be exercised directly. Inlined, the
    /// guard passed with the filter neutered — because a clean tree
    /// yields no hits and an always-empty result is indistinguishable
    /// from a correct one. Caught by mutation, not by reading.
    fn offenders_among(hits: &[String]) -> Vec<String> {
        hits.iter()
            .filter(|hit| !ALLOWED.iter().any(|a| hit.starts_with(a)))
            .cloned()
            .collect()
    }

    #[test]
    fn only_the_gates_consult_the_shared_decision() {
        let offenders = offenders_among(&super::find_in_production("write_gate::"));
        assert!(
            offenders.is_empty(),
            "these reach the shared write decision directly instead of \
             going through `deny_write` / `write_refusal`, which is how \
             the two gates diverged in the first place: {offenders:?}"
        );
    }

    /// The canary — proves the scan detects, on every run, rather than
    /// passing because the tree happens to be clean.
    #[test]
    fn the_convergence_guard_detects_what_it_looks_for() {
        // The detector is `find_in_production`, whose own accuracy
        // tests live beside it; what this pins is that the needle
        // matches a real call and not a mention in prose.
        assert!(
            super::strip_line_comment("let d = crate::write_gate::decide(&ctx);")
                .contains("write_gate::")
        );
        assert!(
            !super::strip_line_comment("// write_gate::decide is fine in a comment")
                .contains("write_gate::")
        );
        assert!(
            !super::strip_line_comment("if self.deny_write(&env, \"rollback\") {")
                .contains("write_gate::")
        );
        // The needle still matches something real. If it stopped, the
        // guard would pass vacuously.
        assert!(
            !super::find_in_production("write_gate::").is_empty(),
            "no production code references write_gate at all — the scan \
             has gone blind"
        );
        // And the FILTER does its job on synthetic input: a path
        // outside the allowlist is an offender, one inside it is not.
        // Without this the allowlist can be widened to everything and
        // the guard still passes, because a clean tree has no hits to
        // filter.
        assert_eq!(
            offenders_among(&["src/app/cmd_ops.rs:12".to_string()]).len(),
            1,
            "a path outside the allowlist must be flagged"
        );
        for ok in ALLOWED {
            assert!(
                offenders_among(&[format!("{ok}:1")]).is_empty(),
                "{ok} is a gate and must not be flagged"
            );
        }
    }
}
