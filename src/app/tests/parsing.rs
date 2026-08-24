//! `parse_*` and friends — argument, URL and config parsing.
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
fn parse_sort_handles_directions() {
    assert_eq!(parse_sort(Some("app:desc")), (SortKey::App, true));
    assert_eq!(parse_sort(Some("name:asc")), (SortKey::Name, false));
    assert_eq!(parse_sort(Some("name")), (SortKey::Name, false));
    assert_eq!(parse_sort(Some("bogus:desc")), (SortKey::App, true)); // unknown key → default key, dir kept
    assert_eq!(parse_sort(None), (SortKey::App, false));
}

#[test]
fn parse_toggle_explicit_and_default() {
    assert!(parse_toggle(Some("on"), false));
    assert!(parse_toggle(Some("yes"), false));
    assert!(parse_toggle(Some("1"), false));
    assert!(!parse_toggle(Some("off"), true));
    assert!(!parse_toggle(Some("no"), true));
    // No arg → toggle current.
    assert!(parse_toggle(None, false));
    assert!(!parse_toggle(None, true));
    // Garbage → toggle current.
    assert!(parse_toggle(Some("maybe"), false));
}

#[test]
fn scope_next_alternates() {
    assert_eq!(Scope::Envs.next(), Scope::Apps);
    assert_eq!(Scope::Apps.next(), Scope::Envs);
}

#[test]
fn scope_prev_is_inverse_of_next() {
    assert_eq!(Scope::Envs.next(), Scope::Apps);
    assert_eq!(Scope::Envs.prev(), Scope::Apps);
    assert_eq!(Scope::Apps.next().next(), Scope::Apps);
    assert_eq!(Scope::Envs.prev().prev(), Scope::Envs);
}

#[test]
fn urlencode_keeps_safe_chars() {
    assert_eq!(urlencode("hello-world_1.0"), "hello-world_1.0");
    assert_eq!(urlencode("a b"), "a%20b");
    assert_eq!(urlencode("a/b?c=d"), "a%2Fb%3Fc%3Dd");
    // Unicode is byte-wise percent-encoded.
    assert!(urlencode("café").starts_with("caf"));
}

#[test]
fn json_escape_handles_quotes_and_controls() {
    assert_eq!(json_escape("hello"), "hello");
    assert_eq!(json_escape(r#"he said "hi""#), r#"he said \"hi\""#);
    assert_eq!(json_escape("line\nbreak"), "line\\nbreak");
    assert_eq!(json_escape("\\path"), "\\\\path");
    // Control character → \uXXXX.
    let out = json_escape("\u{0001}");
    assert_eq!(out, "\\u0001");
}

#[test]
fn parse_env_edit_body_round_trip() {
    let vars = vec![
        ("LOG_LEVEL".into(), "info".into()),
        (
            "DB_URL".into(),
            "postgres://user:pass@host:5432/db?sslmode=require".into(),
        ),
    ];
    let body = crate::app::build_env_edit_body("env", &vars);
    let parsed = crate::app::parse_env_edit_body(&body);
    assert_eq!(parsed.get("LOG_LEVEL").map(String::as_str), Some("info"));
    // Value containing `=` (postgres URL) passes through intact
    // because we split on the *first* `=` only.
    assert_eq!(
        parsed.get("DB_URL").map(String::as_str),
        Some("postgres://user:pass@host:5432/db?sslmode=require")
    );
}

#[test]
fn parse_env_edit_body_skips_comments_and_blanks() {
    let body = "# comment\n\nDB_HOST=localhost\n   # indented comment\n\nLOG=debug\n";
    let parsed = crate::app::parse_env_edit_body(body);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed.get("DB_HOST").map(String::as_str), Some("localhost"));
    assert_eq!(parsed.get("LOG").map(String::as_str), Some("debug"));
}

#[test]
fn parse_access_denied_handles_assumed_role() {
    let msg = "User: arn:aws:sts::123456789012:assumed-role/EbmanReadOnly/session-abc \
                   is not authorized to perform: elasticbeanstalk:RebuildEnvironment \
                   on resource: arn:aws:elasticbeanstalk:eu-west-2:123:environment/foo/bar";
    let parsed = crate::app::parse_access_denied(msg);
    assert_eq!(
        parsed,
        Some((
            "arn:aws:iam::123456789012:role/EbmanReadOnly".into(),
            "elasticbeanstalk:RebuildEnvironment".into()
        )),
        "assumed-role should be rewritten to the role ARN"
    );
}

#[test]
fn parse_access_denied_handles_iam_user() {
    let msg = "User: arn:aws:iam::123456789012:user/alice is not authorized to \
                   perform: s3:GetObject on resource: arn:aws:s3:::bucket/key";
    let parsed = crate::app::parse_access_denied(msg);
    assert_eq!(
        parsed,
        Some((
            "arn:aws:iam::123456789012:user/alice".into(),
            "s3:GetObject".into()
        )),
        "IAM-user ARN should pass through unchanged"
    );
}

#[test]
fn parse_access_denied_returns_none_on_unrelated_error() {
    assert_eq!(
        crate::app::parse_access_denied("ThrottlingException: rate exceeded"),
        None
    );
    assert_eq!(crate::app::parse_access_denied("random garbage text"), None);
}

#[test]
fn parse_s3_url_rejects_malformed() {
    assert!(crate::app::parse_s3_url("/local/path.zip").is_none());
    assert!(crate::app::parse_s3_url("s3://").is_none());
    assert!(crate::app::parse_s3_url("s3://bucket").is_none());
    assert!(crate::app::parse_s3_url("s3://bucket/").is_none());
    assert!(crate::app::parse_s3_url("s3:///key").is_none());
}

#[test]
fn parse_metric_extra_args_defaults_to_average() {
    let (stat, dims) = crate::app::parse_metric_extra_args(&[]);
    assert_eq!(stat, "Average");
    assert!(dims.is_empty());
}

#[test]
fn parse_metric_extra_args_picks_stat_first() {
    let (stat, dims) = crate::app::parse_metric_extra_args(&["Sum"]);
    assert_eq!(stat, "Sum");
    assert!(dims.is_empty());
}

#[test]
fn parse_metric_extra_args_picks_dims_when_present() {
    let (stat, dims) = crate::app::parse_metric_extra_args(&["InstanceId=i-abc"]);
    assert_eq!(stat, "Average");
    assert_eq!(dims, vec![("InstanceId".into(), "i-abc".into())]);
}

#[test]
fn parse_metric_extra_args_supports_both_in_any_order() {
    let (stat, dims) = crate::app::parse_metric_extra_args(&["Sum", "InstanceId=i-abc,Tier=web"]);
    assert_eq!(stat, "Sum");
    assert_eq!(
        dims,
        vec![
            ("InstanceId".into(), "i-abc".into()),
            ("Tier".into(), "web".into()),
        ]
    );
    // Reversed order: dims first.
    let (stat, dims) = crate::app::parse_metric_extra_args(&["InstanceId=i-abc", "Sum"]);
    assert_eq!(stat, "Sum");
    assert_eq!(dims, vec![("InstanceId".into(), "i-abc".into())]);
}

#[test]
fn expand_tilde_only_replaces_leading() {
    // No env mutation. This test used to `set_var("HOME")` under a
    // `// SAFETY: tests run single-threaded by default` comment, which
    // is false — `cargo test` is parallel by default, `profiles.rs`
    // says so in its own comment while racing this test for the same
    // variable, and several production paths read `HOME` live.
    let home = |h: &str| Some(std::ffi::OsString::from(h));
    assert_eq!(
        crate::app::expand_tilde_from(home("/Users/tester"), "~/foo/bar"),
        "/Users/tester/foo/bar"
    );
    // No leading tilde → unchanged.
    assert_eq!(
        crate::app::expand_tilde_from(home("/Users/tester"), "/abs/path"),
        "/abs/path"
    );
    // `~name` left alone (not supported).
    assert_eq!(
        crate::app::expand_tilde_from(home("/Users/tester"), "~tom/foo"),
        "~tom/foo"
    );
    // Mid-path tilde left alone.
    assert_eq!(
        crate::app::expand_tilde_from(home("/Users/tester"), "/foo/~/bar"),
        "/foo/~/bar"
    );
    // No HOME at all → the tilde stays, rather than expanding to "/".
    assert_eq!(
        crate::app::expand_tilde_from(None, "~/foo/bar"),
        "~/foo/bar"
    );
}

#[test]
fn parse_named_arg_picks_up_value_after_flag() {
    let rest: Vec<&str> = vec!["on", "--retention", "14"];
    assert_eq!(
        crate::app::parse_named_arg::<i32>(&rest, "--retention"),
        Some(14)
    );
    // Flag absent.
    assert_eq!(
        crate::app::parse_named_arg::<i32>(&["on"], "--retention"),
        None
    );
    // Flag present but no following value.
    assert_eq!(
        crate::app::parse_named_arg::<i32>(&["on", "--retention"], "--retention"),
        None
    );
    // Following value doesn't parse.
    assert_eq!(
        crate::app::parse_named_arg::<i32>(&["on", "--retention", "abc"], "--retention"),
        None
    );
}

#[test]
fn parse_tag_args_happy_path() {
    let v: Vec<&str> = vec!["Owner", "platform-team"];
    let (k, v) = crate::app::parse_tag_args(&v).unwrap();
    assert_eq!(k, "Owner");
    assert_eq!(v, "platform-team");
}

#[test]
fn parse_tag_args_joins_value_tokens_with_spaces() {
    let v: Vec<&str> = vec!["Description", "owned", "by", "platform"];
    let (k, v) = crate::app::parse_tag_args(&v).unwrap();
    assert_eq!(k, "Description");
    assert_eq!(v, "owned by platform");
}

#[test]
fn parse_tag_args_rejects_missing_value() {
    // Bare key with no value tokens.
    let v: Vec<&str> = vec!["Owner"];
    assert!(crate::app::parse_tag_args(&v).is_none());
    // Empty input.
    let v: Vec<&str> = vec![];
    assert!(crate::app::parse_tag_args(&v).is_none());
}

#[test]
fn event_time_format_parse_round_trips() {
    for f in [
        EventTimeFormat::Utc,
        EventTimeFormat::Local,
        EventTimeFormat::Age,
    ] {
        assert_eq!(EventTimeFormat::parse(f.label()), Some(f));
    }
    // Case-insensitive + the "relative" alias for age.
    assert_eq!(EventTimeFormat::parse("UTC"), Some(EventTimeFormat::Utc));
    assert_eq!(
        EventTimeFormat::parse("relative"),
        Some(EventTimeFormat::Age)
    );
    assert_eq!(EventTimeFormat::parse("nonsense"), None);
}

#[test]
fn shell_quote_passes_safe_chars_unchanged() {
    assert_eq!(shell_quote("safe-Name_1.0"), "safe-Name_1.0");
    assert_eq!(shell_quote("with space"), "'with space'");
    // Single quote escape uses POSIX trick: '\''
    assert_eq!(shell_quote("o'clock"), "'o'\\''clock'");
}

// --- partition-aware :explain and console links -----------------------

#[test]
fn parse_access_denied_rewrites_a_govcloud_session_arn() {
    // The rewrite matched the literal `arn:aws:sts::`, so in GovCloud,
    // China or an ISO partition the branch never fired and the raw
    // session ARN went to `iam:SimulatePrincipalPolicy`, which rejects
    // it — session credentials aren't a policy attachment point. The
    // endpoint fix got `:explain` to the right IAM endpoint; this is
    // what it failed on once it got there.
    let msg = "User: arn:aws-us-gov:sts::123456789012:assumed-role/EbAdmin/session \
               is not authorized to perform: elasticbeanstalk:UpdateEnvironment";
    let (principal, action) = crate::app::parse_access_denied(msg).expect("parsed");
    assert_eq!(principal, "arn:aws-us-gov:iam::123456789012:role/EbAdmin");
    assert_eq!(action, "elasticbeanstalk:UpdateEnvironment");
}

#[test]
fn parse_access_denied_leaves_a_plain_user_arn_alone() {
    let msg = "User: arn:aws:iam::1:user/alice is not authorized to perform: s3:GetObject";
    let (principal, _) = crate::app::parse_access_denied(msg).expect("parsed");
    assert_eq!(principal, "arn:aws:iam::1:user/alice");
}

#[test]
fn parse_access_denied_keeps_a_non_assumed_role_sts_principal() {
    // Making the rewrite partition-generic moved the `?` operators into
    // an arm that now fires for EVERY partition, so an STS ARN that
    // isn't an assumed-role — a federated user, say — propagates None
    // out of the whole function. Before, the branch simply didn't match
    // and the principal was returned unchanged.
    let msg = "User: arn:aws-us-gov:sts::123456789012:federated-user/ci-bot \
               is not authorized to perform: elasticbeanstalk:UpdateEnvironment";
    let (principal, action) = crate::app::parse_access_denied(msg)
        .expect("a federated-user denial must still parse, not vanish");
    assert_eq!(
        principal,
        "arn:aws-us-gov:sts::123456789012:federated-user/ci-bot"
    );
    assert_eq!(action, "elasticbeanstalk:UpdateEnvironment");
}

#[test]
fn json_surfaces_are_parsed_by_a_json_parser() {
    // Three JSON inputs used to go through `serde_yml` on the
    // reasoning that JSON is a YAML subset. True — but it means every
    // YAML feature applies to input ebman doesn't control: two LLM
    // response bodies carrying model-generated text, and a tfstate
    // file discovered by walking up from cwd. Anchor/alias expansion
    // is the specific hazard. `serde_json` was a direct dependency the
    // whole time, so the comment justifying the detour was stale too.
    //
    // Pinned by call site rather than by behaviour: the hazard is the
    // *parser choice*, and a test that fed YAML in would only prove
    // one of its features is absent.
    // Extended after the first version missed four more: the lint
    // baseline parser (whose own error message says "baseline JSON
    // parse failed"), and three round-trip tests asserting output is
    // valid JSON while reading it with a YAML parser — which accepts
    // things JSON rejects, so they asserted less than they appeared
    // to. A guard scoped to the files I happened to be editing is the
    // same mistake as a backlog entry nobody re-checks.
    for (name, src) in [
        ("llm.rs", include_str!("../../llm.rs")),
        ("terraform.rs", include_str!("../../terraform.rs")),
        ("lint/mod.rs", include_str!("../../lint/mod.rs")),
        ("lint/rules.rs", include_str!("../../lint/rules.rs")),
        ("audit.rs", include_str!("../../audit.rs")),
        ("cli/mod.rs", include_str!("../../cli/mod.rs")),
    ] {
        let code: String = src
            .lines()
            .map(super::scan::strip_line_comment)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !code.contains("serde_yml"),
            "{name} parses JSON with the YAML parser again"
        );
    }
    // `saved_config.rs` and `eb_cli.rs` are exempt and stay exempt:
    // EB saved configurations and `.elasticbeanstalk/config.yml`
    // really are YAML. They are also the ONLY two remaining
    // `serde_yml` consumers, which is what makes the RUSTSEC waiver
    // on it a two-file problem rather than a nine-file one.
    assert!(
        include_str!("../../saved_config.rs").contains("serde_yml"),
        "saved configs are genuinely YAML — if this flipped, check why"
    );
}
