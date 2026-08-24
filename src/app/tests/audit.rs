//! The audit journal, its webhook, and the export paths.
//!
//! Split out of the 9,515-line `app/tests.rs`. Bodies moved
//! unchanged apart from one rewrite: `super::` meant `crate::app` in
//! the flat file and would mean `crate::app::tests` here, so every
//! explicit `super::` path was re-anchored (rustfmt reflowed some
//! lines as a result, since the new path is longer).

use super::super::*;
#[allow(unused_imports)]
use super::support::*;

#[test]
fn md_escape_protects_pipes_and_backslashes() {
    assert_eq!(md_escape("simple"), "simple");
    assert_eq!(md_escape("a|b|c"), "a\\|b\\|c");
    assert_eq!(md_escape("back\\slash"), "back\\\\slash");
    assert_eq!(md_escape("a\\|b"), "a\\\\\\|b");
}

#[test]
fn describe_env_dumps_known_fields() {
    let env = Environment {
        name: "my-env".into(),
        application: "my-app".into(),
        status: "Ready".into(),
        health: "Green".into(),
        platform: "Java 17".into(),
        solution_stack: String::new(),
        tier: "Web".into(),
        cname: "my-env.elb.amazonaws.com".into(),
        version_label: "v42".into(),
        arn: None,
        updated: None,
        id: None,
        region: None,
    };
    let text = describe_env(&env);
    assert!(text.contains("\"name\""));
    assert!(text.contains("my-env"));
    assert!(text.contains("\"updated\":         null"));
}

#[test]
fn the_clipboard_is_only_reached_through_yank() {
    // `yank` is stubbed under `cfg(test)` so the suite can't clobber the
    // clipboard of whoever runs it. That only holds while `yank` is the
    // sole door: `:update` reached `arboard` directly once, and every
    // test that ran it wrote to the real clipboard.
    //
    // The walk and the comment-stripping live in `super::scan`, which
    // has accuracy tests of its own. They used to be open-coded here,
    // and the stripper was `line.split("//").next()` — which truncates
    // inside a string literal, so a call placed after a URL literal on
    // the same line was invisible to this guard. Demonstrated, not
    // theorised.
    let sites = super::scan::find_in_production("arboard::");
    assert_eq!(
        sites.len(),
        1,
        "the only `arboard::` call belongs inside `yank`; found: {sites:?}"
    );
    assert!(sites[0].starts_with("src/app.rs:"), "{sites:?}");
}
