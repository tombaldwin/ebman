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
fn chars_in_pattern(pat: &syn::Pat, out: &mut Vec<char>) {
    match pat {
        syn::Pat::TupleStruct(ts) => {
            let is_char_ctor = ts.path.segments.last().is_some_and(|s| s.ident == "Char")
                && ts
                    .path
                    .segments
                    .iter()
                    .any(|s| s.ident == "KeyCode" || s.ident == "Char");
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
            let guarded = arm
                .guard
                .as_ref()
                .is_some_and(|(_, g)| is_modifier_guard(g));
            let line = arm.pat.span().start().line;
            let mut chars = Vec::new();
            chars_in_pattern(&arm.pat, &mut chars);
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

/// A file that will not parse must be loud, not "clean".
#[test]
#[should_panic(expected = "could not parse source")]
fn unparseable_source_is_loud() {
    shadowed_key_arms("fn f( {");
}

/// The rule itself, against the real keymap.
#[test]
fn the_keymap_puts_guarded_key_arms_first() {
    let src =
        std::fs::read_to_string("src/app/input.rs").expect("the keymap lives at src/app/input.rs");
    let (violations, both_forms) = shadowed_key_arms(&src);

    assert!(
        violations.is_empty(),
        "ARCHITECTURE rule 4: an unguarded KeyCode::Char arm precedes the \
         guarded arm for the same character, so the chord is unreachable. \
         Move the guarded arm above it. {}",
        violations
            .iter()
            .map(|s| format!(
                "'{}' unguarded at input.rs:{} shadows the guard at :{}",
                s.ch, s.unguarded_line, s.guarded_line
            ))
            .collect::<Vec<_>>()
            .join("; ")
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
        checked >= 2,
        "expected more than one module to match on KeyCode::Char; the sweep \
         found {checked} and may be looking in the wrong place"
    );
    assert!(
        offenders.is_empty(),
        "ARCHITECTURE rule 4 violated:\n{}",
        offenders.join("\n")
    );
}
