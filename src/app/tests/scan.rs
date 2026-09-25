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

/// The production half of ONE source file, located by path suffix from
/// the crate root.
///
/// `include_str!` resolves relative to the file that writes it, so
/// moving a test re-points every guard inside it — and re-points them
/// SILENTLY when the new directory happens to contain a file of the
/// same name. `include_str!("writes.rs")` from `cli/mcp/tests/writes.rs`
/// reads the test file itself, and the guard then scans its own source
/// and passes. A path that stops existing fails loudly; a path that
/// resolves to the wrong file does not.
///
/// Locating from the root instead means the subject cannot move out
/// from under a guard, and an ambiguous or missing name is an assertion
/// rather than a wrong answer. Test sources are excluded, so a guard
/// can never end up reading itself.
pub(crate) fn production_source(suffix: &str) -> String {
    let hits: Vec<(String, String)> = source_files()
        .into_iter()
        .filter(|(p, _)| !is_test_path(p) && p.ends_with(suffix))
        .collect();
    // Zero and many are different mistakes and want different
    // advice. Zero is the likelier one — a rename, a typo, a file that
    // became a test path, the wrong working directory — and telling
    // someone to "qualify the suffix" there points at the opposite of
    // the fix.
    assert!(
        !hits.is_empty(),
        "`{suffix}` names no production file — it was renamed, misspelled, or \
         is now a test path (test sources are excluded here on purpose)"
    );
    assert_eq!(
        hits.len(),
        1,
        "`{suffix}` names {} production files ({:?}) — a guard must name exactly one \
         subject, so qualify the suffix with enough of its directory to be unique",
        hits.len(),
        hits.iter().map(|(p, _)| p).collect::<Vec<_>>()
    );
    production_half(&hits[0].1)
}

/// Is this path test-only source? Kept beside the scan so every guard
/// agrees on the answer.
pub(crate) fn is_test_path(path: &str) -> bool {
    path.contains("/tests/") || path.ends_with("/tests.rs") || path.ends_with("tests.rs")
}

/// Every file git tracks — which is what `cargo package` publishes,
/// bar `Cargo.toml`'s `exclude` and two files cargo generates.
///
/// `None` ONLY when there is genuinely no repository: `cargo mutants`
/// builds in a scratch copy of the tree with no `.git`, so `git
/// ls-files` fails there through no fault of the code under test. Any
/// other git failure panics — "git errored" and "there is no repo" are
/// different, and treating the first as the second is how a guard goes
/// quiet in the environment that matters.
///
/// Carries its own floor, like [`source_files`]: a listing that finds
/// almost nothing is a broken listing, and a guard over it passes
/// vacuously.
pub(crate) fn tracked_files() -> Option<Vec<String>> {
    let out = match std::process::Command::new("git")
        .args(["ls-files"])
        // `LC_ALL=C`, so the stderr match below is not locale-dependent.
        // git translates "not a git repository" — Apple Git ships no
        // translations, Homebrew and Linux git do — so on a localised
        // machine the skip condition would not match and every caller
        // would fail loudly instead of skipping. Loud is the safer wrong
        // direction, but a guard that cannot run where `cargo mutants`
        // runs is not much of a guard.
        .env("LC_ALL", "C")
        .output()
    {
        Ok(out) => out,
        Err(e) => panic!("could not run git: {e}"),
    };
    if !out.status.success() {
        let no_repo = String::from_utf8_lossy(&out.stderr).contains("not a git repository");
        assert!(
            no_repo,
            "git ls-files failed for a reason other than a missing \
             repository: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        return None;
    }
    let files: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    assert!(
        files.len() > 50,
        "only {} tracked files — the listing failed and a guard over it \
         would pass on an empty result",
        files.len()
    );
    Some(files)
}

/// The account IDs AWS's own documentation uses as placeholders. A
/// fixture using one of these is recognisably a fixture.
pub(crate) const AWS_DOC_ACCOUNT_IDS: &[&str] = &[
    "123456789012",
    "111122223333",
    "444455556666",
    "555555555555",
    "777788889999",
];

/// Every token in `text` that could be a REAL AWS account ID, as
/// (1-based line, value).
///
/// A candidate is a run of exactly twelve ASCII digits with no ASCII
/// letter, digit or `_` on either side — the shape of an account ID
/// standing alone, or as the account field of an ARN, where it sits
/// between `:` delimiters. So one rule covers both halves of the
/// backlog item: a separate ARN check would find nothing this does not.
///
/// The alphanumeric boundary is what keeps `Cargo.lock` quiet: its
/// checksums are hex, and hex routinely holds a twelve-digit run
/// between two letters.
///
/// Not candidates: [`AWS_DOC_ACCOUNT_IDS`], and repdigits
/// (`000000000000`, `111111111111`, …). The risk this exists for is a
/// real value that LOOKS like a fixture, and nobody mistakes a
/// repdigit for anything but a placeholder — so excluding the class is
/// a rule about the shape, not an allowlist of values.
pub(crate) fn account_id_candidates(text: &str) -> Vec<(usize, &str)> {
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut hits = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if !bytes[i].is_ascii_digit() {
                i += 1;
                continue;
            }
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let bounded = (start == 0 || !is_word(bytes[start - 1]))
                && (i == bytes.len() || !is_word(bytes[i]));
            if i - start != 12 || !bounded {
                continue;
            }
            // ASCII digits only, so this slice is on char boundaries.
            let run = &line[start..i];
            let repdigit = run.bytes().all(|b| b == run.as_bytes()[0]);
            if !repdigit && !AWS_DOC_ACCOUNT_IDS.contains(&run) {
                hits.push((idx + 1, run));
            }
        }
    }
    hits
}

/// `123456789012` -> `12…12`: enough to find in `path:line`, not
/// enough to be the leak. A guard whose job is stopping a real account
/// ID being published must not print one into a public CI log.
pub(crate) fn mask_account_id(id: &str) -> String {
    match (id.get(..2), id.get(id.len().saturating_sub(2)..)) {
        (Some(head), Some(tail)) if id.len() > 4 => format!("{head}…{tail}"),
        _ => "…".to_owned(),
    }
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
        // Through the shared helper, which skips only when there is
        // genuinely no repository (the `cargo mutants` scratch copy) and
        // carries the floor this guard used to hold inline.
        let Some(files) = super::tracked_files() else {
            return;
        };

        let bad: Vec<&str> = files
            .iter()
            .map(String::as_str)
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

    /// No published file may carry an account ID that could be real.
    ///
    /// Everything git tracks ships to crates.io, and docs.rs renders the
    /// test modules as browsable HTML. A crates.io version cannot be
    /// unpublished, only yanked, and a yanked version stays downloadable
    /// — so a real account ID pasted as a fixture is published, indexed
    /// and permanent. 0.34.1 was yanked for publishing a hostname; this
    /// is the same door.
    ///
    /// Review cannot be relied on for it: a real value beside genuine
    /// placeholders reads as "12 digits, test file, fine". The likeliest
    /// source is a test of redaction or masking — pasting a real value
    /// is how you prove the masking works — and `CHANGELOG.md`, which
    /// carries field reports from real fleets.
    ///
    /// Reads every tracked file lossily rather than skipping the ones
    /// that are not UTF-8: a text file with one stray byte must not drop
    /// out of the scan in silence.
    #[test]
    fn no_published_file_carries_a_real_looking_account_id() {
        let Some(files) = super::tracked_files() else {
            return;
        };
        let mut scanned = 0usize;
        let mut hits = Vec::new();
        for path in &files {
            // A tracked file deleted in the working tree is not read;
            // the floor below stops that becoming a vacuous pass.
            let Ok(bytes) = std::fs::read(path) else {
                continue;
            };
            scanned += 1;
            let text = String::from_utf8_lossy(&bytes);
            for (line, id) in super::account_id_candidates(&text) {
                hits.push(format!("{path}:{line}: {}", super::mask_account_id(id)));
            }
        }
        assert!(
            scanned > 50,
            "only {scanned} tracked files could be read — a scan over \
             nothing passes vacuously"
        );
        assert!(
            hits.is_empty(),
            "these look like real AWS account IDs and would be published \
             permanently in the crate: {hits:#?}\n\
             Replace each with one of AWS's documentation placeholders \
             ({:?}) or a repdigit. Do NOT add it to the placeholder list: \
             that list is AWS's, and widening it to quiet this guard is a \
             stop condition in CLAUDE.md. If it is not an account ID at \
             all, the detector is wrong for that shape — fix the detector.",
            super::AWS_DOC_ACCOUNT_IDS
        );
    }
}

/// The account-ID detector, tested apart from the tree it scans.
///
/// Every fixture here is ASSEMBLED at runtime from four-digit pieces.
/// This file is itself scanned by the guard above, so a twelve-digit
/// literal written out in a test would make the guard fire on the
/// tests that prove it works.
#[cfg(test)]
mod account_ids {
    use super::{account_id_candidates, mask_account_id, AWS_DOC_ACCOUNT_IDS};

    /// Twelve digits that are not a placeholder or a repdigit.
    fn plausible() -> String {
        ["1234", "5678", "9013"].concat()
    }

    #[test]
    fn a_bare_id_is_found_with_its_line() {
        let id = plausible();
        let text = format!("first line\naccount = {id}\nlast");
        assert_eq!(account_id_candidates(&text), vec![(2, id.as_str())]);
    }

    /// The ARN half of the backlog item, which the digit rule covers
    /// without a second check: the account field sits between `:`s.
    #[test]
    fn the_account_field_of_an_arn_is_found() {
        let id = plausible();
        let arn = format!("arn:aws:iam::{id}:role/deploy");
        assert_eq!(account_id_candidates(&arn), vec![(1, id.as_str())]);
    }

    #[test]
    fn a_candidate_at_either_end_of_a_line_is_found() {
        let id = plausible();
        assert_eq!(account_id_candidates(&id), vec![(1, id.as_str())]);
        assert_eq!(
            account_id_candidates(&format!("\"{id}\"")),
            vec![(1, id.as_str())]
        );
    }

    #[test]
    fn aws_documentation_placeholders_are_not_candidates() {
        for id in AWS_DOC_ACCOUNT_IDS {
            let arn = format!("arn:aws:sts::{id}:assumed-role/x/y");
            assert!(
                account_id_candidates(&arn).is_empty(),
                "{id} is an AWS documentation placeholder"
            );
        }
    }

    #[test]
    fn repdigits_are_not_candidates() {
        for d in '0'..='9' {
            let rep: String = std::iter::repeat_n(d, 12).collect();
            assert!(account_id_candidates(&rep).is_empty(), "{rep}");
        }
    }

    /// Each of these was a live false positive, or is one shape away
    /// from one: `Cargo.lock` checksums are hex and hold twelve-digit
    /// runs between letters; `999999999999d` is a duration literal.
    #[test]
    fn digits_inside_a_larger_word_are_not_candidates() {
        let id = plausible();
        for text in [
            format!("checksum = \"ab{id}cd\""),
            format!("{id}d"),
            format!("x_{id}"),
            format!("{id}_x"),
            format!("{id}4"),     // thirteen digits
            format!("4{id}"),     // thirteen digits
            id[..11].to_string(), // eleven digits
        ] {
            assert!(account_id_candidates(&text).is_empty(), "{text}");
        }
    }

    #[test]
    fn a_masked_id_locates_without_leaking() {
        let id = plausible();
        let masked = mask_account_id(&id);
        assert_eq!(masked, "12…13");
        assert!(!masked.contains(&id[2..10]));
    }

    /// Too short to mask meaningfully: reveal nothing rather than most
    /// of it. Unreachable from the detector, which only yields twelve
    /// digits — but it is a public helper and its edge is its contract.
    #[test]
    fn a_value_too_short_to_mask_reveals_nothing() {
        for short in ["", "1", "1234"] {
            assert_eq!(mask_account_id(short), "…", "{short:?}");
        }
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

/// The production half of a source file: every line outside a
/// `#[cfg(test)]` module.
///
/// Six guards hand-rolled this as `src.split("#[cfg(test)]").next()`,
/// which truncates at the first INLINE test-only item — a
/// `#[cfg(test)] fn for_tests(...)` seam, of which this codebase has
/// many — rather than at the test module. Measured on 2026-09-19:
/// `cli/mcp/mod.rs` guards saw 253 of 988 production lines,
/// `writes.rs` 489 of 1425, `tools.rs` 740 of 1734. Each guard
/// reported clean over roughly a third of its subject.
///
/// This is the `line.split("//").next()` lesson with a different
/// splitter, and CLAUDE.md already records that five hand-rolled
/// scanners taught it once and it was not generalised. So: one
/// implementation, here, with its own accuracy test.
///
/// Not a prefix. `cli/mod.rs` has production code BETWEEN two test
/// modules, so anything that stops at the first one loses the rest.
/// Test modules are excised wherever they appear and the remainder is
/// joined.
pub(crate) fn production_half(src: &str) -> String {
    /// Is this line a top-level `mod` declaration? Accepts the
    /// visibility forms that actually occur (`pub(crate) mod tests;`
    /// in `app.rs`), because missing one leaks the declaration into
    /// the production half — or worse, see below.
    fn mod_decl(line: &str) -> Option<&str> {
        let rest = line
            .strip_prefix("pub(crate) ")
            .or_else(|| line.strip_prefix("pub(super) "))
            .or_else(|| line.strip_prefix("pub "))
            .unwrap_or(line);
        rest.starts_with("mod ").then_some(rest)
    }

    let lines: Vec<&str> = src.lines().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < lines.len() {
        // A top-level test MODULE, which rustfmt guarantees sits at
        // column 0. An inline `#[cfg(test)]` item is one declaration
        // and stays in view — treating it as a boundary is the bug
        // this function exists to remove.
        if lines[i].trim_end() != "#[cfg(test)]" {
            out.push_str(lines[i]);
            out.push('\n');
            i += 1;
            continue;
        }
        // `#[path = "..."]` may sit between the attribute and the
        // `mod`, which is how the relocated mcp tests are spelled.
        let mut j = i + 1;
        if lines
            .get(j)
            .is_some_and(|l| l.trim_start().starts_with("#[path"))
        {
            j += 1;
        }
        let Some(decl) = lines.get(j).and_then(|l| mod_decl(l)) else {
            out.push_str(lines[i]);
            out.push('\n');
            i += 1;
            continue;
        };
        if decl.trim_end().ends_with(';') {
            // OUT-OF-LINE: `mod tests;`. There is no body here, so
            // there is nothing to skip past — and skipping to the next
            // column-0 `}` eats whatever production code follows.
            //
            // It did. `src/aws.rs` declares its tests mid-file, and
            // this function was silently deleting the 27 lines after
            // it — including the whole `pub(crate) struct
            // AwsErrorMeta`. Every guard reading that production half
            // was blind to a public type, and would have passed over a
            // violation inside it. The three other files with this
            // spelling happen to sit at EOF, which is the only reason
            // it cost nothing there.
            i = j + 1;
            continue;
        }
        // INLINE: skip to the module's closing brace at column 0,
        // because the module is at column 0. Brace COUNTING was tried
        // and is wrong — a `{` inside a format string unbalances it,
        // and this codebase is full of `"{{\"pending\":true"`-shaped
        // literals.
        i = j + 1;
        while i < lines.len() {
            let closes = lines[i] == "}";
            i += 1;
            if closes {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod production_source_tests {
    use super::*;

    /// The helper must read the PRODUCTION file, not a same-named test
    /// file — the failure it exists to prevent.
    #[test]
    fn locates_production_not_the_same_named_test_file() {
        // `cli/mcp/writes.rs` and `cli/mcp/tests/writes.rs` both end
        // with `writes.rs`; only one of them is the subject.
        let src = production_source("cli/mcp/writes.rs");
        assert!(
            src.contains("pub(super) enum WriteVerb"),
            "production writes.rs must carry the write verbs"
        );
        assert!(
            !src.contains("#[test]"),
            "the production half must not be the test file"
        );
    }

    /// An ambiguous name is an assertion, not a coin flip over which
    /// file a guard ends up scanning.
    #[test]
    #[should_panic(expected = "a guard must name exactly one subject")]
    fn an_ambiguous_suffix_panics() {
        // Two production `mod.rs` under `src/` is the common case; pick
        // a suffix that cannot be unique.
        let _ = production_source("mod.rs");
    }

    #[test]
    #[should_panic(expected = "names no production file")]
    fn a_missing_subject_panics() {
        let _ = production_source("no-such-file-anywhere.rs");
    }
}

#[cfg(test)]
mod production_half_tests {

    /// An OUT-OF-LINE test module declaration has no body here, so
    /// nothing after it may be dropped.
    ///
    /// This was live: `src/aws.rs` declares `mod tests;` mid-file and
    /// the old implementation skipped to the next column-0 `}`,
    /// deleting 27 lines of production code including a public struct.
    #[test]
    fn an_out_of_line_test_module_does_not_eat_what_follows() {
        let src = "fn before() {\n}\n\n#[cfg(test)]\nmod tests;\n\n\
                   pub(crate) struct KeepMe {\n    pub a: u8,\n}\n";
        let prod = super::production_half(src);
        assert!(prod.contains("fn before()"), "{prod}");
        assert!(
            prod.contains("pub(crate) struct KeepMe"),
            "everything after an out-of-line declaration is production: {prod}"
        );
        assert!(!prod.contains("mod tests;"), "the declaration goes: {prod}");
    }

    /// The real file the bug was found in, so the case cannot drift
    /// away from its subject.
    #[test]
    fn aws_rs_keeps_the_type_declared_after_its_test_module() {
        let prod = super::production_source("aws.rs");
        assert!(
            prod.contains("pub(crate) struct AwsErrorMeta"),
            "`AwsErrorMeta` is declared after `mod tests;` in aws.rs and must \
             survive the split"
        );
    }

    /// The `#[path]` spelling the relocated mcp tests use is a test
    /// module too, and must be excised rather than left in the
    /// production half.
    #[test]
    fn a_path_attributed_test_module_is_excised() {
        let src = "fn before() {\n}\n\n#[cfg(test)]\n#[path = \"tests/x.rs\"]\nmod tests;\n\n\
                   fn after() {\n}\n";
        let prod = super::production_half(src);
        assert!(prod.contains("fn before()"));
        assert!(prod.contains("fn after()"), "{prod}");
        assert!(!prod.contains("#[path"), "the declaration goes: {prod}");
        assert!(!prod.contains("mod tests;"), "{prod}");
    }

    /// A `pub(crate) mod tests;` — the spelling `app.rs` uses — is
    /// still a test module.
    #[test]
    fn a_visibility_qualified_test_module_is_excised() {
        let src = "fn before() {\n}\n\n#[cfg(test)]\npub(crate) mod tests;\n\nfn after() {\n}\n";
        let prod = super::production_half(src);
        assert!(prod.contains("fn after()"), "{prod}");
        assert!(!prod.contains("mod tests;"), "{prod}");
    }

    /// The splitter six guards hand-rolled, and the blind spot they
    /// all shared.
    ///
    /// `src.split("#[cfg(test)]").next()` stops at the first INLINE
    /// test-only item, of which this codebase has many. Measured
    /// 2026-09-19 before the fix: `cli/mcp/mod.rs` guards saw 253 of
    /// 988 production lines, `writes.rs` 489 of 1425, `tools.rs` 740
    /// of 1734. Each reported clean over roughly a third of its
    /// subject — and widening the scan immediately surfaced a real
    /// violation that had been invisible.
    #[test]
    fn an_inline_test_item_does_not_end_the_production_half() {
        let src = "fn a() {}\n\
                   #[cfg(test)]\n\
                   fn only_for_tests() {}\n\
                   fn b() {}\n";
        let prod = super::production_half(src);
        assert!(prod.contains("fn a()"), "{prod}");
        assert!(
            prod.contains("fn b()"),
            "an inline `#[cfg(test)]` item is ONE declaration, not a boundary — \
             stopping there is what hid two thirds of three files: {prod}"
        );
    }

    #[test]
    fn a_test_module_is_excised_and_code_after_it_survives() {
        let src = "fn a() {}\n\
                   #[cfg(test)]\n\
                   mod tests {\n\
                   fn hidden() {}\n\
                   }\n\
                   fn b() {}\n";
        let prod = super::production_half(src);
        assert!(prod.contains("fn a()"));
        assert!(
            !prod.contains("fn hidden()"),
            "the module body is test-only and must not be scanned: {prod}"
        );
        assert!(
            prod.contains("fn b()"),
            "production code AFTER a test module survives — `cli/mod.rs` has some, \
             so anything treating this as a prefix loses it: {prod}"
        );
    }

    /// Brace COUNTING was tried and is wrong here.
    #[test]
    fn a_brace_inside_a_string_does_not_end_the_module() {
        let src = "fn a() {}\n\
                   #[cfg(test)]\n\
                   mod tests {\n\
                   let s = \"{{\\\"pending\\\":true}}\";\n\
                   fn hidden() {}\n\
                   }\n\
                   fn b() {}\n";
        let prod = super::production_half(src);
        assert!(
            !prod.contains("fn hidden()"),
            "a `{{` in a format string must not close the module early — this \
             codebase is full of them: {prod}"
        );
        assert!(prod.contains("fn b()"), "{prod}");
    }

    /// The real files, so the helper is exercised on its actual subject.
    #[test]
    fn the_real_sources_keep_their_production_code() {
        for (path, needle) in [
            ("src/cli/mcp/mod.rs", "fn call_timeout_secs"),
            ("src/cli/mcp/writes.rs", "fn dispatch_dlq_batch"),
            ("src/cli/mcp/tools.rs", "fn tool_doctor"),
        ] {
            let src = std::fs::read_to_string(path).expect("read");
            let prod = super::production_half(&src);
            assert!(
                prod.contains(needle),
                "`{needle}` is production code in {path} and a guard scanning it \
                 must see it — the old splitter did not"
            );
            assert!(
                !prod.contains("#[tokio::test]"),
                "{path}: test bodies must be excised"
            );
        }
    }
}
