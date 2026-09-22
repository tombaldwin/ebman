//! ARCHITECTURE rule 4: a guarded `KeyCode::Char(c) if <modifier>` arm must
//! come BEFORE the unguarded arm for the same character.
//!
//! Rust tries arms top to bottom, so an unguarded `KeyCode::Char('d')` placed
//! first swallows `Ctrl-D` — the guarded arm below it never runs and the chord
//! silently does the unmodified thing. The compiler stays quiet: both arms are
//! reachable *patterns*, and it is only the guard that makes one a subset of
//! the other.
//!
//! This was the one rule in `ARCHITECTURE.md` with nothing behind it, and it
//! has bitten. Neither existing mechanism can express it:
//!
//! - **A source scan can't**, because judging arm order means knowing which
//!   `match` an arm belongs to, and a line-level scan cannot tell. A naive
//!   attempt at exactly this reported four violations in `input.rs`, all
//!   false — it compared arms sitting in different `match` blocks, and
//!   attributed two of them to a function neither was in.
//! - **Mutation testing can't**, because `cargo-mutants` deletes bodies and
//!   flips operators. It does not permute match arms.
//!
//! So this one parses. `syn` hands back `Expr::Match` → `Arm { pat, guard }`,
//! which is precisely the question the rule asks. Matching on the AST rather
//! than on rendered text is the point: this file asks "is this pattern a
//! `KeyCode::Char` tuple-struct whose one field is a char literal", not "does
//! this line contain some characters".

use syn::visit::Visit;

/// One `KeyCode::Char(..)` arm: which char, whether it carries a modifier
/// guard, and where it sits within its own `match`.
#[derive(Debug, Clone)]
struct CharArm {
    ch: char,
    guarded: bool,
    index: usize,
    line: usize,
}

/// An unguarded arm for `ch` preceding a guarded one in the same `match`.
#[derive(Debug, Clone)]
struct Shadowed {
    ch: char,
    unguarded_line: usize,
    guarded_line: usize,
}

/// Idents a guard expression mentions, so a *modifier* guard can be told from
/// any other guard.
///
/// The distinction matters both ways. `if key.modifiers.contains(CONTROL)` is
/// the rule-4 shape. `if *cursor > 0` is not, and an arm guarded on that
/// legitimately precedes the unguarded arm for the same char — calling it a
/// violation is how a blunt detector invents work.
#[derive(Default)]
struct IdentCollector {
    idents: Vec<String>,
}

impl<'ast> Visit<'ast> for IdentCollector {
    fn visit_ident(&mut self, i: &'ast proc_macro2::Ident) {
        self.idents.push(i.to_string());
    }
}

fn is_modifier_guard(guard: &syn::Expr) -> bool {
    let mut c = IdentCollector::default();
    c.visit_expr(guard);
    c.idents.iter().any(|i| {
        matches!(
            i.as_str(),
            "modifiers" | "KeyModifiers" | "CONTROL" | "SHIFT" | "ALT" | "SUPER"
        )
    })
}

/// Every char literal reached through a `KeyCode::Char(..)` pattern, following
/// the pattern nesting the keymap actually uses: `|` alternations, tuple
/// patterns like `(KeyCode::Char('y'), Mode::Detail)`, parens and references.
/// The guard expression on a match arm.
///
/// `syn` 3 removed `Arm::guard` and represents a guarded arm as
/// `Pat::Guard { pat, guard }` — the guard moved from the arm to the
/// pattern. Extracted into a named function because the same structural
/// change has to be applied in two places, and missing the second one
/// leaves this rule compiling and checking nothing.
fn arm_guard(arm: &syn::Arm) -> Option<&syn::Expr> {
    match &arm.pat {
        syn::Pat::Guard(g) => Some(&g.guard),
        _ => None,
    }
}

/// The pattern with any guard wrapper removed.
///
/// Under `syn` 3 a guarded arm's `pat` IS the `Pat::Guard` wrapper, so
/// a walker looking for `KeyCode::Char(..)` finds nothing inside it.
/// This rule's whole job is ordering guarded Ctrl arms against
/// unguarded ones for the same character, so failing to unwrap would
/// make it see no guarded arms at all.
fn unguarded_pat(pat: &syn::Pat) -> &syn::Pat {
    match pat {
        syn::Pat::Guard(g) => &g.pat,
        other => other,
    }
}

fn chars_in_pattern(pat: &syn::Pat, out: &mut Vec<char>) {
    match pat {
        syn::Pat::TupleStruct(ts) => {
            // `KeyCode::Char(..)`, or a bare `Char(..)` under a glob
            // import. The qualifier check has to look at the segment
            // immediately before `Char`: written as "is KeyCode anywhere in
            // the path" it is satisfied by the `Char` segment itself and so
            // excludes nothing, which would admit some unrelated enum's
            // `Char(char)` variant into the rule.
            let segs: Vec<String> = ts
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            let is_char_ctor = segs.last().is_some_and(|s| s == "Char")
                && segs.iter().rev().nth(1).is_none_or(|q| q == "KeyCode");
            if is_char_ctor {
                for elem in &ts.elems {
                    if let syn::Pat::Lit(lit) = elem {
                        if let syn::Lit::Char(c) = &lit.lit {
                            out.push(c.value());
                        }
                    }
                }
            }
            // A `KeyCode::Char` never nests another, but other tuple structs
            // can wrap one — keep descending either way.
            for elem in &ts.elems {
                chars_in_pattern(elem, out);
            }
        }
        syn::Pat::Or(or) => {
            for case in &or.cases {
                chars_in_pattern(case, out);
            }
        }
        syn::Pat::Tuple(t) => {
            for elem in &t.elems {
                chars_in_pattern(elem, out);
            }
        }
        syn::Pat::Slice(s) => {
            for elem in &s.elems {
                chars_in_pattern(elem, out);
            }
        }
        syn::Pat::Struct(s) => {
            for field in &s.fields {
                chars_in_pattern(&field.pat, out);
            }
        }
        syn::Pat::Paren(p) => chars_in_pattern(&p.pat, out),
        syn::Pat::Reference(r) => chars_in_pattern(&r.pat, out),
        syn::Pat::Type(t) => chars_in_pattern(&t.pat, out),
        syn::Pat::Ident(i) => {
            if let Some((_, sub)) = &i.subpat {
                chars_in_pattern(sub, out);
            }
        }
        _ => {}
    }
}

#[derive(Default)]
struct MatchVisitor {
    violations: Vec<Shadowed>,
    /// Chars seen in both forms *somewhere*, used to prove the guard is
    /// actually looking at something.
    both_forms: Vec<char>,
}

impl<'ast> Visit<'ast> for MatchVisitor {
    fn visit_expr_match(&mut self, m: &'ast syn::ExprMatch) {
        use syn::spanned::Spanned as _;

        let mut arms: Vec<CharArm> = Vec::new();
        for (index, arm) in m.arms.iter().enumerate() {
            // `syn` 3 removed `Arm::guard`: a guarded arm is now a
            // `Pat::Guard { pat, guard }`, following Rust's
            // guard-patterns RFC. The guard moved from the ARM to the
            // PATTERN, so both reads below had to move with it.
            let guarded = arm_guard(arm).is_some_and(is_modifier_guard);
            let line = arm.pat.span().start().line;
            let mut chars = Vec::new();
            // Unwrap the guard wrapper before walking for characters.
            // Without this a guarded arm contributes NO chars, so the
            // rule silently stops seeing exactly the arms it exists to
            // order — a guard that compiles, runs, and checks nothing.
            chars_in_pattern(unguarded_pat(&arm.pat), &mut chars);
            for ch in chars {
                arms.push(CharArm {
                    ch,
                    guarded,
                    index,
                    line,
                });
            }
        }

        // Within THIS match only: does an unguarded arm precede a guarded one
        // for the same char?
        for a in arms.iter().filter(|a| a.guarded) {
            if let Some(earlier) = arms
                .iter()
                .find(|b| b.ch == a.ch && !b.guarded && b.index < a.index)
            {
                self.violations.push(Shadowed {
                    ch: a.ch,
                    unguarded_line: earlier.line,
                    guarded_line: a.line,
                });
            }
            if arms.iter().any(|b| b.ch == a.ch && !b.guarded) && !self.both_forms.contains(&a.ch) {
                self.both_forms.push(a.ch);
            }
        }

        syn::visit::visit_expr_match(self, m);
    }
}

/// Rule-4 violations in one file's source, plus the chars that appear in both
/// guarded and unguarded form (the surface the rule has to police).
fn shadowed_key_arms(src: &str) -> (Vec<Shadowed>, Vec<char>) {
    let file = match syn::parse_file(src) {
        Ok(f) => f,
        // A parse failure must not read as "no violations" — that is the
        // vacuous pass this codebase has shipped before.
        Err(e) => panic!("could not parse source for the key-arm guard: {e}"),
    };
    let mut v = MatchVisitor::default();
    v.visit_file(&file);
    (v.violations, v.both_forms)
}

/// The guard has to FIND a planted violation. Without this it could return an
/// empty vec forever and read as a clean tree.
#[test]
fn an_unguarded_arm_shadowing_a_guarded_one_is_found() {
    let src = r#"
        fn f(key: KeyEvent) {
            match key.code {
                KeyCode::Char('d') => self.detail(),
                KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => self.dlq(),
                _ => {}
            }
        }
    "#;
    let (v, _) = shadowed_key_arms(src);
    assert_eq!(v.len(), 1, "the shadowed Ctrl-D arm must be found: {v:?}");
    assert_eq!(v[0].ch, 'd');
    assert!(
        v[0].unguarded_line < v[0].guarded_line,
        "the report names the offending order: {v:?}"
    );
}

#[test]
fn the_correct_order_is_clean() {
    let src = r#"
        fn f(key: KeyEvent) {
            match key.code {
                KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => self.dlq(),
                KeyCode::Char('d') => self.detail(),
                _ => {}
            }
        }
    "#;
    assert!(shadowed_key_arms(src).0.is_empty());
}

/// The false-positive shape that defeated the line-level attempt: the same
/// char in two DIFFERENT matches, unguarded first in one of them.
#[test]
fn arms_in_different_matches_do_not_shadow_each_other() {
    let src = r#"
        fn f(key: KeyEvent) {
            match a {
                KeyCode::Char('k') => self.up(),
                _ => {}
            }
            match b {
                KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => self.top(),
                _ => {}
            }
        }
    "#;
    assert!(
        shadowed_key_arms(src).0.is_empty(),
        "arms in separate matches are independent — this is exactly what a \
         line-level scan gets wrong"
    );
}

/// A non-modifier guard is not a rule-4 guard.
#[test]
fn a_non_modifier_guard_is_not_treated_as_one() {
    let src = r#"
        fn f() {
            match key.code {
                KeyCode::Char('k') if *cursor > 0 => self.up(),
                KeyCode::Char('k') => self.wrap(),
                _ => {}
            }
        }
    "#;
    assert!(shadowed_key_arms(src).0.is_empty());
}

/// The keymap nests: `(KeyCode::Char('y'), Mode::Detail)` and `'a' | 'b'`.
#[test]
fn chars_are_found_through_tuples_and_alternations() {
    let src = r#"
        fn f() {
            match (key.code, mode) {
                (KeyCode::Char('y') | KeyCode::Char('Y'), Mode::Detail) => self.yank(),
                (KeyCode::Char('y'), _) if key.modifiers.contains(KeyModifiers::CONTROL) => self.other(),
                _ => {}
            }
        }
    "#;
    let (v, both) = shadowed_key_arms(src);
    assert_eq!(v.len(), 1, "the tuple-nested 'y' pair must be seen: {v:?}");
    assert_eq!(both, vec!['y']);
}

/// Some other enum's `Char(char)` variant is not a key. Before the qualifier
/// check looked at the segment immediately before `Char`, it asked whether
/// `KeyCode` appeared anywhere in the path — which the `Char` segment itself
/// satisfies, so the check excluded nothing.
#[test]
fn an_unrelated_char_variant_is_not_a_key_arm() {
    let src = r#"
        fn f() {
            match token {
                Token::Char('d') => self.a(),
                Token::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => self.b(),
                _ => {}
            }
        }
    "#;
    let (v, both) = shadowed_key_arms(src);
    assert!(v.is_empty(), "Token::Char is not KeyCode::Char: {v:?}");
    assert!(both.is_empty(), "and it is not part of the policed surface");
}

/// A file that will not parse must be loud, not "clean".
#[test]
#[should_panic(expected = "could not parse source")]
fn unparseable_source_is_loud() {
    shadowed_key_arms("fn f( {");
}

/// The rule itself, against the real keymap.
#[test]
fn the_keymap_puts_guarded_key_arms_first() {
    // Every file the keymap lives in, not one hard-coded path.
    //
    // This read `src/app/input.rs` alone. Five modes already delegate
    // to `mode_keys.rs`, and moving the last two there would have left
    // this test passing while policing an emptier file — a guard clean
    // against the violation it exists to stop, which this repo has
    // shipped before. The tree-wide sweep below would still have
    // caught a violation; what would have been lost silently is the
    // NON-VACUITY check at the end, which is the half that notices the
    // rule has gone quiet.
    const KEYMAPS: [&str; 2] = ["src/app/input.rs", "src/app/mode_keys.rs"];
    // Parsed per file and merged — concatenating two modules is not
    // valid Rust and the parser rightly refuses it, which is the
    // failure mode this guard's own panic exists to make loud.
    // Formatted per file, so each line number keeps the path it came
    // from. Merging the raw `Shadowed` values first lost that — the
    // report said "keymap line 546" across two files, which sends the
    // reader to the wrong place or to both.
    let mut violations: Vec<String> = Vec::new();
    let mut both_forms = std::collections::BTreeSet::new();
    for path in KEYMAPS {
        let src = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let (v, f) = shadowed_key_arms(&src);
        violations.extend(v.iter().map(|s| {
            format!(
                "'{}' unguarded at {}:{} shadows the guard at :{}",
                s.ch, path, s.unguarded_line, s.guarded_line
            )
        }));
        both_forms.extend(f);
    }

    assert!(
        violations.is_empty(),
        "ARCHITECTURE rule 4: an unguarded KeyCode::Char arm precedes the \
         guarded arm for the same character, so the chord is unreachable. \
         Move the guarded arm above it. {}",
        violations.join("; ")
    );

    // Non-vacuous: if the keymap ever stops having chars in both forms, this
    // test is passing on an empty set and should be re-pointed, not deleted.
    assert!(
        both_forms.len() >= 5,
        "expected the keymap to still have several chars in both guarded and \
         unguarded form for this rule to police; found {both_forms:?}"
    );
}

/// Rule 4 is not confined to `input.rs` — any module that matches on
/// `KeyCode::Char` is subject to it. Sweep the tree so a new keymap surface
/// inherits the guard instead of needing to remember it.
#[test]
fn every_source_file_puts_guarded_key_arms_first() {
    let mut offenders: Vec<String> = Vec::new();
    // 20 files match today. A floor of 2 would still have passed on a
    // walk that had collapsed to almost nothing.
    let mut checked = 0usize;
    for (path, src) in super::scan::source_files() {
        if !src.contains("KeyCode::Char") {
            continue;
        }
        checked += 1;
        let (violations, _) = shadowed_key_arms(&src);
        for s in violations {
            offenders.push(format!(
                "{}:{} — unguarded '{}' shadows the guarded arm at :{}",
                path, s.unguarded_line, s.ch, s.guarded_line
            ));
        }
    }
    assert!(
        checked >= 10,
        "expected at least 10 modules to match on KeyCode::Char; the sweep \
         found {checked} and is probably looking in the wrong place"
    );
    assert!(
        offenders.is_empty(),
        "ARCHITECTURE rule 4 violated:\n{}",
        offenders.join("\n")
    );
}

/// The canary: the rule must still DETECT a violation.
///
/// Without this the sweep passes identically whether it is working or
/// blind — a clean tree and a broken parser look the same. That became
/// concrete with `syn` 3, which moved the guard from the ARM to the
/// PATTERN: `chars_in_pattern` on a `Pat::Guard` wrapper finds no
/// characters, so the rule would have compiled, run, reported zero
/// offenders, and checked nothing.
#[test]
fn the_key_arm_rule_can_see_a_violation() {
    // An unguarded 'p' BEFORE the Ctrl-guarded 'p' — the shadowing this
    // rule exists to stop, since the compiler does not warn on it.
    let bad = r#"
        fn handle(k: KeyCode, m: KeyModifiers) {
            match k {
                KeyCode::Char('p') => purge(),
                KeyCode::Char('p') if m.contains(KeyModifiers::CONTROL) => pin(),
                _ => {}
            }
        }
    "#;
    let (violations, _) = shadowed_key_arms(bad);
    assert_eq!(
        violations.len(),
        1,
        "the rule must flag an unguarded arm shadowing a guarded one: {violations:?}"
    );
    assert_eq!(violations[0].ch, 'p');

    // And the correct order must NOT be flagged, or the rule is just
    // noise that everyone learns to ignore.
    let good = r#"
        fn handle(k: KeyCode, m: KeyModifiers) {
            match k {
                KeyCode::Char('p') if m.contains(KeyModifiers::CONTROL) => pin(),
                KeyCode::Char('p') => purge(),
                _ => {}
            }
        }
    "#;
    let (violations, chars_seen) = shadowed_key_arms(good);
    assert!(
        violations.is_empty(),
        "correct order must pass: {violations:?}"
    );
    assert!(
        chars_seen.contains(&'p'),
        "and the walk must have SEEN the 'p' arms — an empty set here \
         means the parser stopped extracting characters from guarded \
         patterns, which is exactly how this rule goes quiet: {chars_seen:?}"
    );
}

/// Overlay key routing must stay exhaustive.
///
/// `Overlay::key_route` replaced an if-chain whose precedence lived in
/// statement order and which a new variant could silently miss. The
/// protection is that the match names every variant, so adding one
/// without classifying it fails to compile — and the single edit that
/// removes that protection while still compiling is a `_` arm.
///
/// Found its own justification on the way in: a hand-written list of
/// the variants missed `About`, and the compiler named it in seconds.
#[test]
fn overlay_key_routing_has_no_wildcard_arm() {
    let src = std::fs::read_to_string("src/app/input.rs").expect("input.rs");
    let body = super::scan::production_half(&src);
    let start = body
        .find("fn key_route(&self) -> OverlayRoute {")
        .expect("the routing classification must exist");
    // Brace-counted, not `find("\n    }")`. That worked only because
    // every interior line happens to be more deeply indented; a
    // rustfmt reflow could end the slice early, and the wildcard check
    // would then scan a truncated region and pass.
    let mut depth = 0i32;
    let mut end = start;
    for (i, c) in body[start..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = start + i + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(end > start, "could not find the end of `key_route`");
    let m = &body[start..end];

    for wildcard in ["_ =>", "_=>"] {
        assert!(
            !m.contains(wildcard),
            "a wildcard arm routes every future overlay to one branch by default, \
             which is the forgetting this classification exists to prevent — name \
             the variant instead: {m}"
        );
    }

    // Non-vacuous: the slice must actually be the match.
    assert!(
        m.matches("Overlay::").count() >= 10,
        "expected the classification to name every variant; found {} — the slice \
         is probably not the match body",
        m.matches("Overlay::").count()
    );
}
