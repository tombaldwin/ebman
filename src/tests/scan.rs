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

/// A file's production code as statements a guard can match whole: its
/// production half (test modules excised), line comments stripped, and
/// each line that starts with `.` joined to the one before. rustfmt
/// splits a long method chain one call per line, so a guard matching
/// `cfg.app_tabs.envs.insert(` line by line misses
/// `self\n.cfg\n.app_tabs\n.envs\n.insert(`, a shape production already
/// uses. Joined, both read the same.
pub(crate) fn statements_of(src: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in production_half(src).lines() {
        let code = strip_line_comment(line).trim();
        if code.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(prev) if code.starts_with('.') => prev.push_str(code),
            _ => out.push(code.to_string()),
        }
    }
    out
}

/// Every production statement (see [`statements_of`]) that `hit`
/// accepts, as `path: statement`. For a guard whose needle can be split
/// across lines; [`find_in_production`] reports line numbers, which a
/// joined statement no longer has.
pub(crate) fn production_statements_where(hit: impl Fn(&str) -> bool) -> Vec<String> {
    let mut out = Vec::new();
    for (path, text) in source_files() {
        if is_test_path(&path) {
            continue;
        }
        for st in statements_of(&text) {
            if hit(&st) {
                out.push(format!("{path}: {st}"));
            }
        }
    }
    out
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

/// Is this path test-only source: a `tests.rs`, or anything under a
/// `tests/` directory? Kept beside the scan so every guard agrees on the
/// answer. Exact on both counts: it was `ends_with("tests.rs")`, which
/// also took a production `contests.rs`, while a second copy of the rule
/// (`is_test_source`, now gone) had the exact form.
pub(crate) fn is_test_path(path: &str) -> bool {
    let path = std::path::Path::new(path);
    path.file_name().and_then(|f| f.to_str()) == Some("tests.rs")
        || path.components().any(|c| c.as_os_str() == "tests")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact shape that defeated eight guards.
    /// Test code in both shapes this repo has, and nothing else:
    /// over-excluding would switch every source guard off silently,
    /// which is worse than the failures they catch.
    #[test]
    fn is_test_path_takes_test_trees_and_nothing_else() {
        for test in [
            "src/aws/tests/eb.rs",
            "src/ui/tests.rs",
            "src/app/tests/refresh.rs",
            "src/freeze/tests.rs",
        ] {
            assert!(super::is_test_path(test), "{test}");
        }
        for prod in [
            "src/app.rs",
            "src/app/cmd_misc.rs",
            "src/ui/detail.rs",
            "src/app/latest_stacks.rs",
            "src/app/contests.rs",
            "src/attests/x.rs",
        ] {
            assert!(!super::is_test_path(prod), "{prod}");
        }
    }

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
/// Not a prefix. A file can have production code BETWEEN two test
/// modules (`cli/mod.rs` did), so anything that stops at the first one
/// loses the rest.
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
mod statements_of_tests {
    use super::statements_of;

    #[test]
    fn a_split_chain_is_one_statement_and_test_modules_and_comments_are_not_read() {
        let src = "fn f(app: &mut App) {\n    app\n        .cfg // the config\n        .app_tabs\n        .envs\n        .insert(k, v);\n    let x = 1;\n}\n\n#[cfg(test)]\nmod tests {\n    fn t() { app.cfg.app_tabs.envs.insert(k, v); }\n}\n";
        let st = statements_of(src);
        assert!(
            st.contains(&"app.cfg.app_tabs.envs.insert(k, v);".to_string()),
            "{st:#?}"
        );
        assert!(
            st.contains(&"let x = 1;".to_string()),
            "a statement after a chain stays its own"
        );
        assert_eq!(
            st.iter().filter(|s| s.contains("insert(")).count(),
            1,
            "the test module's insert is not production: {st:#?}"
        );
        assert!(
            !st.iter().any(|s| s.contains("the config")),
            "comments are stripped"
        );
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
            "production code AFTER a test module survives — `cli/mod.rs` had some, \
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

/// `src` split into its functions, as (name, body), each body running to
/// the next `fn` header. A closure stays in the function that spawns it,
/// which is the point: a task's audit line belongs with the dispatch
/// that launched it. A nested `fn` starts a new body; nothing here has
/// one where it matters.
pub(crate) fn fn_bodies(src: &str) -> Vec<(String, String)> {
    fn fn_name(line: &str) -> Option<&str> {
        let mut rest = line.trim_start();
        for prefix in ["pub(crate) ", "pub(super) ", "pub "] {
            rest = rest.strip_prefix(prefix).unwrap_or(rest);
        }
        rest = rest.strip_prefix("async ").unwrap_or(rest);
        let rest = rest.strip_prefix("fn ")?;
        let end = rest.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
        (end > 0).then(|| &rest[..end])
    }
    let mut out: Vec<(String, String)> = Vec::new();
    for line in src.lines() {
        let code = strip_line_comment(line);
        if let Some(name) = fn_name(code) {
            out.push((name.to_string(), String::new()));
        }
        if let Some((_, body)) = out.last_mut() {
            body.push_str(code);
            body.push('\n');
        }
    }
    out
}

#[test]
fn fn_bodies_splits_at_each_header_and_keeps_closures() {
    let src = "\
use x;
pub(crate) fn one(&self) {
    tokio::spawn(async move { audit(&region) });
}
    pub(super) async fn two_x() {} // fn three() in a comment
fn three<T>() {}
";
    let bodies = fn_bodies(src);
    let names: Vec<&str> = bodies.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["one", "two_x", "three"]);
    assert!(bodies[0].1.contains("audit(&region)"), "{:?}", bodies[0]);
    assert!(!bodies[1].1.contains("three"), "comments are stripped");
}
