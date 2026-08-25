//! `:help <topic>` — and the guard that keeps every topic reachable.
//!
//! `HelpTopic::Shell` was never constructed anywhere in production. The
//! screen existed, was written, and even had a render test — which set
//! `app.help.topic` directly, so it exercised a renderer down a path nothing
//! could take. In the embedded shell every keystroke belongs to the
//! subprocess, `?` included, so the screen explaining how to get back out was
//! the one screen you could not ask for.
//!
//! `:help <topic>` is how it's reached now. The guard below is the part that
//! matters longer term: a new topic that isn't in `HelpTopic::ALL` can't be
//! named, and would sit unreachable exactly as `Shell` did.

use super::support::test_app;
use crate::app::{HelpTopic, Mode};

#[test]
fn every_topic_round_trips_through_its_name() {
    for &topic in HelpTopic::ALL {
        assert_eq!(
            HelpTopic::from_arg(topic.arg_name()),
            Some(topic),
            "{topic:?} does not parse back from its own name"
        );
    }
}

#[test]
fn topic_names_are_unique() {
    let mut names: Vec<&str> = HelpTopic::ALL.iter().map(|t| t.arg_name()).collect();
    let before = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(
        before,
        names.len(),
        "two topics answer to the same name, so one of them is unreachable: \
         {names:?}"
    );
}

#[test]
fn topic_names_are_case_insensitive() {
    assert_eq!(HelpTopic::from_arg("SHELL"), Some(HelpTopic::Shell));
    assert_eq!(HelpTopic::from_arg("Shell"), Some(HelpTopic::Shell));
}

#[test]
fn an_unknown_topic_is_not_a_topic() {
    assert_eq!(HelpTopic::from_arg("shel"), None);
    assert_eq!(HelpTopic::from_arg(""), None);
}

#[test]
fn help_shell_opens_the_shell_topic() {
    let mut app = test_app();
    app.execute_command("help shell");
    assert_eq!(app.mode, Mode::Help);
    assert_eq!(app.help.topic, HelpTopic::Shell);
}

/// An unknown topic must not open help on whatever the inference would have
/// picked — silently showing the wrong screen for a typo'd name is worse
/// than saying so.
#[test]
fn an_unknown_topic_reports_instead_of_guessing() {
    let mut app = test_app();
    app.execute_command("help shel");
    assert_eq!(
        app.mode,
        Mode::Normal,
        "a typo'd topic opened the help screen anyway"
    );
    let msg = app.error_message.clone().unwrap_or_default();
    assert!(
        msg.contains("shel") && msg.contains("shell"),
        "the message should name what was typed and what is available: {msg:?}"
    );
    super::support::assert_no_run_on_spaces(&msg);
}

/// No-arg `:help` keeps inferring from context — the arg form is additive.
#[test]
fn bare_help_still_infers_the_topic() {
    let mut app = test_app();
    app.execute_command("help");
    assert_eq!(app.mode, Mode::Help);
    assert_eq!(app.help.topic, HelpTopic::Global);
}

/// The reachability guard. `ALL` is what `from_arg` searches, so a variant
/// missing from it cannot be named — which is precisely the state `Shell`
/// was in, and the compiler had nothing to say about it.
#[test]
fn every_help_topic_variant_is_in_all() {
    let src = std::fs::read_to_string("src/app/types.rs").expect("src/app/types.rs");
    let file = syn::parse_file(&src).expect("types.rs must parse");
    let variants: Vec<String> = file
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Enum(e) if e.ident == "HelpTopic" => {
                Some(e.variants.iter().map(|v| v.ident.to_string()).collect())
            }
            _ => None,
        })
        .expect("could not find `enum HelpTopic` in src/app/types.rs");

    assert!(
        variants.len() >= 5,
        "found only {} HelpTopic variants — the enum walk is broken",
        variants.len()
    );

    let reachable: Vec<String> = HelpTopic::ALL.iter().map(|t| format!("{t:?}")).collect();
    let missing: Vec<&String> = variants.iter().filter(|v| !reachable.contains(v)).collect();
    assert!(
        missing.is_empty(),
        "HelpTopic::{missing:?} are not in `HelpTopic::ALL`, so `:help <name>` \
         cannot reach them and they render only if something assigns the \
         variant directly. That is how `Shell` sat unreachable — add them to \
         ALL and give each an `arg_name`."
    );
}
