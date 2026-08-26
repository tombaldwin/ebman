//! `format_*` / `wrap_*` / `redact*` — `&str`-in, `String`-out.
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
fn redact_block_preserves_length() {
    assert_eq!(redact_block(""), "");
    assert_eq!(redact_block("hello").chars().count(), 5);
    assert_eq!(redact_block("über-café").chars().count(), 9);
}

#[test]
fn diff_env_vars_produces_set_and_remove_lists() {
    let mut original = std::collections::BTreeMap::new();
    original.insert("KEEP".into(), "same".into());
    original.insert("CHANGE".into(), "old".into());
    original.insert("DROP".into(), "going".into());
    let mut edited = std::collections::BTreeMap::new();
    edited.insert("KEEP".into(), "same".into()); // unchanged
    edited.insert("CHANGE".into(), "new".into()); // updated
    edited.insert("NEW".into(), "added".into()); // added

    let (to_set, to_remove) = crate::app::diff_env_vars("ns", &original, &edited);
    // CHANGE + NEW should be in to_set; KEEP excluded (unchanged).
    let set_keys: std::collections::BTreeSet<&str> =
        to_set.iter().map(|(_, k, _)| k.as_str()).collect();
    assert_eq!(
        set_keys,
        ["CHANGE", "NEW"]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        "to_set should include changed + added keys"
    );
    assert!(
        !set_keys.contains("KEEP"),
        "unchanged key must not re-dispatch"
    );
    // DROP should be in to_remove.
    assert_eq!(to_remove.len(), 1);
    assert_eq!(to_remove[0].1, "DROP");
}

#[test]
fn diff_env_vars_empty_when_unchanged() {
    let mut original = std::collections::BTreeMap::new();
    original.insert("A".into(), "1".into());
    original.insert("B".into(), "2".into());
    let edited = original.clone();
    let (to_set, to_remove) = crate::app::diff_env_vars("ns", &original, &edited);
    assert!(to_set.is_empty());
    assert!(to_remove.is_empty());
}

#[test]
fn try_pretty_json_passes_through_non_json() {
    assert_eq!(
        crate::app::try_pretty_json("just a string"),
        "just a string"
    );
    assert_eq!(crate::app::try_pretty_json(""), "");
}

#[test]
fn try_pretty_json_indents_objects() {
    let pretty = crate::app::try_pretty_json(r#"{"a":1,"b":2}"#);
    let lines: Vec<&str> = pretty.lines().collect();
    assert!(lines.len() >= 4, "lines={lines:?}");
    assert!(lines.iter().any(|l| l.contains("\"a\": 1")));
    assert!(lines.iter().any(|l| l.contains("\"b\": 2")));
}

#[test]
fn try_pretty_json_emits_empty_containers_inline() {
    // Empty container must stay on one line, not split to `{\n}`.
    assert_eq!(crate::app::try_pretty_json("{}"), "{}");
    assert_eq!(crate::app::try_pretty_json("[]"), "[]");
    // Nested empty container — the outer object expands, the
    // inner `{}` stays inline beside its key.
    let pretty = crate::app::try_pretty_json(r#"{"a":{}}"#);
    assert!(pretty.contains("\"a\": {}"), "got: {pretty}");
}

#[test]
fn try_pretty_json_preserves_strings_with_braces() {
    // A `{` inside a string must not trigger indent.
    let pretty = crate::app::try_pretty_json(r#"{"msg":"hello {world}"}"#);
    assert!(pretty.contains("hello {world}"));
}

#[test]
fn format_age_buckets() {
    // Exact values at each boundary, not `ends_with`. Every `<` in this
    // ladder survived the mutation sweep as `<=`, because the old test
    // sampled the middle of each bucket (120s, 5h, 10d) where an
    // off-by-one at the edge is invisible.
    let now = chrono::Utc::now();
    let at = |d: chrono::Duration| crate::app::format_age(now, now - d);
    use chrono::Duration as D;

    for (d, want) in [
        (D::seconds(0), "0s ago"),
        (D::seconds(30), "30s ago"),
        (D::seconds(59), "59s ago"),
        (D::seconds(60), "1m ago"), // `secs < 60`
        (D::seconds(3599), "59m ago"),
        (D::seconds(3600), "1h ago"), // `mins < 60`
        (D::hours(47), "47h ago"),
        (D::hours(48), "2d ago"), // `hrs < 48`
        (D::days(59), "59d ago"),
        (D::days(60), "~2mo ago"), // `days < 60`
        (D::days(90), "~3mo ago"), // `days / 30`, not `days % 30`
        (D::days(719), "~23mo ago"),
        (D::days(720), "~1y ago"), // `months < 24`
    ] {
        assert_eq!(at(d), want, "format_age at {d}");
    }

    // A future timestamp clamps rather than going negative.
    assert_eq!(crate::app::format_age(now, now + D::hours(1)), "0s ago");
}

#[test]
fn humanize_short_age_buckets() {
    // Untested before the 2026-08-26 sweep: seven survivors, every
    // comparison in the ladder. Each bucket needs a value at its edge
    // AND one inside it — the edge kills `<=`, the interior kills the
    // `==` and `>` forms, which otherwise fall through to the next arm.
    use crate::app::humanize_short_age as h;
    use std::time::Duration;
    for (secs, want) in [
        (0, "0s"),
        (30, "30s"),
        (59, "59s"),
        (60, "1m"), // `secs < 60`
        (1800, "30m"),
        (3599, "59m"),
        (3600, "1h"), // `secs < 3600`
        (43_200, "12h"),
        (86_399, "23h"),
        (86_400, "1d"), // `secs < 86_400`
        (259_200, "3d"),
    ] {
        assert_eq!(h(Duration::from_secs(secs)), want, "{secs}s");
    }
}

#[test]
fn format_aws_error_routes_invalid_client_token_to_configure_hint() {
    let app = test_app();
    let out = app.format_aws_error(
        "refresh",
        "InvalidClientTokenId: The security token included in the request is invalid",
    );
    assert!(
        out.contains("credentials invalid"),
        "expected credentials-invalid hint, got: {out}"
    );
    assert!(
        out.contains("aws configure --profile"),
        "expected `aws configure` remediation, got: {out}"
    );
}

#[test]
fn format_aws_error_routes_signature_mismatch_to_configure_hint() {
    let app = test_app();
    let out = app.format_aws_error(
            "list_environments",
            "SignatureDoesNotMatch: The request signature we calculated does not match the signature you provided",
        );
    assert!(
        out.contains("credentials invalid"),
        "expected credentials-invalid hint, got: {out}"
    );
}

#[test]
fn format_aws_error_keeps_existing_expired_token_routing() {
    // The new invalid-creds arm must not steal traffic that the
    // existing ExpiredToken arm should keep handling. Belt-and-
    // braces test so a future re-ordering doesn't silently
    // regress the SSO refresh hint.
    let app = test_app();
    let out = app.format_aws_error(
        "refresh",
        "ExpiredToken: The security token included in the request is expired",
    );
    assert!(
        out.contains("credentials expired"),
        "expected expired-creds hint, got: {out}"
    );
    assert!(
        out.contains("aws sso login"),
        "expected `aws sso login` remediation, got: {out}"
    );
}

#[test]
fn wrap_with_hanging_indent_first_line_keeps_lead_marker() {
    let out = crate::app::wrap_with_hanging_indent(
        "Threshold Crossed: alarm details continue",
        30,
        "  ↳ ",
        "    ",
    );
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("  ↳ "));
    // Continuation line uses the cont prefix.
    if lines.len() > 1 {
        assert!(lines[1].starts_with("    "));
    }
}

#[test]
fn wrap_with_hanging_indent_hard_breaks_oversize_words() {
    // A single 50-char word at width 20 + 4-char lead → body width 16.
    let big_word = "x".repeat(50);
    let out = crate::app::wrap_with_hanging_indent(&big_word, 20, "    ", "    ");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 4, "50 chars in 16-wide chunks");
    assert_eq!(lines[0], "    ".to_string() + &"x".repeat(16));
    assert_eq!(lines[3], "    ".to_string() + &"x".repeat(2));
}

#[test]
fn wrap_with_hanging_indent_wraps_exactly_at_the_body_width() {
    // Distinct lead and cont so which line is which is visible.
    let w = |text: &str, width: usize| crate::app::wrap_with_hanging_indent(text, width, "A", "B");
    // width 10, 1-char lead → body width 9.
    //
    // "abcd efgh" is exactly 9 including the space, so it must stay on
    // one line: `candidate_len > body_width` is a strict `>`, and the
    // sweep found both `==` and `>=` survivable here.
    assert_eq!(w("abcd efgh", 10), "Aabcd efgh");
    // One char more and it wraps.
    assert_eq!(w("abcd efghi", 10), "Aabcd\nBefghi");
    // The candidate length is `current + 1 + word` — the joining space
    // counts. "abcd efg" is 8, still one line; the arithmetic mutants
    // (`-`, `*`) change where that tips.
    assert_eq!(w("abcd efg", 10), "Aabcd efg");
}

#[test]
fn wrap_with_hanging_indent_flushes_pending_text_before_a_hard_break() {
    // A short word followed by one too long to fit: the pending "ab"
    // must be emitted FIRST, on its own line, before the oversize word
    // is chunked. Deleting the `!` on that emptiness check survived —
    // it inverts the guard, so the pending text is held back and comes
    // out after the chunks, in the wrong order.
    let out = crate::app::wrap_with_hanging_indent(&format!("ab {}", "z".repeat(12)), 10, "A", "B");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "Aab", "the pending word leads");
    assert_eq!(lines[1], "B".to_string() + &"z".repeat(9));
    assert_eq!(lines[2], "B".to_string() + &"z".repeat(3));
}

#[test]
fn derive_version_label_uses_filename_stem_and_timestamp() {
    let l = crate::app::derive_version_label("./build.zip", 1684512345);
    assert_eq!(l, "build_1684512345");
    let l = crate::app::derive_version_label("/tmp/myapp-2.1.0.zip", 42);
    assert_eq!(l, "myapp-2.1.0_42");
}

#[test]
fn derive_version_label_sanitises_disallowed_chars() {
    // EB version labels don't allow spaces or weird punctuation; we
    // replace them with `_` so the operator gets a valid label even from
    // a goofy filename.
    let l = crate::app::derive_version_label("/tmp/build with spaces & specials!.zip", 1);
    assert_eq!(l, "build_with_spaces___specials__1");
}

#[test]
fn derive_version_label_falls_back_to_bundle_on_pathological_input() {
    // Bare `/` has no filename stem.
    let l = crate::app::derive_version_label("/", 9);
    assert_eq!(l, "bundle_9");
}

#[test]
fn format_env_vars_aligns_on_equals() {
    let vars = vec![
        ("DEBUG".into(), "1".into()),
        ("DATABASE_URL".into(), "postgres://x".into()),
    ];
    let out = crate::app::format_env_vars(&vars);
    assert!(out.contains("DEBUG"));
    assert!(out.contains("= 1"));
    assert!(out.contains("DATABASE_URL"));
    let vars = vec![("EMPTY".into(), "".into())];
    assert!(crate::app::format_env_vars(&vars).contains("\"\""));
}

#[test]
fn format_env_vars_handles_empty_input() {
    assert_eq!(crate::app::format_env_vars(&[]), "(no env vars set)");
}

#[test]
fn format_template_settings_groups_by_namespace() {
    let s = vec![
        (
            "aws:elasticbeanstalk:environment".into(),
            "EnvironmentType".into(),
            "LoadBalanced".into(),
        ),
        ("aws:autoscaling:asg".into(), "MinSize".into(), "2".into()),
        ("aws:autoscaling:asg".into(), "MaxSize".into(), "8".into()),
    ];
    let out = crate::app::format_template_settings(&s);
    assert!(out.contains("[aws:autoscaling:asg]"));
    assert!(out.contains("[aws:elasticbeanstalk:environment]"));
    assert!(out.contains("MinSize"));
    assert!(out.contains("= 2"));
    // Empty value renders as the literal "" so operators can tell empty
    // from unset.
    let s = vec![(
        "aws:elasticbeanstalk:application:environment".into(),
        "DEBUG".into(),
        String::new(),
    )];
    assert!(crate::app::format_template_settings(&s).contains("DEBUG"));
    assert!(crate::app::format_template_settings(&s).contains("\"\""));
}

#[test]
fn format_template_settings_handles_empty_input() {
    assert_eq!(
        crate::app::format_template_settings(&[]),
        "(no option settings)"
    );
}

#[test]
fn action_labels_are_distinct_and_non_empty() {
    // Catches accidental "placeholder Action::Rebuild" reuses — every
    // variant must carry its own label so audit logs + toasts reflect
    // what was actually dispatched.
    //
    // 0.19: extended to include `Action::Capacity` which had been
    // missing since 0.6 (caught by the 0.17.4 review pass). Now
    // exhaustive across all 15 variants — every variant gets an
    // explicit assertion so future additions can't skip the
    // distinctness check.
    use crate::app::Action;
    use std::collections::HashSet;
    let all = [
        Action::Rebuild,
        Action::RestartAppServer,
        Action::SwapCnames,
        Action::Terminate,
        Action::Deploy,
        Action::UpgradePlatform,
        Action::Clone,
        Action::Scale,
        Action::Capacity,
        Action::AbortUpdate,
        Action::ConfigSave,
        Action::ConfigDelete,
        Action::ConfigApply,
        Action::TerminateInstance,
        Action::SsmRun,
    ];
    let mut labels = HashSet::new();
    for a in all {
        let l = a.label();
        assert!(!l.is_empty(), "{a:?} has empty label");
        assert!(labels.insert(l), "{a:?} reuses label {l:?}");
    }
    // 15 = the full Action enum size. Update both the array
    // above and this guard if a new variant is added.
    assert_eq!(all.len(), 15);
}

#[test]
fn format_org_accounts_includes_switch_hint_when_configured() {
    use crate::aws::OrgAccount;
    let accounts = vec![
        OrgAccount {
            id: "111122223333".into(),
            name: "prod".into(),
            email: Some("prod@example.com".into()),
            status: "ACTIVE".into(),
        },
        OrgAccount {
            id: "444455556666".into(),
            name: "sandbox".into(),
            email: None,
            status: "SUSPENDED".into(),
        },
    ];
    let mut configured = std::collections::HashMap::new();
    configured.insert("prod".to_string(), "prod".to_string());
    let body = crate::app::format_org_accounts(&accounts, &configured);
    assert!(body.contains("● prod"));
    assert!(body.contains("⊘ sandbox"));
    assert!(body.contains("prod@example.com"));
    // Switch hint only for the configured account.
    assert!(body.contains(":account prod"));
    assert!(!body.contains(":account sandbox"));
}

#[test]
fn format_org_accounts_empty_returns_hint() {
    let body = crate::app::format_org_accounts(&[], &std::collections::HashMap::new());
    assert!(body.contains("no accounts returned"));
}

#[test]
fn format_org_accounts_matches_id_when_named_by_id() {
    use crate::aws::OrgAccount;
    let accounts = vec![OrgAccount {
        id: "111122223333".into(),
        name: "prod".into(),
        email: None,
        status: "ACTIVE".into(),
    }];
    // Operator named the AssumeRole entry by account-id rather
    // than friendly name — still matches.
    let mut configured = std::collections::HashMap::new();
    configured.insert("111122223333".to_string(), "111122223333".to_string());
    let body = crate::app::format_org_accounts(&accounts, &configured);
    assert!(body.contains(":account 111122223333"));
}

#[test]
fn build_lineage_collapses_consecutive_same_label_events() {
    // EB emits multiple events per deploy (started / instance OK /
    // env update completed). `build_lineage` must collapse them
    // into one row carrying the full first→last span. Newest-first
    // input → newest-first output.
    use chrono::TimeZone;
    let ts = |y, mo, d, h, mi| chrono::Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap();
    let mk = |t, vl: &str| crate::aws::Event {
        at: Some(t),
        env: "e".into(),
        application: "a".into(),
        message: "deploy event".into(),
        severity: "INFO".into(),
        version_label: Some(vl.into()),
    };
    // 3 events for build-9 (latest deploy) then 2 for build-8.
    let evs = vec![
        mk(ts(2026, 5, 24, 12, 7), "build-9"),
        mk(ts(2026, 5, 24, 12, 5), "build-9"),
        mk(ts(2026, 5, 24, 12, 0), "build-9"),
        mk(ts(2026, 5, 24, 11, 3), "build-8"),
        mk(ts(2026, 5, 24, 11, 0), "build-8"),
    ];
    let rows = crate::app::build_lineage(&evs);
    assert_eq!(rows.len(), 2, "expected 2 distinct deploys, got {rows:?}");
    // Newest first: build-9 then build-8.
    assert_eq!(rows[0].label, "build-9");
    assert_eq!(rows[1].label, "build-8");
    // first_at = earliest, last_at = latest within the group.
    assert_eq!(rows[0].first_at, Some(ts(2026, 5, 24, 12, 0)));
    assert_eq!(rows[0].last_at, Some(ts(2026, 5, 24, 12, 7)));
    assert_eq!(rows[1].first_at, Some(ts(2026, 5, 24, 11, 0)));
    assert_eq!(rows[1].last_at, Some(ts(2026, 5, 24, 11, 3)));
}

#[test]
fn build_lineage_drops_events_without_version_label() {
    // Events without a version_label (routine health transitions,
    // scaling notices) must not produce phantom rows.
    let ev = |vl: Option<&str>| crate::aws::Event {
        at: None,
        env: "e".into(),
        application: "a".into(),
        message: "noise".into(),
        severity: "INFO".into(),
        version_label: vl.map(String::from),
    };
    let evs = vec![ev(None), ev(Some("")), ev(None)];
    assert!(crate::app::build_lineage(&evs).is_empty());
}

#[test]
fn redact_for_log_preserves_length_with_block_chars() {
    assert_eq!(
        crate::app::redact_for_log("540847557034", true),
        "▓".repeat(12)
    );
    assert_eq!(
        crate::app::redact_for_log("540847557034", false),
        "540847557034"
    );
    // Em-dash placeholder + empty stay readable so the context line
    // doesn't render `▓` for "no account known yet".
    assert_eq!(crate::app::redact_for_log("—", true), "—");
    assert_eq!(crate::app::redact_for_log("", true), "");
}

#[test]
fn format_unavailability_line_distinguishes_zero_from_partial_from_full() {
    let (text, caution) = crate::app::format_unavailability_line("Immutable", 0, 4);
    assert!(text.contains("no in-service unavailability"));
    assert!(!caution);
    let (text, caution) = crate::app::format_unavailability_line("Rolling", 1, 4);
    assert!(text.contains("max 1/4 instance unavailable"));
    assert!(caution);
    let (text, caution) = crate::app::format_unavailability_line("AllAtOnce", 4, 4);
    assert!(text.contains("max 4/4 instances unavailable"));
    assert!(caution);
}

#[test]
fn format_ssm_results_renders_per_instance_sections() {
    // Two instances with different statuses → each gets its own
    // header (instance id + status + exit code) and stdout/stderr
    // sections. Empty-output instance shows the `(no output)` stub
    // so the operator can distinguish "ran cleanly, said nothing"
    // from "didn't run".
    let rows = vec![
        crate::aws::SsmRunResult {
            instance_id: "i-aaa".into(),
            status: "Success".into(),
            exit_code: 0,
            stdout: "hello world\nline two".into(),
            stderr: String::new(),
        },
        crate::aws::SsmRunResult {
            instance_id: "i-bbb".into(),
            status: "Failed".into(),
            exit_code: 2,
            stdout: String::new(),
            stderr: "permission denied".into(),
        },
    ];
    let body = crate::app::format_ssm_results("uptime", &rows);
    // Command line surfaced in header.
    assert!(body.contains("`uptime`"));
    // Both per-instance section headers present with exit codes.
    assert!(body.contains("i-aaa [Success, exit=0]"));
    assert!(body.contains("i-bbb [Failed, exit=2]"));
    // stdout content present.
    assert!(body.contains("hello world"));
    assert!(body.contains("line two"));
    // stderr content present.
    assert!(body.contains("permission denied"));
}

#[test]
fn format_ssm_results_truncates_long_output() {
    // A 100-line stdout blob must collapse to MAX_LINES_PER_STREAM
    // (50) + a "… (N more lines truncated)" footer so the overlay
    // stays scannable.
    let stdout: String = (0..100).map(|i| format!("line {i}\n")).collect();
    let rows = vec![crate::aws::SsmRunResult {
        instance_id: "i-aaa".into(),
        status: "Success".into(),
        exit_code: 0,
        stdout,
        stderr: String::new(),
    }];
    let body = crate::app::format_ssm_results("seq 0 99", &rows);
    // Truncation footer cites the number of dropped lines.
    assert!(
        body.contains("50 more lines truncated"),
        "expected truncation footer, got body:\n{body}"
    );
    // Last preserved line is `line 49`, not `line 99`.
    assert!(body.contains("line 49"));
    assert!(!body.contains("line 99"));
}

#[test]
fn format_ssm_results_empty_rows_produces_stub() {
    let body = crate::app::format_ssm_results("uptime", &[]);
    assert!(body.contains("No instances targeted"));
}
