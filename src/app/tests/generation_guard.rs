//! ARCHITECTURE rule 3: a result from a superseded context is dropped.
//!
//! Every spawned task captures the `generation` it launched at. If the
//! operator switches region, profile or account while it's in flight,
//! `generation` advances and the result must not be applied to the new
//! context.
//!
//! The design here is better than the other four rules get: there is a single
//! enforcement point. `AppMsg::generation()` classifies each variant, and
//! `handle_msg` drops the message once, before dispatching, so no individual
//! handler carries its own guard. The match in `generation()` is exhaustive,
//! so the compiler *forces* a new variant to be classified.
//!
//! Which is exactly why this file exists. The compiler makes you classify;
//! it does not make you classify *correctly*, and the cheapest way to satisfy
//! it is to append the new variant to the `None` arm — a one-line change with
//! a plausible reason that silently exempts a whole result path from the
//! invariant. That arm was documented and untested, which is the shape this
//! codebase keeps finding: correct, commented, and deletable without anything
//! noticing.
//!
//! So the classification is checked two ways. Structurally, carrying a
//! `gen: u64` field and being classified `Some` have to agree — that part
//! needs no allowlist and no judgement, because a variant that carries a
//! generation and is exempted from the generation check is unambiguously
//! wrong. Only variants with no `gen` field at all need a recorded reason,
//! and there are three.
//!
//! And behaviourally, because the structural half says nothing about whether
//! `handle_msg` still *acts* on the classification.

use super::support::{mk_application, test_app};
use crate::app::AppMsg;
use syn::visit::Visit;

/// The variants deliberately exempt from the generation check, and why.
///
/// Adding to this list exempts a result path from rule 3. It is the cheapest
/// wrong path available here, so it is not a step to take mid-task: if a new
/// variant seems to belong here, that is a decision for the maintainer.
const UNGUARDED: &[(&str, &str)] = &[
    (
        "Rebuild",
        "carries the context switch itself. It arrives to *advance* the \
         generation, so testing it against the current one would drop the \
         message that does the advancing.",
    ),
    (
        "ClientRefreshed",
        "carries `rebuild_epoch` and `apply_client_refresh` checks that \
         instead. A generation guard would be the wrong test: the whole point \
         of a client refresh is that the context has NOT changed.",
    ),
    (
        "UpdateCheck",
        "a background check for a newer ebman release. Nothing to do with an \
         AWS context, so no generation applies.",
    ),
];

// ---------------------------------------------------------------------------
// Behavioural: the enforcement point still enforces.
// ---------------------------------------------------------------------------

/// Both halves in one test on purpose. "Stale results are dropped" passes
/// vacuously if the handler never applies anything, so the fresh delivery has
/// to be shown working with the same message shape first.
#[test]
fn a_result_from_a_superseded_context_is_dropped() {
    let mut app = test_app();

    let gen_at_launch = app.generation;
    app.handle_msg(AppMsg::Applications {
        gen: gen_at_launch,
        result: Ok(vec![mk_application("delivered")]),
    });
    assert_eq!(
        app.applications
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>(),
        vec!["delivered"],
        "a result from the current context must be applied — otherwise the \
         stale half below proves nothing"
    );

    // The operator switches region / profile / account.
    app.generation += 1;

    app.handle_msg(AppMsg::Applications {
        gen: gen_at_launch,
        result: Ok(vec![mk_application("from-the-old-context")]),
    });
    assert_eq!(
        app.applications
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>(),
        vec!["delivered"],
        "ARCHITECTURE rule 3: a result launched at generation {gen_at_launch} \
         was applied after the context moved on. `handle_msg` must drop it."
    );
}

// ---------------------------------------------------------------------------
// Structural: the classification agrees with the payload.
// ---------------------------------------------------------------------------

/// Variant name -> does it carry a `gen: u64` field.
fn appmsg_variants() -> Vec<(String, bool)> {
    let src = std::fs::read_to_string("src/app.rs").expect("src/app.rs");
    let file = syn::parse_file(&src).expect("src/app.rs must parse");
    for item in &file.items {
        if let syn::Item::Enum(e) = item {
            if e.ident == "AppMsg" {
                return e
                    .variants
                    .iter()
                    .map(|v| {
                        let has_gen = matches!(&v.fields, syn::Fields::Named(f)
                        if f.named.iter().any(|f| {
                            f.ident.as_ref().is_some_and(|i| i == "gen")
                        }));
                        (v.ident.to_string(), has_gen)
                    })
                    .collect();
            }
        }
    }
    panic!("could not find `enum AppMsg` in src/app.rs");
}

/// Every variant name mentioned by a pattern, however it is spelled:
/// `Refresh { .. }`, `UpdateCheck(_)`, or an `|` chain of either.
fn variants_in_pattern(pat: &syn::Pat, out: &mut Vec<String>) {
    let name = |path: &syn::Path| {
        path.segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default()
    };
    match pat {
        syn::Pat::Or(or) => {
            for case in &or.cases {
                variants_in_pattern(case, out);
            }
        }
        syn::Pat::Struct(s) => out.push(name(&s.path)),
        syn::Pat::TupleStruct(t) => out.push(name(&t.path)),
        syn::Pat::Path(p) => out.push(name(&p.path)),
        syn::Pat::Paren(p) => variants_in_pattern(&p.pat, out),
        _ => {}
    }
}

#[derive(Default)]
struct GenerationFn {
    /// (variant, classified as Some)
    classified: Vec<(String, bool)>,
    found: bool,
}

impl<'ast> Visit<'ast> for GenerationFn {
    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        if f.sig.ident != "generation" {
            return;
        }
        self.found = true;
        struct Arms<'a>(&'a mut Vec<(String, bool)>);
        impl<'ast> Visit<'ast> for Arms<'_> {
            fn visit_expr_match(&mut self, m: &'ast syn::ExprMatch) {
                for arm in &m.arms {
                    // `None` is a bare path; anything else (`Some(*gen)`) is
                    // the guarded classification.
                    let is_none = matches!(&*arm.body, syn::Expr::Path(p)
                        if p.path.segments.last().is_some_and(|s| s.ident == "None"));
                    let mut names = Vec::new();
                    variants_in_pattern(&arm.pat, &mut names);
                    for n in names {
                        self.0.push((n, !is_none));
                    }
                }
                syn::visit::visit_expr_match(self, m);
            }
        }
        Arms(&mut self.classified).visit_block(&f.block);
    }
}

fn classification() -> Vec<(String, bool)> {
    let src = std::fs::read_to_string("src/app/msg.rs").expect("src/app/msg.rs");
    let file = syn::parse_file(&src).expect("src/app/msg.rs must parse");
    let mut v = GenerationFn::default();
    v.visit_file(&file);
    assert!(
        v.found,
        "`AppMsg::generation` has moved or been renamed — this guard is no \
         longer looking at the enforcement point"
    );
    v.classified
}

/// Carrying a `gen` and being subject to the generation check must agree.
/// This half needs no allowlist: a variant that carries a generation and is
/// classified `None` is exempt from the invariant it was built to satisfy.
#[test]
fn every_variant_carrying_a_generation_is_checked_against_it() {
    let variants = appmsg_variants();
    let classified = classification();

    // Non-vacuity: a walk that found a handful of variants would pass while
    // saying nothing.
    assert!(
        variants.len() >= 50,
        "found only {} AppMsg variants — the enum walk is broken",
        variants.len()
    );

    let mut problems = Vec::new();

    // A variant listed in both arms would be classified by whichever comes
    // first, so the lookup below could report it as fine while the `None` arm
    // also names it. `-D warnings` catches this as an unreachable pattern,
    // but only in CI and only if the arms are in that order.
    for (name, _) in &variants {
        let n = classified.iter().filter(|(c, _)| c == name).count();
        if n > 1 {
            problems.push(format!(
                "AppMsg::{name} is classified {n} times by `generation()` — one \
                 of them is unreachable and the other decides the \
                 behaviour."
            ));
        }
    }

    for (name, has_gen) in &variants {
        let Some((_, guarded)) = classified.iter().find(|(n, _)| n == name) else {
            problems.push(format!(
                "AppMsg::{name} is not classified by `generation()` at all"
            ));
            continue;
        };
        match (has_gen, guarded) {
            (true, false) => problems.push(format!(
                "AppMsg::{name} carries `gen: u64` but `generation()` returns \
                 None for it, so `handle_msg` will apply it after a context \
                 switch. That is ARCHITECTURE rule 3."
            )),
            (false, true) => problems.push(format!(
                "AppMsg::{name} is classified as carrying a generation but has \
                 no `gen` field"
            )),
            _ => {}
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// And the variants with no `gen` at all — the only ones the structural rule
/// above can't decide — each need a recorded reason.
#[test]
fn every_unguarded_variant_has_a_recorded_reason() {
    let unguarded: Vec<String> = appmsg_variants()
        .into_iter()
        .filter(|(_, has_gen)| !has_gen)
        .map(|(n, _)| n)
        .collect();

    let allowed: Vec<&str> = UNGUARDED.iter().map(|(n, _)| *n).collect();

    let missing: Vec<&String> = unguarded
        .iter()
        .filter(|n| !allowed.contains(&n.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "ARCHITECTURE rule 3: {missing:?} carry no `gen`, so they are exempt \
         from the generation check, and no reason is recorded for that. \
         Either give the variant a `gen: u64` and let `handle_msg` guard it, \
         or add an entry to UNGUARDED saying why it isn't context-bound — \
         which is a maintainer's call, not a step to take mid-task."
    );

    let stale: Vec<&&str> = allowed
        .iter()
        .filter(|n| !unguarded.contains(&n.to_string()))
        .collect();
    assert!(
        stale.is_empty(),
        "UNGUARDED names {stale:?}, which no longer exist or now carry a \
         `gen`. A recorded reason for a variant that isn't there is a lie the \
         next reader will believe — drop the entry."
    );
}
