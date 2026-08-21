//! Unit tests for the `aws` module.
//!
//! Split out of `src/aws.rs`. `use super::*` still resolves to
//! `crate::aws`, which glob-re-exports every per-service sub-module.
//!
//! One thing did change: this module is now a *sibling* of `aws::eb`,
//! `aws::cloudwatch` and the rest, not a child of the module that owns
//! their internals. A helper that is private to its sub-module is
//! unreachable from here. The ones tests need — `map_env`,
//! `compare_versions`, `to_smithy` — are `pub(super)`, which reaches
//! all of `aws`, including this module. Anything narrower needs its own
//! `#[cfg(test)]` block next to the code.

use super::*;

#[test]
fn platform_branch_from_arn_takes_full_branch_segment() {
    // The ARN's name segment itself contains " running " — the
    // solution-stack split must not fire first (it used to,
    // returning "on 64bit Amazon Linux 2023/4.0.1").
    assert_eq!(
            platform_branch_from(
                "arn:aws:elasticbeanstalk:us-east-1::platform/Python 3.9 running on 64bit Amazon Linux 2023/4.0.1"
            ),
            "Python 3.9 running on 64bit Amazon Linux 2023"
        );
}

#[test]
fn platform_branch_from_solution_stack_yields_family_prefix() {
    // Bare family — a prefix of the real PlatformBranchName
    // ("Python 3.9 running on …"), which is why the filter uses
    // begins_with rather than `=`.
    assert_eq!(
        platform_branch_from("64bit Amazon Linux 2023 v4.0.1 running Python 3.9"),
        "Python 3.9"
    );
    assert_eq!(platform_branch_from(""), "");
}

#[test]
fn platform_family_from_solution_stack() {
    assert_eq!(
        platform_family("64bit Amazon Linux 2 v3.5.0 running Java 17"),
        "Java 17"
    );
    assert_eq!(
        platform_family("64bit Amazon Linux 2 v3.7.0 running Tomcat 9 Corretto 17"),
        "Tomcat 9 Corretto 17"
    );
    assert_eq!(
        platform_family("64bit Amazon Linux 2023 v6.1.0 running Node.js 18"),
        "Node.js 18"
    );
}

#[test]
fn platform_family_from_arn() {
    assert_eq!(
            platform_family(
                "arn:aws:elasticbeanstalk:us-east-1::platform/Java 17 running on 64bit Amazon Linux 2/3.5.0"
            ),
            "Java 17"
        );
}

#[test]
fn platform_family_handles_empty_and_unknown() {
    assert_eq!(platform_family(""), "");
    assert_eq!(platform_family("just a string"), "just a string");
}

#[test]
fn stack_family_version_splits_solution_stack() {
    assert_eq!(
        stack_family_version("64bit Amazon Linux 2023 v6.1.0 running Node.js 18"),
        Some((
            "64bit Amazon Linux 2023 running Node.js 18".to_string(),
            "6.1.0".to_string()
        ))
    );
}

#[test]
fn stack_family_version_rejects_versionless() {
    assert_eq!(stack_family_version(""), None);
    assert_eq!(stack_family_version("some platform with no version"), None);
    // A leading-v word that isn't a dotted number must not be mistaken
    // for the version token.
    assert_eq!(stack_family_version("running via vN stack"), None);
}

#[test]
fn latest_stack_versions_keeps_newest_per_family() {
    let stacks = vec![
        "64bit Amazon Linux 2 v3.1.0 running Node.js 14".to_string(),
        "64bit Amazon Linux 2 v3.10.0 running Node.js 14".to_string(),
        "64bit Amazon Linux 2 v3.2.0 running Node.js 14".to_string(),
        "64bit Amazon Linux 2023 v6.1.0 running Node.js 18".to_string(),
    ];
    let latest = latest_stack_versions(&stacks);
    assert_eq!(
        latest.get("64bit Amazon Linux 2 running Node.js 14"),
        Some(&"3.10.0".to_string())
    );
    assert_eq!(
        latest.get("64bit Amazon Linux 2023 running Node.js 18"),
        Some(&"6.1.0".to_string())
    );
}

#[test]
fn newer_stack_version_flags_only_superseded() {
    let latest =
        latest_stack_versions(&["64bit Amazon Linux 2023 v6.1.0 running Node.js 18".to_string()]);
    // Older patch → flagged.
    assert_eq!(
        newer_stack_version("64bit Amazon Linux 2023 v6.0.3 running Node.js 18", &latest),
        Some("6.1.0".to_string())
    );
    // Already current → not flagged.
    assert_eq!(
        newer_stack_version("64bit Amazon Linux 2023 v6.1.0 running Node.js 18", &latest),
        None
    );
    // Different family (Node 18 vs 20) → not flagged.
    assert_eq!(
        newer_stack_version("64bit Amazon Linux 2023 v1.0.0 running Node.js 20", &latest),
        None
    );
    // No parseable stack → not flagged.
    assert_eq!(newer_stack_version("", &latest), None);
}

#[test]
fn normalize_tier_maps_known_names() {
    assert_eq!(normalize_tier("WebServer"), "Web");
    assert_eq!(normalize_tier("Worker"), "Worker");
    assert_eq!(normalize_tier("Other"), "Other");
}

#[test]
fn derive_dlq_url_appends_suffix() {
    assert_eq!(
        derive_dlq_url("https://sqs.us-east-1.amazonaws.com/123/awseb-e-foo-queue"),
        Some("https://sqs.us-east-1.amazonaws.com/123/awseb-e-foo-queue-dlq".to_string())
    );
}

#[test]
fn should_multipart_crosses_threshold() {
    assert!(!should_multipart(0, 64));
    assert!(!should_multipart(63, 64));
    assert!(should_multipart(64, 64));
    assert!(should_multipart(1_000_000, 64));
}

#[test]
fn plan_part_lengths_exact_multiple() {
    // 48 bytes, 16-byte parts → three full parts, no remainder.
    assert_eq!(plan_part_lengths(48, 16), vec![16, 16, 16]);
}

#[test]
fn plan_part_lengths_partial_last_part() {
    // 17 bytes, 8-byte parts → 8 + 8 + 1.
    assert_eq!(plan_part_lengths(17, 8), vec![8, 8, 1]);
}

#[test]
fn plan_part_lengths_zero_and_under_one_part() {
    // Zero input yields no parts (no upload to make).
    assert!(plan_part_lengths(0, 16).is_empty());
    // File smaller than one part is still one part.
    assert_eq!(plan_part_lengths(5, 16), vec![5]);
    // Defensive: zero part_size yields no plan (caller guard).
    assert!(plan_part_lengths(100, 0).is_empty());
}

#[test]
fn summarise_instance_health_rolls_up_buckets() {
    use aws_sdk_elasticbeanstalk::types::InstanceHealthSummary;

    // Mixed: 2 ok + 1 info = 3 healthy; total adds severity buckets.
    let s = InstanceHealthSummary::builder()
        .ok(2)
        .info(1)
        .warning(1)
        .degraded(0)
        .severe(1)
        .pending(0)
        .no_data(0)
        .unknown(0)
        .build();
    let counts = super::summarise_instance_health(Some(&s));
    assert_eq!(counts.healthy, 3, "ok + info");
    assert_eq!(counts.total, 5, "ok + info + warning + degraded + severe");

    // All-Grey buckets contribute to total but not to healthy.
    let s = InstanceHealthSummary::builder()
        .pending(2)
        .no_data(1)
        .build();
    let counts = super::summarise_instance_health(Some(&s));
    assert_eq!(counts.healthy, 0);
    assert_eq!(counts.total, 3);

    // None input → 0/0 default.
    let counts = super::summarise_instance_health(None);
    assert_eq!(counts.healthy, 0);
    assert_eq!(counts.total, 0);

    // All-empty summary → 0/0 (rare in practice but defensive).
    let s = InstanceHealthSummary::builder().build();
    let counts = super::summarise_instance_health(Some(&s));
    assert_eq!(counts.healthy, 0);
    assert_eq!(counts.total, 0);
}

#[test]
fn parse_window_ms_accepts_minutes_hours_days() {
    // Seconds — the unit every doc example (`--interval 60s`)
    // used but the parser rejected until the 0.26 max-review.
    assert_eq!(super::parse_window_ms("60s"), Some(60_000));
    assert_eq!(super::parse_window_ms("30m"), Some(30 * 60_000));
    assert_eq!(super::parse_window_ms("1h"), Some(60 * 60_000));
    assert_eq!(super::parse_window_ms("6h"), Some(6 * 60 * 60_000));
    assert_eq!(super::parse_window_ms("24h"), Some(24 * 60 * 60_000));
    assert_eq!(super::parse_window_ms("7d"), Some(7 * 24 * 60 * 60_000));
    // Whitespace-trimmed.
    assert_eq!(super::parse_window_ms("  2h  "), Some(2 * 60 * 60_000));
    // Case-insensitive on unit.
    assert_eq!(super::parse_window_ms("3H"), Some(3 * 60 * 60_000));
}

#[test]
fn parse_window_ms_rejects_malformed_input() {
    // Empty.
    assert_eq!(super::parse_window_ms(""), None);
    // Missing unit.
    assert_eq!(super::parse_window_ms("30"), None);
    // Missing number.
    assert_eq!(super::parse_window_ms("h"), None);
    // Unknown unit (y / w).
    assert_eq!(super::parse_window_ms("1y"), None);
    assert_eq!(super::parse_window_ms("2w"), None);
    // Non-positive — silently substituting 0 would surprise the operator.
    assert_eq!(super::parse_window_ms("0h"), None);
    assert_eq!(super::parse_window_ms("-1h"), None);
    // Garbage.
    assert_eq!(super::parse_window_ms("hour"), None);
    // Overflow / absurd windows reject rather than wrap (the
    // wrapped value panicked in debug and silently filtered
    // everything in release; chrono panics past ±262k years).
    assert_eq!(super::parse_window_ms("999999999999d"), None);
    assert_eq!(super::parse_window_ms("9999999999d"), None);
    assert_eq!(
        super::parse_window_ms("36500d"),
        Some(36_500 * 24 * 60 * 60_000)
    );
}

#[test]
fn format_insights_results_renders_table() {
    let results = InsightsResults {
        rows: vec![
            InsightsRow {
                fields: vec![
                    ("@timestamp".into(), "2026-05-23T10:00:00Z".into()),
                    ("@message".into(), "POST /checkout 200 42ms".into()),
                    ("@ptr".into(), "CWL_PTR_X".into()),
                ],
            },
            InsightsRow {
                fields: vec![
                    ("@timestamp".into(), "2026-05-23T10:00:01Z".into()),
                    ("@message".into(), "GET /healthcheck 200 1ms".into()),
                    ("@ptr".into(), "CWL_PTR_Y".into()),
                ],
            },
        ],
        records_scanned: 1234,
        records_matched: 2,
    };
    let body = super::format_insights_results(
        &results,
        "fields @timestamp, @message",
        &["/aws/elasticbeanstalk/prod/var/log/web.stdout.log".to_string()],
    );
    assert!(
        body.contains("matched: 2 / scanned: 1234"),
        "stats line present"
    );
    assert!(body.contains("@timestamp"), "@timestamp header present");
    assert!(body.contains("@message"), "@message header present");
    // @ptr is a record-locator field — always dropped from operator-facing output.
    assert!(
        !body.contains("@ptr"),
        "@ptr field should be filtered out of the rendered table"
    );
    assert!(body.contains("POST /checkout"), "first row body present");
    assert!(body.contains("GET /healthcheck"), "second row body present");
}

#[test]
fn format_insights_results_empty_input_shows_no_rows_stub() {
    let results = InsightsResults {
        rows: vec![],
        records_scanned: 1000,
        records_matched: 0,
    };
    let body = super::format_insights_results(
        &results,
        "fields @message | filter @message like /never/",
        &["/aws/elasticbeanstalk/prod/var/log/web.stdout.log".to_string()],
    );
    assert!(body.contains("no rows matched"), "empty-input stub fires");
    assert!(
        body.contains("matched: 0 / scanned: 1000"),
        "stats line still present"
    );
}

#[test]
fn format_insights_results_truncates_long_values() {
    // A 200-character message should get truncated to ≤ COL_MAX (60)
    // so the table doesn't dominate the overlay.
    let huge = "x".repeat(200);
    let results = InsightsResults {
        rows: vec![InsightsRow {
            fields: vec![("@message".into(), huge.clone())],
        }],
        records_scanned: 1,
        records_matched: 1,
    };
    let body = super::format_insights_results(&results, "fields @message", &[]);
    assert!(
        !body.contains(&huge),
        "raw 200-char value should not appear untouched"
    );
    assert!(
        body.contains("…"),
        "truncation marker should signal the cut to the operator"
    );
}

#[tokio::test]
async fn upload_bundle_uses_multipart_when_size_meets_threshold() {
    // Mocks the three multipart calls (CreateMultipartUpload →
    // UploadPart×N → CompleteMultipartUpload) and feeds upload_bundle
    // a 17-byte tempfile with an 8-byte part size + 1-byte threshold,
    // so we exercise three parts (8, 8, 1) without holding hundreds
    // of MiB in test memory.
    use aws_sdk_s3::operation::complete_multipart_upload::CompleteMultipartUploadOutput;
    use aws_sdk_s3::operation::create_multipart_upload::CreateMultipartUploadOutput;
    use aws_sdk_s3::operation::upload_part::UploadPartOutput;

    const BUCKET: &str = "elasticbeanstalk-eu-west-2-123";
    const KEY: &str = "applications/big-app/v1";
    const UPLOAD_ID: &str = "test-upload-id";

    let cmu_rule = mock!(aws_sdk_s3::Client::create_multipart_upload)
        .match_requests(|req| req.bucket() == Some(BUCKET) && req.key() == Some(KEY))
        .then_output(|| {
            CreateMultipartUploadOutput::builder()
                .upload_id(UPLOAD_ID)
                .build()
        });

    // One rule per UploadPart call — aws-smithy-mocks enforces
    // sequential rule matching by default, so a single rule reused
    // across N calls would only match the first call. We assert the
    // part number per rule to pin the order as well as the count.
    let up_rule_1 = mock!(aws_sdk_s3::Client::upload_part)
        .match_requests(|req| {
            req.bucket() == Some(BUCKET)
                && req.key() == Some(KEY)
                && req.upload_id() == Some(UPLOAD_ID)
                && req.part_number() == Some(1)
        })
        .then_output(|| UploadPartOutput::builder().e_tag("\"etag-1\"").build());
    let up_rule_2 = mock!(aws_sdk_s3::Client::upload_part)
        .match_requests(|req| {
            req.bucket() == Some(BUCKET)
                && req.key() == Some(KEY)
                && req.upload_id() == Some(UPLOAD_ID)
                && req.part_number() == Some(2)
        })
        .then_output(|| UploadPartOutput::builder().e_tag("\"etag-2\"").build());
    let up_rule_3 = mock!(aws_sdk_s3::Client::upload_part)
        .match_requests(|req| {
            req.bucket() == Some(BUCKET)
                && req.key() == Some(KEY)
                && req.upload_id() == Some(UPLOAD_ID)
                && req.part_number() == Some(3)
        })
        .then_output(|| UploadPartOutput::builder().e_tag("\"etag-3\"").build());

    let cmpu_rule = mock!(aws_sdk_s3::Client::complete_multipart_upload)
        .match_requests(|req| {
            req.bucket() == Some(BUCKET)
                && req.key() == Some(KEY)
                && req.upload_id() == Some(UPLOAD_ID)
                && req.multipart_upload().map(|m| m.parts().len()) == Some(3)
        })
        .then_output(|| CompleteMultipartUploadOutput::builder().build());

    let s3 = mock_client!(
        aws_sdk_s3,
        [&cmu_rule, &up_rule_1, &up_rule_2, &up_rule_3, &cmpu_rule]
    );
    let cfg = SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    let client = AwsClient::for_tests(
        Client::new(&cfg),
        SqsClient::new(&cfg),
        CwClient::new(&cfg),
        CwLogsClient::new(&cfg),
        s3,
        Ec2Client::new(&cfg),
    );

    let tmp = std::env::temp_dir().join(format!("ebman-test-multipart-{}.bin", std::process::id()));
    let bytes = vec![0xABu8; 17];
    std::fs::write(&tmp, &bytes).expect("write tempfile");
    let res = client.upload_bundle_with(BUCKET, KEY, &tmp, 1, 8).await;
    let _ = std::fs::remove_file(&tmp);
    res.expect("multipart upload should succeed");

    assert_eq!(cmu_rule.num_calls(), 1, "CreateMultipartUpload");
    assert_eq!(up_rule_1.num_calls(), 1, "UploadPart #1");
    assert_eq!(up_rule_2.num_calls(), 1, "UploadPart #2");
    assert_eq!(up_rule_3.num_calls(), 1, "UploadPart #3");
    assert_eq!(cmpu_rule.num_calls(), 1, "CompleteMultipartUpload");
}

#[tokio::test]
async fn upload_bundle_aborts_multipart_on_upload_part_failure() {
    // Pins the orphan-prevention invariant: when UploadPart fails
    // mid-flight, the upload loop must issue AbortMultipartUpload
    // before returning the error, otherwise S3 would accumulate
    // partial-upload storage charges that never roll up to a
    // CompleteMultipartUpload.
    use aws_sdk_s3::operation::create_multipart_upload::CreateMultipartUploadOutput;
    use aws_sdk_s3::operation::upload_part::{UploadPartError, UploadPartOutput};
    use aws_smithy_mocks::mock;

    const BUCKET: &str = "elasticbeanstalk-eu-west-2-123";
    const KEY: &str = "applications/abort-test/v1";
    const UPLOAD_ID: &str = "test-abort-upload-id";

    let cmu_rule = mock!(aws_sdk_s3::Client::create_multipart_upload).then_output(|| {
        CreateMultipartUploadOutput::builder()
            .upload_id(UPLOAD_ID)
            .build()
    });
    // First UploadPart succeeds, second fails. We test the worst
    // case where some parts have already landed in S3 — the abort
    // is what reclaims them.
    let up_ok = mock!(aws_sdk_s3::Client::upload_part)
        .match_requests(|req| req.part_number() == Some(1))
        .then_output(|| UploadPartOutput::builder().e_tag("\"etag-1\"").build());
    let up_fail = mock!(aws_sdk_s3::Client::upload_part)
        .match_requests(|req| req.part_number() == Some(2))
        .then_error(|| {
            UploadPartError::unhandled(aws_smithy_types::error::ErrorMetadata::builder().build())
        });
    let abort_rule = mock!(aws_sdk_s3::Client::abort_multipart_upload)
        .match_requests(|req| {
            req.bucket() == Some(BUCKET)
                && req.key() == Some(KEY)
                && req.upload_id() == Some(UPLOAD_ID)
        })
        .then_output(|| {
            aws_sdk_s3::operation::abort_multipart_upload::AbortMultipartUploadOutput::builder()
                .build()
        });

    let s3 = mock_client!(aws_sdk_s3, [&cmu_rule, &up_ok, &up_fail, &abort_rule]);
    let cfg = SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    let client = AwsClient::for_tests(
        Client::new(&cfg),
        SqsClient::new(&cfg),
        CwClient::new(&cfg),
        CwLogsClient::new(&cfg),
        s3,
        Ec2Client::new(&cfg),
    );

    let tmp = std::env::temp_dir().join(format!("ebman-test-abort-{}.bin", std::process::id()));
    std::fs::write(&tmp, vec![0xCDu8; 16]).expect("write tempfile");
    // threshold=1 forces multipart; part_size=8 → 2 parts (8 + 8).
    let res = client.upload_bundle_with(BUCKET, KEY, &tmp, 1, 8).await;
    let _ = std::fs::remove_file(&tmp);
    assert!(res.is_err(), "upload should surface UploadPart failure");
    assert_eq!(abort_rule.num_calls(), 1, "AbortMultipartUpload must fire");
}

#[test]
fn derive_dlq_url_skips_already_dlq() {
    assert_eq!(
        derive_dlq_url("https://sqs.us-east-1.amazonaws.com/123/foo-dlq"),
        None
    );
}

#[test]
fn derive_dlq_url_strips_trailing_slash() {
    assert_eq!(
        derive_dlq_url("https://sqs.us-east-1.amazonaws.com/123/foo/"),
        Some("https://sqs.us-east-1.amazonaws.com/123/foo-dlq".to_string())
    );
}

// ─── Mocked-AWS integration tests ─────────────────────────────────────
//
// These exercise the SDK code paths against `aws-smithy-mocks` so we
// can lock down past regressions and run without an AWS account. Each
// test names the specific bug it pins to keep the intent crisp when
// a future change "breaks" it.

use aws_smithy_mocks::{mock, mock_client};

/// Build a minimal `AwsClient` where only one sub-client is mocked and
/// the rest are plain SDK defaults (which will fail loudly if any
/// unmocked code path is reached — exactly the signal we want).
fn client_with_eb(eb: Client) -> AwsClient {
    let cfg = aws_config::SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    AwsClient::for_tests(
        eb,
        SqsClient::new(&cfg),
        CwClient::new(&cfg),
        CwLogsClient::new(&cfg),
        S3Client::new(&cfg),
        Ec2Client::new(&cfg),
    )
}

fn client_with_cw_logs(cw_logs: CwLogsClient) -> AwsClient {
    let cfg = aws_config::SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    AwsClient::for_tests(
        Client::new(&cfg),
        SqsClient::new(&cfg),
        CwClient::new(&cfg),
        cw_logs,
        S3Client::new(&cfg),
        Ec2Client::new(&cfg),
    )
}

fn client_with_cw(cw: CwClient) -> AwsClient {
    let cfg = aws_config::SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    AwsClient::for_tests(
        Client::new(&cfg),
        SqsClient::new(&cfg),
        cw,
        CwLogsClient::new(&cfg),
        S3Client::new(&cfg),
        Ec2Client::new(&cfg),
    )
}

/// SSM isn't an arg to `for_tests` (only EB / SQS / CW / CW Logs
/// / S3 / EC2 are — to keep that signature manageable across the
/// existing 11 call sites). Tests that need a mocked SSM client
/// override the field on the constructed AwsClient — the field
/// is `pub(crate)` for exactly this.
fn client_with_ssm(ssm: aws_sdk_ssm::Client) -> AwsClient {
    let cfg = aws_config::SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    let mut c = AwsClient::for_tests(
        Client::new(&cfg),
        SqsClient::new(&cfg),
        CwClient::new(&cfg),
        CwLogsClient::new(&cfg),
        S3Client::new(&cfg),
        Ec2Client::new(&cfg),
    );
    c.ssm = ssm;
    c
}

fn client_with_eb_and_s3(eb: Client, s3: S3Client) -> AwsClient {
    let cfg = aws_config::SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    AwsClient::for_tests(
        eb,
        SqsClient::new(&cfg),
        CwClient::new(&cfg),
        CwLogsClient::new(&cfg),
        s3,
        Ec2Client::new(&cfg),
    )
}

fn client_with_sqs(sqs: SqsClient) -> AwsClient {
    let cfg = aws_config::SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    AwsClient::for_tests(
        Client::new(&cfg),
        sqs,
        CwClient::new(&cfg),
        CwLogsClient::new(&cfg),
        S3Client::new(&cfg),
        Ec2Client::new(&cfg),
    )
}

/// Build a base `AwsClient` then swap in a mocked sub-client for one of
/// the secondary services (ACM / Organizations / Cost Explorer /
/// Secrets Manager). Field access works because the `tests` module
/// is a child of `aws`, so the private sub-client fields are visible.
/// The macro saves repeating six SDK-config lines per test.
macro_rules! client_with_sub {
    ($field:ident = $value:expr) => {{
        let cfg = aws_config::SdkConfig::builder()
            .region(Region::new("us-east-1"))
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build();
        let mut c = AwsClient::for_tests(
            Client::new(&cfg),
            SqsClient::new(&cfg),
            CwClient::new(&cfg),
            CwLogsClient::new(&cfg),
            S3Client::new(&cfg),
            Ec2Client::new(&cfg),
        );
        c.$field = $value;
        c
    }};
}

#[tokio::test]
async fn list_secrets_maps_secretlistentry_to_summary() {
    // Pins the happy path through ListSecrets — field-by-field
    // mapping from `SecretListEntry` to `SecretSummary`, sort by
    // last_changed desc, and the optional `name_filter` substring
    // match. Caught here because `:secrets` is the operator's
    // entry into Secrets Manager and a silent field-rename in the
    // SDK output would break the picker without a compile error.
    use aws_sdk_secretsmanager::operation::list_secrets::ListSecretsOutput;
    use aws_sdk_secretsmanager::types::SecretListEntry;
    use aws_smithy_types::DateTime as SmithyDt;

    let rule = mock!(aws_sdk_secretsmanager::Client::list_secrets).then_output(|| {
        ListSecretsOutput::builder()
            .secret_list(
                SecretListEntry::builder()
                    .name("prod/db-password")
                    .arn("arn:aws:secretsmanager:us-east-1:123:secret:prod/db-password-AbCdEf")
                    .description("Production DB master password")
                    .last_changed_date(SmithyDt::from_secs(1_700_000_000))
                    .build(),
            )
            .secret_list(
                SecretListEntry::builder()
                    .name("staging/api-key")
                    .arn("arn:aws:secretsmanager:us-east-1:123:secret:staging/api-key-XyZ")
                    // Older timestamp than prod/db-password — should
                    // sort below in the result.
                    .last_changed_date(SmithyDt::from_secs(1_600_000_000))
                    .build(),
            )
            .build()
    });
    let secrets = mock_client!(aws_sdk_secretsmanager, [&rule]);
    let client = client_with_sub!(secrets = secrets);

    let all = client.list_secrets(None).await.expect("ok");
    assert_eq!(all.len(), 2);
    // Newest first.
    assert_eq!(all[0].name, "prod/db-password");
    assert_eq!(all[1].name, "staging/api-key");
    // Description + last_changed survived the round-trip.
    assert_eq!(
        all[0].description.as_deref(),
        Some("Production DB master password")
    );
    assert!(all[0].last_changed.is_some());
}

#[tokio::test]
async fn list_certificates_filters_to_issued_and_extracts_domain() {
    // Pins two contract points: (1) ListCertificates is called with
    // `CertificateStatus::Issued` so revoked / pending / expired
    // certs don't show up in the `:listener-edit` picker, and (2)
    // the response's `domain_name` lands in `AcmCert.domain`.
    use aws_sdk_acm::operation::list_certificates::ListCertificatesOutput;
    use aws_sdk_acm::types::{CertificateStatus, CertificateSummary};

    let rule = mock!(aws_sdk_acm::Client::list_certificates)
        .match_requests(|req| {
            req.certificate_statuses()
                .contains(&CertificateStatus::Issued)
        })
        .then_output(|| {
            ListCertificatesOutput::builder()
                .certificate_summary_list(
                    CertificateSummary::builder()
                        .certificate_arn("arn:aws:acm:us-east-1:123:certificate/abcd")
                        .domain_name("*.example.com")
                        .build(),
                )
                .certificate_summary_list(
                    CertificateSummary::builder()
                        .certificate_arn("arn:aws:acm:us-east-1:123:certificate/efgh")
                        .domain_name("api.example.com")
                        .build(),
                )
                .build()
        });
    let acm = mock_client!(aws_sdk_acm, [&rule]);
    let client = client_with_sub!(acm = acm);

    let certs = client.list_certificates().await.expect("ok");
    assert_eq!(certs.len(), 2);
    // Sorted by domain.
    assert_eq!(certs[0].domain, "*.example.com");
    assert_eq!(certs[1].domain, "api.example.com");
    assert_eq!(rule.num_calls(), 1, "ListCertificates fired once");
}

#[tokio::test]
async fn list_org_accounts_sorts_active_first_then_by_name() {
    // The overlay puts ACTIVE accounts at the top so the operator
    // sees switchable accounts before suspended/closed ones. Pins
    // both the field mapping and the sort.
    use aws_sdk_organizations::operation::list_accounts::ListAccountsOutput;
    use aws_sdk_organizations::types::{Account, AccountStatus};

    let rule = mock!(aws_sdk_organizations::Client::list_accounts).then_output(|| {
        ListAccountsOutput::builder()
            .accounts(
                Account::builder()
                    .id("999999999999")
                    .name("zzz-closed")
                    .email("zzz@example.com")
                    .status(AccountStatus::Suspended)
                    .build(),
            )
            .accounts(
                Account::builder()
                    .id("222222222222")
                    .name("staging")
                    .email("staging@example.com")
                    .status(AccountStatus::Active)
                    .build(),
            )
            .accounts(
                Account::builder()
                    .id("111111111111")
                    .name("prod")
                    .email("prod@example.com")
                    .status(AccountStatus::Active)
                    .build(),
            )
            .build()
    });
    let org = mock_client!(aws_sdk_organizations, [&rule]);
    let client = client_with_sub!(org = org);

    let accounts = client.list_org_accounts().await.expect("ok");
    assert_eq!(accounts.len(), 3);
    // ACTIVE first, sorted by name within status.
    assert_eq!(accounts[0].name, "prod");
    assert_eq!(accounts[1].name, "staging");
    assert_eq!(accounts[2].name, "zzz-closed");
}

#[tokio::test]
async fn fetch_env_costs_extracts_env_name_from_tag_group_key() {
    // Cost Explorer encodes the tag group key as
    // `elasticbeanstalk:environment-name$<value>`; the prefix split
    // and the f64 amount parse are the load-bearing bits to pin.
    // Also asserts the metric / granularity / group-by shape on the
    // request, since silently switching from Monthly to Daily would
    // wreck the cache assumptions in cost_cache.rs.
    use aws_sdk_costexplorer::operation::get_cost_and_usage::GetCostAndUsageOutput;
    use aws_sdk_costexplorer::types::{Granularity, Group, MetricValue, ResultByTime};

    let rule = mock!(aws_sdk_costexplorer::Client::get_cost_and_usage)
        .match_requests(|req| {
            req.granularity() == Some(&Granularity::Monthly)
                && req.metrics().iter().any(|m| m == "UnblendedCost")
                && req
                    .group_by()
                    .iter()
                    .any(|g| g.key() == Some("elasticbeanstalk:environment-name"))
        })
        .then_output(|| {
            let mut metrics = std::collections::HashMap::new();
            metrics.insert(
                "UnblendedCost".to_string(),
                MetricValue::builder().amount("150.25").unit("USD").build(),
            );
            GetCostAndUsageOutput::builder()
                .results_by_time(
                    ResultByTime::builder()
                        .groups(
                            Group::builder()
                                .keys("elasticbeanstalk:environment-name$uflexi-prod")
                                .set_metrics(Some(metrics))
                                .build(),
                        )
                        .build(),
                )
                .build()
        });
    let cost = mock_client!(aws_sdk_costexplorer, [&rule]);
    let client = client_with_sub!(cost = cost);

    let costs = client.fetch_env_costs().await.expect("ok");
    assert_eq!(costs.rows.len(), 1);
    assert_eq!(costs.rows[0].env_name, "uflexi-prod");
    assert!(!costs.truncated, "a single complete page is not truncated");
    assert!(
        (costs.rows[0].cost_usd - 150.25).abs() < f64::EPSILON,
        "amount parsed from string"
    );
}

// ── Regression #1 ────────────────────────────────────────────────────
// `DescribeConfigurationSettings` returns `WorkerQueueURL = ""` when
// EB autocreates the queue (the operator didn't override it). The
// original code looked only at option settings and would show "no
// queue" for the most common worker-tier shape. The fix queries
// `DescribeEnvironmentResources` first and only falls back to option
// settings when explicit overrides exist.

#[tokio::test]
async fn log_tail_skips_already_delivered_boundary_ids() {
    // After a page-capped poll the watermark stays AT the boundary
    // millisecond and its events are re-fetched — the carried id
    // set must filter them so the overlay shows no duplicates.
    use aws_sdk_cloudwatchlogs::operation::filter_log_events::FilterLogEventsOutput;
    use aws_sdk_cloudwatchlogs::types::FilteredLogEvent;

    let page = aws_smithy_mocks::mock!(CwLogsClient::filter_log_events).then_output(|| {
        FilterLogEventsOutput::builder()
            .events(
                FilteredLogEvent::builder()
                    .timestamp(1_000)
                    .event_id("e1")
                    .log_stream_name("i-abc")
                    .message("already delivered")
                    .build(),
            )
            .events(
                FilteredLogEvent::builder()
                    .timestamp(1_000)
                    .event_id("e2")
                    .log_stream_name("i-abc")
                    .message("new at boundary")
                    .build(),
            )
            .build()
    });
    let cw_logs = aws_smithy_mocks::mock_client!(aws_sdk_cloudwatchlogs, [&page]);
    let client = client_with_cw_logs(cw_logs);
    let skip: std::collections::HashSet<String> = ["e1".to_string()].into_iter().collect();
    let (events, next_since, _carry) = client
        .fetch_recent_log_events("/aws/eb/env", 1_000, 1000, &skip)
        .await
        .expect("ok");
    let msgs: Vec<&str> = events.iter().map(|e| e.message.as_str()).collect();
    assert_eq!(msgs, vec!["new at boundary"], "e1 filtered, e2 delivered");
    assert_eq!(next_since, 1_000, "no newer event — watermark holds");
}

#[tokio::test]
async fn worker_queues_primary_error_with_empty_fallback_is_an_error() {
    // 0.27 re-review C-class: AccessDenied on the primary
    // discovery call + an Ok-but-empty fallback (the COMMON
    // autocreated-queue case — sqsd option settings are empty)
    // used to read as Ok("no queues") and silently clear DLQ
    // alerting. It must surface as Err.
    use aws_sdk_elasticbeanstalk::operation::describe_configuration_settings::DescribeConfigurationSettingsOutput;
    use aws_sdk_elasticbeanstalk::operation::describe_environment_resources::DescribeEnvironmentResourcesError;

    let der = mock!(Client::describe_environment_resources).then_error(|| {
        DescribeEnvironmentResourcesError::generic(
            aws_smithy_types::error::ErrorMetadata::builder()
                .code("AccessDenied")
                .message("not authorized")
                .build(),
        )
    });
    let dcs = mock!(Client::describe_configuration_settings)
        .then_output(|| DescribeConfigurationSettingsOutput::builder().build());
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&der, &dcs]);
    let client = client_with_eb(eb);
    let result = client.describe_worker_queues("app", "wk-env").await;
    assert!(
        result.is_err(),
        "primary error + empty fallback must be Err, got {result:?}"
    );
    assert_eq!(der.num_calls(), 1);
    assert_eq!(dcs.num_calls(), 1);
}

#[tokio::test]
async fn worker_queues_resolves_via_describe_environment_resources_when_autocreated() {
    use aws_sdk_elasticbeanstalk::operation::describe_environment_resources::DescribeEnvironmentResourcesOutput;
    use aws_sdk_elasticbeanstalk::types::{EnvironmentResourceDescription, Queue};

    let der = mock!(Client::describe_environment_resources).then_output(|| {
        DescribeEnvironmentResourcesOutput::builder()
            .environment_resources(
                EnvironmentResourceDescription::builder()
                    .queues(
                        Queue::builder()
                            .name("WorkerQueue")
                            .url("https://sqs.us-east-1.amazonaws.com/123/awseb-e-foo-queue")
                            .build(),
                    )
                    .queues(
                        Queue::builder()
                            .name("WorkerDeadLetterQueue")
                            .url("https://sqs.us-east-1.amazonaws.com/123/awseb-e-foo-queue-dlq")
                            .build(),
                    )
                    .build(),
            )
            .build()
    });
    // Provide an empty configuration-settings response — that's the
    // exact failure mode the bug fix is defending against.
    let dcs = mock!(Client::describe_configuration_settings).then_output(|| {
            aws_sdk_elasticbeanstalk::operation::describe_configuration_settings::DescribeConfigurationSettingsOutput::builder()
                .build()
        });
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&der, &dcs]);
    let client = client_with_eb(eb);

    // We can't actually fetch SQS stats without mocking SQS too, but
    // the URL resolution is the bit that regressed — assert by
    // calling the option-settings-only path that drives the same
    // logic without the stats round-trip.
    // describe_worker_queues calls queue_stats which would fail
    // against the default sqs client. Use a try-await dance to
    // observe at least the call shape via the mock's call counter.
    let _ = client.describe_worker_queues("eb-app", "eb-env").await;
    assert_eq!(
        der.num_calls(),
        1,
        "describe_environment_resources should be the primary path"
    );
}

// ── Regression #2 ────────────────────────────────────────────────────
// `peek_messages` originally made a single `ReceiveMessage` call —
// but SQS may return fewer than the requested batch on any one call
// (it's a maximum, not a guarantee). The fix loops with short long-
// polling, dedupes by message id across iterations, and bails after
// two empty batches in a row.

#[tokio::test]
async fn peek_messages_loops_and_dedupes_across_batches() {
    use aws_sdk_sqs::operation::receive_message::ReceiveMessageOutput;
    use aws_sdk_sqs::types::Message;

    // First call returns 2 messages, second call returns 1 (including
    // a duplicate of msg-1), third returns empty, fourth returns
    // empty → loop should exit. Expect 3 unique messages.
    fn msg(id: &'static str) -> Message {
        Message::builder().message_id(id).body(id).build()
    }
    let rule = mock!(aws_sdk_sqs::Client::receive_message)
        .sequence()
        .output(|| {
            ReceiveMessageOutput::builder()
                .messages(msg("msg-1"))
                .messages(msg("msg-2"))
                .build()
        })
        .output(|| {
            ReceiveMessageOutput::builder()
                .messages(msg("msg-1")) // dup
                .messages(msg("msg-3"))
                .build()
        })
        .output(|| ReceiveMessageOutput::builder().build())
        .output(|| ReceiveMessageOutput::builder().build())
        .build();
    let sqs = mock_client!(aws_sdk_sqs, [&rule]);
    let client = client_with_sqs(sqs);

    let out = client
        .peek_messages("https://sqs.us-east-1.amazonaws.com/123/q", 10)
        .await
        .expect("peek should succeed");
    let ids: Vec<String> = out.iter().map(|m| m.id.clone()).collect();
    assert_eq!(ids, vec!["msg-1", "msg-2", "msg-3"]);
}

#[tokio::test]
async fn peek_messages_stops_after_two_empty_batches() {
    use aws_sdk_sqs::operation::receive_message::ReceiveMessageOutput;
    // Sequence returns empty twice — should stop without exhausting
    // the call cap.
    let rule = mock!(aws_sdk_sqs::Client::receive_message)
        .sequence()
        .output(|| ReceiveMessageOutput::builder().build())
        .output(|| ReceiveMessageOutput::builder().build())
        // If we reach this, the stop-on-two-empty guard is broken.
        .output(|| {
            ReceiveMessageOutput::builder()
                .messages(
                    aws_sdk_sqs::types::Message::builder()
                        .message_id("late")
                        .body("late")
                        .build(),
                )
                .build()
        })
        .build();
    let sqs = mock_client!(aws_sdk_sqs, [&rule]);
    let client = client_with_sqs(sqs);

    let out = client
        .peek_messages("https://sqs.us-east-1.amazonaws.com/123/q", 10)
        .await
        .expect("peek should succeed");
    assert!(
        out.is_empty(),
        "should have stopped before consuming the 'late' message"
    );
    assert_eq!(
        rule.num_calls(),
        2,
        "exactly two empty-batch calls should terminate the loop"
    );
}

// ── Happy-path coverage ──────────────────────────────────────────────
// Lock down the most-used path so refactors of `list_environments`
// don't silently break the table-rendering surface.

#[tokio::test]
async fn list_environments_maps_describe_environments_to_env_rows() {
    use aws_sdk_elasticbeanstalk::operation::describe_environments::DescribeEnvironmentsOutput;
    use aws_sdk_elasticbeanstalk::types::{EnvironmentDescription, EnvironmentTier};

    let de = mock!(Client::describe_environments).then_output(|| {
        DescribeEnvironmentsOutput::builder()
            .environments(
                EnvironmentDescription::builder()
                    .environment_name("api-prod")
                    .application_name("api")
                    .status("Ready".into())
                    .health("Green".into())
                    .cname("api-prod.eba.amazonaws.com")
                    .version_label("build-42")
                    .solution_stack_name("64bit Amazon Linux 2 v3.5.0 running Java 17")
                    .tier(EnvironmentTier::builder().name("WebServer").build())
                    .build(),
            )
            .build()
    });
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&de]);
    let client = client_with_eb(eb);

    let envs = client.list_environments().await.expect("ok");
    assert_eq!(envs.len(), 1);
    let e = &envs[0];
    assert_eq!(e.name, "api-prod");
    assert_eq!(e.application, "api");
    assert_eq!(e.tier, "Web", "tier normalises WebServer → Web");
    assert_eq!(e.platform, "Java 17");
    assert_eq!(e.version_label, "build-42");
}

#[tokio::test]
async fn list_application_versions_pages_through_next_token() {
    // Pins the pagination invariant: orgs with hundreds of historical
    // versions per app must see every entry in `:versions` and let
    // `:rollback` find labels that fall past the first page. Two
    // pages of mocked responses; the second carries no next_token so
    // the loop terminates. Both pages' entries must appear in the
    // returned Vec.
    use aws_sdk_elasticbeanstalk::operation::describe_application_versions::DescribeApplicationVersionsOutput;
    use aws_sdk_elasticbeanstalk::types::ApplicationVersionDescription;

    let page1 = mock!(Client::describe_application_versions)
        .match_requests(|req| {
            req.application_name() == Some("uflexi") && req.next_token().is_none()
        })
        .then_output(|| {
            DescribeApplicationVersionsOutput::builder()
                .application_versions(
                    ApplicationVersionDescription::builder()
                        .version_label("build-101")
                        .description("first")
                        .build(),
                )
                .application_versions(
                    ApplicationVersionDescription::builder()
                        .version_label("build-100")
                        .description("zeroth")
                        .build(),
                )
                .next_token("PAGE_2")
                .build()
        });
    let page2 = mock!(Client::describe_application_versions)
        .match_requests(|req| req.next_token() == Some("PAGE_2"))
        .then_output(|| {
            DescribeApplicationVersionsOutput::builder()
                .application_versions(
                    ApplicationVersionDescription::builder()
                        .version_label("build-099")
                        .description("rolled")
                        .build(),
                )
                .build()
        });
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&page1, &page2]);
    let client = client_with_eb(eb);

    let versions = client
        .list_application_versions("uflexi")
        .await
        .expect("ok");
    let labels: Vec<&str> = versions.iter().map(|v| v.label.as_str()).collect();
    assert_eq!(
        labels,
        vec!["build-101", "build-100", "build-099"],
        "all three versions from both pages should be returned",
    );
    assert_eq!(page1.num_calls(), 1, "first page fetched once");
    assert_eq!(page2.num_calls(), 1, "second page fetched once");
}

#[tokio::test]
async fn log_tail_fetch_follows_next_token_without_skipping_events() {
    // 0.27 fix: a truncated FilterLogEvents page used to advance
    // the watermark past events it never received — silent line
    // drops during traffic spikes.
    use aws_sdk_cloudwatchlogs::operation::filter_log_events::FilterLogEventsOutput;
    use aws_sdk_cloudwatchlogs::types::FilteredLogEvent;

    let mk = |ts: i64, msg: &str| {
        FilteredLogEvent::builder()
            .timestamp(ts)
            .log_stream_name("i-abc")
            .message(msg)
            .build()
    };
    let page1 = aws_smithy_mocks::mock!(CwLogsClient::filter_log_events)
        .match_requests(|req| req.next_token().is_none())
        .then_output(move || {
            FilterLogEventsOutput::builder()
                .events(mk(1_000, "a"))
                .events(mk(1_005, "b"))
                .next_token("PAGE_2")
                .build()
        });
    let page2 = aws_smithy_mocks::mock!(CwLogsClient::filter_log_events)
        .match_requests(|req| req.next_token() == Some("PAGE_2"))
        .then_output(move || {
            FilterLogEventsOutput::builder()
                .events(mk(1_005, "c"))
                .events(mk(1_010, "d"))
                .build()
        });
    let cw_logs = aws_smithy_mocks::mock_client!(aws_sdk_cloudwatchlogs, [&page1, &page2]);
    let client = client_with_cw_logs(cw_logs);

    let (events, next_since, carry) = client
        .fetch_recent_log_events("/aws/eb/env", 500, 1000, &Default::default())
        .await
        .expect("ok");
    assert!(
        carry.is_empty(),
        "clean (non-truncated) poll carries no boundary ids"
    );
    let msgs: Vec<&str> = events.iter().map(|e| e.message.as_str()).collect();
    assert_eq!(
        msgs,
        vec!["a", "b", "c", "d"],
        "both pages' events delivered — none skipped"
    );
    assert_eq!(
        next_since, 1_011,
        "watermark advances past the newest RECEIVED event"
    );
    assert_eq!(page1.num_calls(), 1);
    assert_eq!(page2.num_calls(), 1);
}

// ── MultiSelect picker plumbing ─────────────────────────────────────
//
// `:subnets` / `:security-groups` rely on three helpers that all
// need to round-trip cleanly: VPC discovery via option settings,
// EC2 inventory listing filtered by VPC, and the comma-split
// helper that converts EB's CSV format to a clean Vec<String>.

#[test]
fn split_csv_trims_and_drops_empties() {
    assert_eq!(
        split_csv("subnet-a,subnet-b, subnet-c, ,subnet-d"),
        vec!["subnet-a", "subnet-b", "subnet-c", "subnet-d"]
    );
    assert!(split_csv("").is_empty());
    assert!(split_csv(",,,").is_empty());
}

#[tokio::test]
async fn fetch_env_vpc_context_pulls_vpc_id_subnets_and_sgs() {
    use aws_sdk_elasticbeanstalk::operation::describe_configuration_settings::DescribeConfigurationSettingsOutput;
    use aws_sdk_elasticbeanstalk::types::{
        ConfigurationOptionSetting, ConfigurationSettingsDescription,
    };

    let dcs = mock!(Client::describe_configuration_settings).then_output(|| {
        DescribeConfigurationSettingsOutput::builder()
            .configuration_settings(
                ConfigurationSettingsDescription::builder()
                    .option_settings(
                        ConfigurationOptionSetting::builder()
                            .namespace("aws:ec2:vpc")
                            .option_name("VPCId")
                            .value("vpc-123")
                            .build(),
                    )
                    .option_settings(
                        ConfigurationOptionSetting::builder()
                            .namespace("aws:ec2:vpc")
                            .option_name("Subnets")
                            .value("subnet-a,subnet-b")
                            .build(),
                    )
                    .option_settings(
                        ConfigurationOptionSetting::builder()
                            .namespace("aws:ec2:vpc")
                            .option_name("ELBSubnets")
                            .value("subnet-x,subnet-y")
                            .build(),
                    )
                    .option_settings(
                        ConfigurationOptionSetting::builder()
                            .namespace("aws:autoscaling:launchconfiguration")
                            .option_name("SecurityGroups")
                            .value("sg-1,sg-2,sg-3")
                            .build(),
                    )
                    // Noise — should be ignored.
                    .option_settings(
                        ConfigurationOptionSetting::builder()
                            .namespace("aws:elasticbeanstalk:application:environment")
                            .option_name("LOG_LEVEL")
                            .value("debug")
                            .build(),
                    )
                    .build(),
            )
            .build()
    });
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&dcs]);
    let client = client_with_eb(eb);

    let ctx = client
        .fetch_env_vpc_context("api", "api-prod")
        .await
        .expect("ok");
    assert_eq!(ctx.vpc_id.as_deref(), Some("vpc-123"));
    assert_eq!(ctx.subnets, vec!["subnet-a", "subnet-b"]);
    assert_eq!(ctx.elb_subnets, vec!["subnet-x", "subnet-y"]);
    assert_eq!(ctx.security_groups, vec!["sg-1", "sg-2", "sg-3"]);
}

#[tokio::test]
async fn list_subnets_in_vpc_filters_orders_and_extracts_name_tag() {
    use aws_sdk_ec2::operation::describe_subnets::DescribeSubnetsOutput;
    use aws_sdk_ec2::types::{Subnet, Tag};

    let ds = mock!(aws_sdk_ec2::Client::describe_subnets).then_output(|| {
        DescribeSubnetsOutput::builder()
            .subnets(
                Subnet::builder()
                    .subnet_id("subnet-2b")
                    .availability_zone("us-east-1b")
                    .cidr_block("10.0.2.0/24")
                    .tags(Tag::builder().key("Name").value("private-2b").build())
                    .build(),
            )
            .subnets(
                Subnet::builder()
                    .subnet_id("subnet-1a")
                    .availability_zone("us-east-1a")
                    .cidr_block("10.0.1.0/24")
                    .build(),
            )
            .subnets(
                Subnet::builder()
                    .subnet_id("subnet-1a-overlap")
                    .availability_zone("us-east-1a")
                    .cidr_block("10.0.0.0/24")
                    .build(),
            )
            .build()
    });
    let ec2 = mock_client!(aws_sdk_ec2, [&ds]);
    let cfg = aws_config::SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    let client = AwsClient::for_tests(
        Client::new(&cfg),
        SqsClient::new(&cfg),
        CwClient::new(&cfg),
        CwLogsClient::new(&cfg),
        S3Client::new(&cfg),
        ec2,
    );

    let subnets = client.list_subnets_in_vpc("vpc-abc").await.expect("ok");
    // Ordered by AZ then CIDR — subnet-1a-overlap (10.0.0.0/24) precedes
    // subnet-1a (10.0.1.0/24), then subnet-2b.
    let ids: Vec<&str> = subnets.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["subnet-1a-overlap", "subnet-1a", "subnet-2b"]);
    // Name tag extracted when present, None when absent.
    assert_eq!(subnets[2].name_tag.as_deref(), Some("private-2b"));
    assert!(subnets[1].name_tag.is_none());
}

// ── Write-path coverage ──────────────────────────────────────────────
//
// `update_env_option_settings` is the load-bearing write path —
// every `:capacity`, `:env`, `:tag`, `:subnets`, `:set-option`, etc.
// ultimately funnels through it. Pin the request-shape contract and
// the empty-input guard.

#[tokio::test]
async fn update_env_option_settings_builds_correct_request_shape() {
    use aws_sdk_elasticbeanstalk::operation::update_environment::UpdateEnvironmentOutput;
    // `match_requests` runs the closure against every captured
    // request; returning false means "no rule matched" and the
    // SDK call returns an error, which the test would then trip on.
    // So an assertion-style predicate doubles as the test body.
    let rule = mock!(Client::update_environment)
        .match_requests(|input| {
            if input.environment_name.as_deref() != Some("api-prod") {
                return false;
            }
            let options = input.option_settings();
            if options.len() != 2 {
                return false;
            }
            // Order is preserved from the caller's slice.
            if options[0].namespace.as_deref() != Some("aws:autoscaling:asg")
                || options[0].option_name.as_deref() != Some("MinSize")
                || options[0].value.as_deref() != Some("2")
            {
                return false;
            }
            if options[1].namespace.as_deref() != Some("aws:autoscaling:launchconfiguration")
                || options[1].option_name.as_deref() != Some("InstanceType")
                || options[1].value.as_deref() != Some("t3.medium")
            {
                return false;
            }
            let removes = input.options_to_remove();
            if removes.len() != 1 {
                return false;
            }
            removes[0].namespace.as_deref() == Some("aws:elasticbeanstalk:application:environment")
                && removes[0].option_name.as_deref() == Some("OLD_VAR")
        })
        .then_output(|| UpdateEnvironmentOutput::builder().build());
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&rule]);
    let client = client_with_eb(eb);

    let to_set = vec![
        (
            "aws:autoscaling:asg".to_string(),
            "MinSize".to_string(),
            "2".to_string(),
        ),
        (
            "aws:autoscaling:launchconfiguration".to_string(),
            "InstanceType".to_string(),
            "t3.medium".to_string(),
        ),
    ];
    let to_remove = vec![(
        "aws:elasticbeanstalk:application:environment".to_string(),
        "OLD_VAR".to_string(),
    )];
    client
        .update_env_option_settings("api-prod", &to_set, &to_remove)
        .await
        .expect("expected request shape to match");
    assert_eq!(rule.num_calls(), 1);
}

#[tokio::test]
async fn update_env_option_settings_rejects_empty_input_before_dispatch() {
    // If the guard fails we'd reach the mocked client, which has no
    // rules — that would also error, but with a different message.
    // The empty-input branch must short-circuit *before* any SDK call.
    use aws_sdk_elasticbeanstalk::operation::update_environment::UpdateEnvironmentOutput;
    let trip = mock!(Client::update_environment)
        .then_output(|| UpdateEnvironmentOutput::builder().build());
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&trip]);
    let client = client_with_eb(eb);

    let err = client
        .update_env_option_settings("api-prod", &[], &[])
        .await
        .expect_err("expected guard to fire");
    assert!(
        err.to_string().contains("nothing to do"),
        "expected nothing-to-do guard, got {err}"
    );
    assert_eq!(
        trip.num_calls(),
        0,
        "guard should short-circuit before any SDK call"
    );
}

#[tokio::test]
async fn update_env_option_settings_surfaces_aws_errors() {
    use aws_sdk_elasticbeanstalk::operation::update_environment::UpdateEnvironmentError;
    use aws_sdk_elasticbeanstalk::types::error::InsufficientPrivilegesException;
    let err_rule = mock!(Client::update_environment).then_error(|| {
        UpdateEnvironmentError::InsufficientPrivilegesException(
            InsufficientPrivilegesException::builder()
                .message("not authorized to call UpdateEnvironment")
                .build(),
        )
    });
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&err_rule]);
    let client = client_with_eb(eb);

    let err = client
        .update_env_option_settings(
            "api-prod",
            &[("aws:autoscaling:asg".into(), "MinSize".into(), "2".into())],
            &[],
        )
        .await
        .expect_err("expected AWS error to propagate");
    // The flatten wraps the SDK error string; we just confirm the
    // contextual prefix is present so logs are actionable.
    assert!(
        err.to_string()
            .contains("UpdateEnvironment(option_settings)"),
        "expected wrapped error context, got {err}"
    );
}

#[tokio::test]
async fn list_security_groups_in_vpc_orders_by_name() {
    use aws_sdk_ec2::operation::describe_security_groups::DescribeSecurityGroupsOutput;
    use aws_sdk_ec2::types::SecurityGroup;

    let dsg = mock!(aws_sdk_ec2::Client::describe_security_groups).then_output(|| {
        DescribeSecurityGroupsOutput::builder()
            .security_groups(
                SecurityGroup::builder()
                    .group_id("sg-z")
                    .group_name("zeta")
                    .description("z group")
                    .build(),
            )
            .security_groups(
                SecurityGroup::builder()
                    .group_id("sg-a")
                    .group_name("alpha")
                    .description("a group")
                    .build(),
            )
            .build()
    });
    let ec2 = mock_client!(aws_sdk_ec2, [&dsg]);
    let cfg = aws_config::SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    let client = AwsClient::for_tests(
        Client::new(&cfg),
        SqsClient::new(&cfg),
        CwClient::new(&cfg),
        CwLogsClient::new(&cfg),
        S3Client::new(&cfg),
        ec2,
    );

    let sgs = client
        .list_security_groups_in_vpc("vpc-abc")
        .await
        .expect("ok");
    assert_eq!(sgs.len(), 2);
    assert_eq!(sgs[0].group_name, "alpha");
    assert_eq!(sgs[1].group_name, "zeta");
}

// ── Error-path coverage for the load-bearing read methods ────────────
//
// Each of these mocks the SDK to return a typed error and asserts
// our wrapper preserves the operation-name context. Future
// refactors of these methods will trip a test if they accidentally
// drop the `.map_err(|e| eyre!(...))?` prefix and start propagating
// bare SDK errors.

#[tokio::test]
async fn list_environments_throttling_error_is_recognised_by_predicate() {
    // End-to-end contract: when EB returns a Throttling-coded SDK error
    // on DescribeEnvironments, the flattened error string we surface
    // must trip `is_throttling_error` so the refresh loop installs a
    // back-off horizon instead of treating it like a normal failure.
    // Pinning this guards against an SDK / smithy change to the
    // stringification format silently breaking back-off.
    use aws_sdk_elasticbeanstalk::operation::describe_environments::DescribeEnvironmentsError;
    let rule = mock!(Client::describe_environments).then_error(|| {
        DescribeEnvironmentsError::generic(
            aws_smithy_types::error::ErrorMetadata::builder()
                .code("ThrottlingException")
                .message("Rate exceeded")
                .build(),
        )
    });
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&rule]);
    let client = client_with_eb(eb);

    let err = client
        .list_environments()
        .await
        .expect_err("expected throttling error to propagate");
    // Production path: aws error → eyre::Report → flatten_err_to_string
    // (peeks at the Debug form for SDK throttling tokens) → user-facing
    // string → is_throttling_error. Pinning this contract means a future
    // change to either end can't silently break refresh back-off.
    let s = crate::app::flatten_err_to_string(&err);
    assert!(
        crate::app::is_throttling_error(&s),
        "is_throttling_error should fire on the flattened SDK throttling string, got {s:?}"
    );
    // And the user-facing string stays readable — no Debug noise leaks.
    assert!(
        !s.contains("StatusCode") && !s.contains("Extensions"),
        "throttling toast should be clean, got {s:?}"
    );
}

#[tokio::test]
async fn list_environments_expired_token_surfaces_clean_user_message() {
    // When credentials expire mid-session, the SDK returns an
    // `ExpiredToken`-coded error. The toast should not leak Debug
    // noise (HTTP status, headers, body bytes) and must not be
    // misclassified as throttling. Pinning this guards against an
    // SDK stringification change silently turning the toast into a
    // wall of debug output or routing expired-token through the
    // throttle back-off path.
    use aws_sdk_elasticbeanstalk::operation::describe_environments::DescribeEnvironmentsError;
    let rule = mock!(Client::describe_environments).then_error(|| {
        DescribeEnvironmentsError::generic(
            aws_smithy_types::error::ErrorMetadata::builder()
                .code("ExpiredTokenException")
                .message("The security token included in the request is expired")
                .build(),
        )
    });
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&rule]);
    let client = client_with_eb(eb);

    let err = client
        .list_environments()
        .await
        .expect_err("expected expired-token error to propagate");
    let s = crate::app::flatten_err_to_string(&err);
    assert!(
        !crate::app::is_throttling_error(&s),
        "ExpiredToken should not fire the throttling predicate, got {s:?}"
    );
    assert!(
        !s.contains("StatusCode") && !s.contains("Extensions") && !s.contains("SdkBody"),
        "expired-token toast should be clean, got {s:?}"
    );
}

#[tokio::test]
async fn fetch_env_metrics_batches_and_reorders_by_canonical_id() {
    // CloudWatch `GetMetricData` accepts N queries in one round-trip;
    // `fetch_env_metrics` always dispatches 4 (health / req4xx /
    // req5xx / p90). The response can arrive in any order — the
    // caller re-keys by `id` and returns the canonical order so the
    // Metrics-tab renderer doesn't drift when AWS reorders results
    // (which it has been known to do).
    //
    // Pinning: (a) one batched call, (b) all 4 ids requested, (c)
    // returned series are in canonical order even when the mock
    // shuffles them, (d) labels are mapped per-id.
    use aws_sdk_cloudwatch::operation::get_metric_data::GetMetricDataOutput;
    use aws_sdk_cloudwatch::types::MetricDataResult;
    use aws_smithy_types::DateTime as SdkDateTime;

    let ts = SdkDateTime::from_secs(1_700_000_000);
    let mk_result = move |id: &str, value: f64| {
        MetricDataResult::builder()
            .id(id)
            .timestamps(ts)
            .values(value)
            .build()
    };
    // Return in shuffled order so the test verifies reordering.
    let rule = mock!(aws_sdk_cloudwatch::Client::get_metric_data)
        .match_requests(|req| {
            let ids: Vec<&str> = req
                .metric_data_queries()
                .iter()
                .filter_map(|q| q.id())
                .collect();
            ids == ["health", "req4xx", "req5xx", "p90"]
        })
        .then_output(move || {
            GetMetricDataOutput::builder()
                .metric_data_results(mk_result("req5xx", 12.0))
                .metric_data_results(mk_result("health", 25.0))
                .metric_data_results(mk_result("p90", 0.42))
                .metric_data_results(mk_result("req4xx", 3.0))
                .build()
        });
    let cw = mock_client!(aws_sdk_cloudwatch, [&rule]);
    let client = client_with_cw(cw);

    let series = client
        .fetch_env_metrics("uflexi-prod", 900)
        .await
        .expect("metric fetch should succeed");

    // Single batched call covered all 4 metrics — the function
    // doesn't fan out 4 separate GetMetricData round-trips.
    assert_eq!(rule.num_calls(), 1, "expected exactly one batched call");
    // Canonical order is preserved regardless of response order.
    let ids: Vec<&str> = series.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["health", "req4xx", "req5xx", "p90"]);
    // Per-id label mapping holds — operator-facing labels not raw ids.
    let by_id: std::collections::HashMap<&str, &str> = series
        .iter()
        .map(|s| (s.id.as_str(), s.label.as_str()))
        .collect();
    assert_eq!(by_id["health"], "Env Health (0–25)");
    assert_eq!(by_id["req4xx"], "4xx Requests / min");
    assert_eq!(by_id["req5xx"], "5xx Requests / min");
    assert_eq!(by_id["p90"], "Latency P90");
    // Timestamp/value zipping survived the shuffle.
    let p90 = series.iter().find(|s| s.id == "p90").unwrap();
    assert_eq!(p90.points.len(), 1);
    assert!((p90.points[0].1 - 0.42).abs() < f64::EPSILON);
}

#[tokio::test]
async fn deploy_from_path_chain_dispatches_each_stage() {
    // End-to-end pinning of the multi-stage `:deploy --from PATH` flow:
    //   1. CreateStorageLocation (EB) → returns the managed bucket name
    //   2. PutObject (S3)             → uploads the bundle bytes
    //   3. CreateApplicationVersion   → registers the version
    //   4. UpdateEnvironment          → deploys to the env
    // Each mock asserts the input it receives matches what the previous
    // stage produced, so a future refactor that reorders / drops a stage
    // or rewires the bucket+key threading fails loud here. This is the
    // most multi-step pure-AWS code path in the project and has no other
    // automated coverage today.
    use aws_sdk_elasticbeanstalk::operation::create_application_version::CreateApplicationVersionOutput;
    use aws_sdk_elasticbeanstalk::operation::create_storage_location::CreateStorageLocationOutput;
    use aws_sdk_elasticbeanstalk::operation::update_environment::UpdateEnvironmentOutput;
    use aws_sdk_s3::operation::put_object::PutObjectOutput;

    const BUCKET: &str = "elasticbeanstalk-us-east-1-123456789012";
    const APP: &str = "uflexi-webapp";
    const ENV: &str = "uflexi-prod";
    const LABEL: &str = "build-2026-05-20-1234567890";
    const KEY: &str = "applications/uflexi-webapp/build-2026-05-20-1234567890";
    let bundle_bytes: Vec<u8> = b"PK\x03\x04 ... a real zip would start here".to_vec();

    let csl_rule = mock!(Client::create_storage_location).then_output(|| {
        CreateStorageLocationOutput::builder()
            .s3_bucket(BUCKET)
            .build()
    });

    // Match every PutObject; assert the bucket + key are exactly what
    // we wired upstream (regression guard for the key-threading bug).
    let put_rule = mock!(aws_sdk_s3::Client::put_object)
        .match_requests(|req| req.bucket() == Some(BUCKET) && req.key() == Some(KEY))
        .then_output(|| PutObjectOutput::builder().build());

    let cav_rule = mock!(Client::create_application_version)
        .match_requests(|req| {
            req.application_name() == Some(APP)
                && req.version_label() == Some(LABEL)
                && req.source_bundle().and_then(|s| s.s3_bucket()) == Some(BUCKET)
                && req.source_bundle().and_then(|s| s.s3_key()) == Some(KEY)
                && req.auto_create_application() == Some(false)
        })
        .then_output(|| CreateApplicationVersionOutput::builder().build());

    let upd_rule = mock!(Client::update_environment)
        .match_requests(|req| {
            req.environment_name() == Some(ENV) && req.version_label() == Some(LABEL)
        })
        .then_output(|| UpdateEnvironmentOutput::builder().build());

    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&csl_rule, &cav_rule, &upd_rule]);
    let s3 = mock_client!(aws_sdk_s3, [&put_rule]);
    let client = client_with_eb_and_s3(eb, s3);

    // Stage 1
    let bucket = client
        .create_storage_location()
        .await
        .expect("CreateStorageLocation should return the managed bucket");
    assert_eq!(bucket, BUCKET);

    // Stage 2 — write the bundle to a tempfile and let upload_bundle
    // stream it. Threshold is set very high so this exercises the
    // single-PutObject path (the multipart path is covered by a
    // separate test below).
    let tmp = std::env::temp_dir().join(format!("ebman-test-bundle-{}.zip", std::process::id()));
    std::fs::write(&tmp, &bundle_bytes).expect("write tempfile");
    let upload_res = client
        .upload_bundle_with(&bucket, KEY, &tmp, u64::MAX, 8 * 1024 * 1024)
        .await;
    let _ = std::fs::remove_file(&tmp);
    upload_res.expect("PutObject should succeed");

    // Stage 3
    client
        .create_app_version(APP, LABEL, Some("test deploy"), &bucket, KEY)
        .await
        .expect("CreateApplicationVersion should succeed");

    // Stage 4
    client
        .deploy_version(ENV, LABEL)
        .await
        .expect("UpdateEnvironment should succeed");

    // Each rule should have fired exactly once.
    assert_eq!(csl_rule.num_calls(), 1, "CreateStorageLocation");
    assert_eq!(put_rule.num_calls(), 1, "S3 PutObject");
    assert_eq!(cav_rule.num_calls(), 1, "CreateApplicationVersion");
    assert_eq!(upd_rule.num_calls(), 1, "UpdateEnvironment");
}

#[tokio::test]
async fn list_environments_surfaces_aws_errors_with_op_context() {
    use aws_sdk_elasticbeanstalk::operation::describe_environments::DescribeEnvironmentsError;
    let rule = mock!(Client::describe_environments).then_error(|| {
        DescribeEnvironmentsError::generic(
            aws_smithy_types::error::ErrorMetadata::builder()
                .code("InternalServerError")
                .message("retry later")
                .build(),
        )
    });
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&rule]);
    let client = client_with_eb(eb);

    let err = client
        .list_environments()
        .await
        .expect_err("expected AWS error to propagate");
    assert!(
        err.to_string().contains("DescribeEnvironments"),
        "expected operation context, got {err}"
    );
}

#[tokio::test]
async fn peek_messages_surfaces_sqs_errors_with_op_context() {
    use aws_sdk_sqs::operation::receive_message::ReceiveMessageError;
    let rule = mock!(aws_sdk_sqs::Client::receive_message).then_error(|| {
        ReceiveMessageError::generic(
            aws_smithy_types::error::ErrorMetadata::builder()
                .code("QueueDoesNotExist")
                .message("queue gone")
                .build(),
        )
    });
    let sqs = mock_client!(aws_sdk_sqs, [&rule]);
    let cfg = aws_config::SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    let client = AwsClient::for_tests(
        Client::new(&cfg),
        sqs,
        CwClient::new(&cfg),
        CwLogsClient::new(&cfg),
        S3Client::new(&cfg),
        Ec2Client::new(&cfg),
    );

    let err = client
        .peek_messages("https://sqs.us-east-1.amazonaws.com/123/q", 5)
        .await
        .expect_err("expected SQS error to propagate");
    assert!(
        err.to_string().contains("ReceiveMessage"),
        "expected operation context, got {err}"
    );
}

#[tokio::test]
async fn list_subnets_in_vpc_surfaces_ec2_errors_with_op_context() {
    use aws_sdk_ec2::operation::describe_subnets::DescribeSubnetsError;
    let rule = mock!(aws_sdk_ec2::Client::describe_subnets).then_error(|| {
        DescribeSubnetsError::generic(
            aws_smithy_types::error::ErrorMetadata::builder()
                .code("InvalidVpcID.NotFound")
                .message("vpc-xxx not found")
                .build(),
        )
    });
    let ec2 = mock_client!(aws_sdk_ec2, [&rule]);
    let cfg = aws_config::SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    let client = AwsClient::for_tests(
        Client::new(&cfg),
        SqsClient::new(&cfg),
        CwClient::new(&cfg),
        CwLogsClient::new(&cfg),
        S3Client::new(&cfg),
        ec2,
    );

    let err = client
        .list_subnets_in_vpc("vpc-xxx")
        .await
        .expect_err("expected EC2 error to propagate");
    assert!(
        err.to_string().contains("DescribeSubnets"),
        "expected operation context, got {err}"
    );
}

#[tokio::test]
async fn fetch_alarm_history_extracts_kind_and_summary() {
    // Pins the field mapping from the SDK's AlarmHistoryItem onto
    // ebman's AlarmHistoryEntry. If the SDK ever renames
    // `history_item_type` → something else, this test breaks and
    // the operator-visible `:alarm-history` overlay breaks with
    // it. The mock returns one StateUpdate + one
    // ConfigurationUpdate so both common kinds get an assertion.
    use aws_sdk_cloudwatch::operation::describe_alarm_history::DescribeAlarmHistoryOutput;
    use aws_sdk_cloudwatch::types::{AlarmHistoryItem, HistoryItemType};
    use aws_smithy_types::DateTime as SdkDateTime;

    let rule = mock!(aws_sdk_cloudwatch::Client::describe_alarm_history)
        .match_requests(|req| req.alarm_name() == Some("high-cpu") && req.max_records() == Some(50))
        .then_output(|| {
            DescribeAlarmHistoryOutput::builder()
                .alarm_history_items(
                    AlarmHistoryItem::builder()
                        .alarm_name("high-cpu")
                        .history_item_type(HistoryItemType::StateUpdate)
                        .history_summary("Alarm updated from OK to ALARM")
                        .timestamp(SdkDateTime::from_secs(1_716_640_000))
                        .build(),
                )
                .alarm_history_items(
                    AlarmHistoryItem::builder()
                        .alarm_name("high-cpu")
                        .history_item_type(HistoryItemType::ConfigurationUpdate)
                        .history_summary("Threshold changed to 80")
                        .timestamp(SdkDateTime::from_secs(1_716_530_000))
                        .build(),
                )
                .build()
        });
    let cw = mock_client!(aws_sdk_cloudwatch, [&rule]);
    let client = client_with_cw(cw);

    let entries = client
        .fetch_alarm_history("high-cpu", 50)
        .await
        .expect("ok");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].kind, "StateUpdate");
    assert_eq!(entries[0].summary, "Alarm updated from OK to ALARM");
    assert!(entries[0].at.is_some(), "timestamp coerced from SDK form");
    assert_eq!(entries[1].kind, "ConfigurationUpdate");
    assert_eq!(entries[1].summary, "Threshold changed to 80");
}

#[tokio::test]
async fn fetch_alarm_history_tolerates_missing_optional_fields() {
    // Real CloudWatch sometimes returns items with missing kind /
    // summary / timestamp (especially for older entries). The
    // function must coerce to sensible defaults (`"?"` for an
    // unknown kind, empty string for missing summary,
    // `None` for missing timestamp) rather than panicking.
    use aws_sdk_cloudwatch::operation::describe_alarm_history::DescribeAlarmHistoryOutput;
    use aws_sdk_cloudwatch::types::AlarmHistoryItem;

    let rule = mock!(aws_sdk_cloudwatch::Client::describe_alarm_history).then_output(|| {
        DescribeAlarmHistoryOutput::builder()
            .alarm_history_items(AlarmHistoryItem::builder().build())
            .build()
    });
    let cw = mock_client!(aws_sdk_cloudwatch, [&rule]);
    let client = client_with_cw(cw);

    let entries = client.fetch_alarm_history("any", 10).await.expect("ok");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, "?");
    assert_eq!(entries[0].summary, "");
    assert!(entries[0].at.is_none());
}

#[tokio::test(start_paused = true)]
async fn run_shell_command_collects_per_instance_result_on_success() {
    // Happy path: one instance, SendCommand returns a command_id,
    // first GetCommandInvocation poll returns Success with
    // stdout/stderr/exit_code. `start_paused = true` + advance()
    // skips the actual 2s sleep so the test runs in ms.
    use aws_sdk_ssm::operation::get_command_invocation::GetCommandInvocationOutput;
    use aws_sdk_ssm::operation::send_command::SendCommandOutput;
    use aws_sdk_ssm::types::{Command, CommandInvocationStatus};

    const CMD_ID: &str = "01234567-89ab-cdef-0123-456789abcdef";

    let send_rule = mock!(aws_sdk_ssm::Client::send_command)
        .match_requests(|req| {
            req.document_name() == Some("AWS-RunShellScript")
                && req.instance_ids().contains(&"i-aaa".to_string())
        })
        .then_output(|| {
            SendCommandOutput::builder()
                .command(Command::builder().command_id(CMD_ID).build())
                .build()
        });
    let poll_rule = mock!(aws_sdk_ssm::Client::get_command_invocation)
        .match_requests(|req| {
            req.command_id() == Some(CMD_ID) && req.instance_id() == Some("i-aaa")
        })
        .then_output(|| {
            GetCommandInvocationOutput::builder()
                .command_id(CMD_ID)
                .instance_id("i-aaa")
                .status(CommandInvocationStatus::Success)
                .response_code(0)
                .standard_output_content("up 3 days")
                .build()
        });
    let ssm = mock_client!(aws_sdk_ssm, [&send_rule, &poll_rule]);
    let client = client_with_ssm(ssm);

    // Background the run + advance the paused clock past the
    // first 2s sleep so the poll fires.
    let handle = tokio::spawn(async move {
        client
            .run_shell_command(&["i-aaa".to_string()], "uptime", 60)
            .await
    });
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let results = handle.await.unwrap().expect("ok");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].instance_id, "i-aaa");
    assert_eq!(results[0].status, "Success");
    assert_eq!(results[0].exit_code, 0);
    assert_eq!(results[0].stdout, "up 3 days");
    assert_eq!(results[0].stderr, "");
}

#[tokio::test(start_paused = true)]
async fn run_shell_command_synthesises_local_timeout_when_deadline_passes() {
    // If GetCommandInvocation keeps returning InProgress past the
    // wall-clock deadline, the function emits a synthetic
    // `TimedOut(local)` row for the still-pending instance rather
    // than hanging. Pinning this catches regressions in the
    // deadline-bound break of the poll loop.
    use aws_sdk_ssm::operation::get_command_invocation::GetCommandInvocationOutput;
    use aws_sdk_ssm::operation::send_command::SendCommandOutput;
    use aws_sdk_ssm::types::{Command, CommandInvocationStatus};

    const CMD_ID: &str = "deadbeef-0000-0000-0000-000000000000";

    let send_rule = mock!(aws_sdk_ssm::Client::send_command).then_output(|| {
        SendCommandOutput::builder()
            .command(Command::builder().command_id(CMD_ID).build())
            .build()
    });
    // Permanent InProgress — never resolves.
    let stuck = mock!(aws_sdk_ssm::Client::get_command_invocation).then_output(|| {
        GetCommandInvocationOutput::builder()
            .command_id(CMD_ID)
            .instance_id("i-stuck")
            .status(CommandInvocationStatus::InProgress)
            .response_code(0)
            .build()
    });
    let ssm = mock_client!(aws_sdk_ssm, [&send_rule, &stuck]);
    let client = client_with_ssm(ssm);

    let handle = tokio::spawn(async move {
        // 1s wall-clock — much shorter than the 2s poll interval
        // so we hit the deadline on the FIRST loop iteration.
        client
            .run_shell_command(&["i-stuck".to_string()], "sleep 999", 1)
            .await
    });
    // Advance well past the 1s deadline + the 2s poll interval.
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    let results = handle.await.unwrap().expect("ok");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].instance_id, "i-stuck");
    assert_eq!(
        results[0].status, "TimedOut(local)",
        "synthetic timeout row should signal which instance didn't finish"
    );
    assert_eq!(results[0].exit_code, -1);
}

// ── compare_versions: semver pre-release precedence ────────────────

#[test]
fn compare_versions_ranks_a_prerelease_below_its_release() {
    use std::cmp::Ordering;
    // The bug: cores tie, so the old code fell through to `a.cmp(b)`,
    // and lexicographically "1.0.0-rc1" > "1.0.0" because it's a prefix
    // extension. `:upgrade-platform` then offered an rc as the newest.
    assert_eq!(compare_versions("1.0.0-rc1", "1.0.0"), Ordering::Less);
    assert_eq!(compare_versions("1.0.0", "1.0.0-rc1"), Ordering::Greater);
}

#[test]
fn compare_versions_orders_prereleases_among_themselves() {
    use std::cmp::Ordering;
    assert_eq!(compare_versions("1.0.0-rc1", "1.0.0-rc2"), Ordering::Less);
    assert_eq!(
        compare_versions("1.0.0-alpha", "1.0.0-beta"),
        Ordering::Less
    );
    // Dot-separated identifiers compare left to right, numerically
    // where both are numeric.
    assert_eq!(
        compare_versions("1.0.0-rc.2", "1.0.0-rc.10"),
        Ordering::Less,
        "numeric identifiers compare as numbers, not strings"
    );
    // Numeric ranks below alphanumeric.
    assert_eq!(compare_versions("1.0.0-1", "1.0.0-alpha"), Ordering::Less);
    // Fewer identifiers ranks below more, all else equal.
    assert_eq!(compare_versions("1.0.0-rc", "1.0.0-rc.1"), Ordering::Less);
    assert_eq!(compare_versions("1.0.0-rc1", "1.0.0-rc1"), Ordering::Equal);
}

#[test]
fn compare_versions_still_orders_release_cores() {
    use std::cmp::Ordering;
    // Regression guard: the pre-release work must not disturb the
    // ordering solution stacks rely on.
    assert_eq!(compare_versions("1.0.1", "1.0.0"), Ordering::Greater);
    assert_eq!(compare_versions("2.0.0", "1.9.9"), Ordering::Greater);
    assert_eq!(compare_versions("1.10.0", "1.9.0"), Ordering::Greater);
    assert_eq!(compare_versions("1.0", "1.0.0"), Ordering::Less);
    assert_eq!(compare_versions("4.0.1", "4.0.1"), Ordering::Equal);
}

#[test]
fn platform_picker_sorts_the_release_above_its_rc() {
    // The end-to-end shape: `list_compatible_platforms` sorts
    // descending with `compare_versions(&b.version, &a.version)`.
    let mut versions = vec!["1.0.0", "1.0.0-rc1", "1.0.1", "1.0.0-rc2"];
    versions.sort_by(|a, b| compare_versions(b, a));
    assert_eq!(versions, vec!["1.0.1", "1.0.0", "1.0.0-rc2", "1.0.0-rc1"]);
}

// ── format_insights_results: column set and width measurement ──────

fn insights_row(fields: &[(&str, &str)]) -> InsightsRow {
    InsightsRow {
        fields: fields
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
    }
}

#[test]
fn insights_columns_are_the_union_across_rows_not_just_row_zero() {
    // Insights omits an absent field from a record rather than
    // returning it empty. Taking headers from row 0 alone dropped the
    // `level` column for EVERY row whenever the first matching record
    // happened to be an unstructured line — silently discarding data
    // the operator asked for by name in `fields`.
    let results = InsightsResults {
        rows: vec![
            insights_row(&[("@timestamp", "T1"), ("@message", "plain line")]),
            insights_row(&[
                ("@timestamp", "T2"),
                ("@message", "structured"),
                ("level", "ERROR"),
            ]),
        ],
        records_scanned: 2,
        records_matched: 2,
    };
    let out = format_insights_results(&results, "fields @timestamp, @message, level", &[]);
    assert!(out.contains("level"), "level column must appear:\n{out}");
    assert!(out.contains("ERROR"), "its value must render:\n{out}");
    // The row that lacks the field renders blank, not omitted.
    let body: Vec<&str> = out.lines().filter(|l| l.starts_with('T')).collect();
    assert_eq!(body.len(), 2, "both rows render:\n{out}");
    assert!(body[0].contains("plain line"));
}

#[test]
fn insights_drops_the_synthetic_ptr_field_from_every_row() {
    let results = InsightsResults {
        rows: vec![
            insights_row(&[("@ptr", "abc"), ("@message", "one")]),
            insights_row(&[("@ptr", "def"), ("@message", "two")]),
        ],
        records_scanned: 2,
        records_matched: 2,
    };
    let out = format_insights_results(&results, "q", &[]);
    assert!(
        !out.contains("@ptr"),
        "@ptr must not reach the overlay:\n{out}"
    );
    assert!(!out.contains("abc"));
}

#[test]
fn insights_column_widths_are_measured_in_chars_not_bytes() {
    // A non-ASCII header measured with `len()` over-reserves (three
    // bytes per `é`) while the padding and the separator count chars,
    // so the rule under the header ran short and the columns stepped.
    let results = InsightsResults {
        rows: vec![insights_row(&[("réqüest", "x")])],
        records_scanned: 1,
        records_matched: 1,
    };
    let out = format_insights_results(&results, "q", &[]);
    let lines: Vec<&str> = out.lines().collect();
    let hdr = lines
        .iter()
        .position(|l| l.contains("réqüest"))
        .expect("header row");
    let header_cells = lines[hdr].trim_end().chars().count();
    let sep_cells = lines[hdr + 1].chars().count();
    assert_eq!(
        header_cells,
        sep_cells,
        "separator must be exactly as wide as the header it underlines\nheader: {:?}\nsep:    {:?}",
        lines[hdr],
        lines[hdr + 1]
    );
}

// ── STS expiry conversion ──────────────────────────────────────────

#[test]
fn sts_expiry_converts_a_normal_timestamp() {
    let t = super::sts_expiry_to_system_time(1_700_000_000).expect("representable");
    assert_eq!(
        t.duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        1_700_000_000
    );
}

#[test]
fn sts_expiry_refuses_values_it_cannot_represent() {
    // The old `secs() as u64` wrapped these to ~1.8e19, which made
    // `checked_add` return None, which `Credentials::new` reads as
    // "never expires" — so a skewed clock silently produced a session
    // that was never refreshed and failed every call after an hour.
    for bad in [-1_i64, -1_700_000_000, i64::MIN] {
        let err = super::sts_expiry_to_system_time(bad)
            .expect_err("a negative expiry must be refused, not treated as never-expiring");
        let msg = format!("{err}");
        assert!(
            msg.contains("unusable credential expiry"),
            "error should say what happened: {msg}"
        );
    }
    // Zero is the epoch — representable, but an hour-long STS session
    // is never dated 1970, so it means the same thing. It converts
    // rather than erroring; the SDK sees it as long expired and the
    // refresh tick re-assumes, which is the safe direction.
    assert!(super::sts_expiry_to_system_time(0).is_ok());
}

#[test]
fn sts_expiry_never_wraps_a_large_value_into_the_past() {
    // `SystemTime`'s range is platform-dependent and wide enough here
    // that even `i64::MAX` seconds converts. That's fine — the failure
    // this guards is the other direction, where `as u64` turned a
    // NEGATIVE expiry into a huge one. Whatever the platform does with
    // the top of the range, the result must never land before now.
    if let Ok(t) = super::sts_expiry_to_system_time(i64::MAX) {
        assert!(
            t > std::time::SystemTime::now(),
            "a far-future expiry must not wrap into the past"
        );
    }
    // And the signature itself is the real guarantee: this returns
    // `Result`, so there is no longer any input that yields a silent
    // `None` expiry meaning "never expires".
}

// ── global-service endpoint partition ──────────────────────────────

#[test]
fn global_services_stay_inside_the_operators_partition() {
    use super::global_service_region as g;
    // Commercial — unchanged behaviour.
    assert_eq!(g("us-east-1"), "us-east-1");
    assert_eq!(g("eu-west-2"), "us-east-1");
    assert_eq!(g("ap-southeast-2"), "us-east-1");
    // GovCloud and China: hardcoding us-east-1 here was a
    // cross-partition endpoint, so `:explain` and `:cost on` could
    // never have worked for those operators.
    assert_eq!(g("us-gov-west-1"), "us-gov-west-1");
    assert_eq!(g("us-gov-east-1"), "us-gov-west-1");
    assert_eq!(g("cn-north-1"), "cn-north-1");
    assert_eq!(g("cn-northwest-1"), "cn-north-1");
    // A region we've never heard of falls back to commercial rather
    // than failing — same as before, and the common case.
    assert_eq!(g(""), "us-east-1");
    assert_eq!(g("mars-central-1"), "us-east-1");
}

#[test]
fn global_service_region_never_crosses_a_partition() {
    use super::global_service_region as g;
    // The invariant, stated as a property rather than a table: the
    // chosen endpoint must share the operator region's partition.
    let partition = |r: &str| {
        if r.starts_with("us-gov-") {
            "aws-us-gov"
        } else if r.starts_with("cn-") {
            "aws-cn"
        } else {
            "aws"
        }
    };
    for r in [
        "us-east-1",
        "eu-central-1",
        "sa-east-1",
        "us-gov-west-1",
        "us-gov-east-1",
        "cn-north-1",
        "cn-northwest-1",
    ] {
        assert_eq!(
            partition(g(r)),
            partition(r),
            "global endpoint for {r} left its partition"
        );
    }
}

// ── DescribeEvents pagination ──────────────────────────────────────

#[tokio::test]
async fn list_events_since_follows_next_token() {
    // `:event-tail` advances its watermark past the newest event it
    // received. Dropping `next_token` meant that during a burst larger
    // than one batch, the older events behind the token were never
    // returned by any later poll — silently, with no gap marker.
    use aws_sdk_elasticbeanstalk::operation::describe_events::DescribeEventsOutput;
    use aws_sdk_elasticbeanstalk::types::EventDescription;

    fn ev(msg: &str, secs: i64) -> EventDescription {
        EventDescription::builder()
            .message(msg)
            .environment_name("api-prod")
            .event_date(aws_sdk_elasticbeanstalk::primitives::DateTime::from_secs(
                secs,
            ))
            .build()
    }
    let page1 = mock!(Client::describe_events)
        .match_requests(|req| req.next_token().is_none())
        .then_output(|| {
            DescribeEventsOutput::builder()
                .events(ev("newest", 3_000))
                .next_token("PAGE_2")
                .build()
        });
    let page2 = mock!(Client::describe_events)
        .match_requests(|req| req.next_token() == Some("PAGE_2"))
        .then_output(|| {
            DescribeEventsOutput::builder()
                .events(ev("older — behind the token", 2_000))
                .build()
        });
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&page1, &page2]);
    let client = client_with_eb(eb);

    let events = client.list_events_since(1_000_000, 300).await.unwrap();
    let msgs: Vec<&str> = events.iter().map(|e| e.message.as_str()).collect();
    assert_eq!(
        msgs,
        vec!["newest", "older — behind the token"],
        "both pages must be returned before the watermark advances"
    );
    assert_eq!(page1.num_calls(), 1);
    assert_eq!(page2.num_calls(), 1, "next_token must be followed");
}

#[tokio::test]
async fn list_events_display_calls_do_not_paginate() {
    // The two non-watermarked callers want "the newest N" for a panel.
    // Following tokens there would just cost API calls for events
    // nobody renders, so they stay single-page.
    use aws_sdk_elasticbeanstalk::operation::describe_events::DescribeEventsOutput;
    use aws_sdk_elasticbeanstalk::types::EventDescription;

    let page1 = mock!(Client::describe_events).then_output(|| {
        DescribeEventsOutput::builder()
            .events(
                EventDescription::builder()
                    .message("newest")
                    .environment_name("api-prod")
                    .build(),
            )
            .next_token("PAGE_2")
            .build()
    });
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&page1]);
    let client = client_with_eb(eb);

    let events = client.list_events_for_env("api-prod", 100).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        page1.num_calls(),
        1,
        "a display fetch must not chase next_token"
    );
}

// ── EC2 listings paginate ──────────────────────────────────────────

fn client_with_ec2(ec2: Ec2Client) -> AwsClient {
    let cfg = aws_config::SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    AwsClient::for_tests(
        Client::new(&cfg),
        SqsClient::new(&cfg),
        CwClient::new(&cfg),
        CwLogsClient::new(&cfg),
        S3Client::new(&cfg),
        ec2,
    )
}

#[tokio::test]
async fn list_security_groups_in_vpc_follows_next_token() {
    // A shared VPC can hold more security groups than one page, and a
    // picker that silently shows a subset makes the operator conclude
    // the group doesn't exist and create a duplicate.
    use aws_sdk_ec2::operation::describe_security_groups::DescribeSecurityGroupsOutput;
    use aws_sdk_ec2::types::SecurityGroup;

    fn sg(id: &str, name: &str) -> SecurityGroup {
        SecurityGroup::builder()
            .group_id(id)
            .group_name(name)
            .description("d")
            .build()
    }
    let page1 = mock!(aws_sdk_ec2::Client::describe_security_groups)
        .match_requests(|req| req.next_token().is_none())
        .then_output(|| {
            DescribeSecurityGroupsOutput::builder()
                .security_groups(sg("sg-1", "alpha"))
                .next_token("P2")
                .build()
        });
    let page2 = mock!(aws_sdk_ec2::Client::describe_security_groups)
        .match_requests(|req| req.next_token() == Some("P2"))
        .then_output(|| {
            DescribeSecurityGroupsOutput::builder()
                .security_groups(sg("sg-2", "zulu"))
                .build()
        });
    let ec2 = mock_client!(aws_sdk_ec2, [&page1, &page2]);
    let client = client_with_ec2(ec2);

    let groups = client.list_security_groups_in_vpc("vpc-123").await.unwrap();
    let names: Vec<&str> = groups.iter().map(|g| g.group_name.as_str()).collect();
    assert_eq!(names, vec!["alpha", "zulu"], "both pages must appear");
    assert_eq!(page2.num_calls(), 1, "next_token must be followed");
}

#[tokio::test]
async fn list_subnets_in_vpc_follows_next_token() {
    use aws_sdk_ec2::operation::describe_subnets::DescribeSubnetsOutput;
    use aws_sdk_ec2::types::Subnet;

    fn sn(id: &str, az: &str, cidr: &str) -> Subnet {
        Subnet::builder()
            .subnet_id(id)
            .availability_zone(az)
            .cidr_block(cidr)
            .build()
    }
    let page1 = mock!(aws_sdk_ec2::Client::describe_subnets)
        .match_requests(|req| req.next_token().is_none())
        .then_output(|| {
            DescribeSubnetsOutput::builder()
                .subnets(sn("subnet-1", "us-east-1a", "10.0.1.0/24"))
                .next_token("P2")
                .build()
        });
    let page2 = mock!(aws_sdk_ec2::Client::describe_subnets)
        .match_requests(|req| req.next_token() == Some("P2"))
        .then_output(|| {
            DescribeSubnetsOutput::builder()
                .subnets(sn("subnet-2", "us-east-1b", "10.0.2.0/24"))
                .build()
        });
    let ec2 = mock_client!(aws_sdk_ec2, [&page1, &page2]);
    let client = client_with_ec2(ec2);

    let subnets = client.list_subnets_in_vpc("vpc-123").await.unwrap();
    let ids: Vec<&str> = subnets.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["subnet-1", "subnet-2"]);
    assert_eq!(page2.num_calls(), 1, "next_token must be followed");
}

// ── IAM simulate pagination ────────────────────────────────────────

fn client_with_iam(iam: aws_sdk_iam::Client) -> AwsClient {
    let cfg = aws_config::SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    let mut c = AwsClient::for_tests(
        Client::new(&cfg),
        SqsClient::new(&cfg),
        CwClient::new(&cfg),
        CwLogsClient::new(&cfg),
        S3Client::new(&cfg),
        Ec2Client::new(&cfg),
    );
    c.iam = iam;
    c
}

#[tokio::test]
async fn simulate_principal_policy_follows_the_truncation_marker() {
    // `:explain` renders a decision table. A dropped page doesn't look
    // like an error — the denied action simply isn't listed, and the
    // operator reads "not in the table" as "not the problem".
    use aws_sdk_iam::operation::simulate_principal_policy::SimulatePrincipalPolicyOutput;
    use aws_sdk_iam::types::{EvaluationResult, PolicyEvaluationDecisionType};

    fn res(action: &str, decision: PolicyEvaluationDecisionType) -> EvaluationResult {
        EvaluationResult::builder()
            .eval_action_name(action)
            .eval_decision(decision)
            .build()
            .unwrap()
    }
    let page1 = mock!(aws_sdk_iam::Client::simulate_principal_policy)
        .match_requests(|req| req.marker().is_none())
        .then_output(|| {
            SimulatePrincipalPolicyOutput::builder()
                .evaluation_results(res(
                    "elasticbeanstalk:DescribeEnvironments",
                    PolicyEvaluationDecisionType::Allowed,
                ))
                .is_truncated(true)
                .marker("M2")
                .build()
        });
    let page2 = mock!(aws_sdk_iam::Client::simulate_principal_policy)
        .match_requests(|req| req.marker() == Some("M2"))
        .then_output(|| {
            SimulatePrincipalPolicyOutput::builder()
                .evaluation_results(res(
                    "elasticbeanstalk:UpdateEnvironment",
                    PolicyEvaluationDecisionType::ExplicitDeny,
                ))
                .is_truncated(false)
                .build()
        });
    let iam = mock_client!(aws_sdk_iam, [&page1, &page2]);
    let client = client_with_iam(iam);

    let rows = client
        .simulate_principal_policy(
            "arn:aws:iam::123456789012:role/eb-ec2",
            &[
                "elasticbeanstalk:DescribeEnvironments".to_string(),
                "elasticbeanstalk:UpdateEnvironment".to_string(),
            ],
            &[],
        )
        .await
        .unwrap();
    let actions: Vec<&str> = rows.iter().map(|r| r.action.as_str()).collect();
    assert!(
        actions.contains(&"elasticbeanstalk:UpdateEnvironment"),
        "the denied action behind the marker must reach the overlay: {actions:?}"
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(page2.num_calls(), 1, "marker must be followed");
}

#[tokio::test]
async fn simulate_principal_policy_stops_when_not_truncated() {
    // A marker present without `is_truncated` must not start a loop.
    use aws_sdk_iam::operation::simulate_principal_policy::SimulatePrincipalPolicyOutput;
    use aws_sdk_iam::types::{EvaluationResult, PolicyEvaluationDecisionType};

    let page1 = mock!(aws_sdk_iam::Client::simulate_principal_policy).then_output(|| {
        SimulatePrincipalPolicyOutput::builder()
            .evaluation_results(
                EvaluationResult::builder()
                    .eval_action_name("s3:GetObject")
                    .eval_decision(PolicyEvaluationDecisionType::Allowed)
                    .build()
                    .unwrap(),
            )
            .is_truncated(false)
            .marker("STALE")
            .build()
    });
    let iam = mock_client!(aws_sdk_iam, [&page1]);
    let client = client_with_iam(iam);

    let rows = client
        .simulate_principal_policy("arn:aws:iam::1:role/r", &["s3:GetObject".to_string()], &[])
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(page1.num_calls(), 1, "must not loop on a stale marker");
}

// ── log-tail boundary dedupe survives a stalled watermark ──────────

#[tokio::test]
async fn log_tail_does_not_re_emit_after_a_truncated_poll_goes_quiet() {
    // The loop: a truncated poll stalls the watermark at `max_ts` and
    // carries that millisecond's ids. The group then goes quiet, so the
    // next poll skips them correctly, delivers nothing — and, not being
    // truncated itself, used to drop the carry while leaving the
    // watermark where it was. Every poll after that re-fetched the same
    // events with an empty skip set and re-printed them.
    use aws_sdk_cloudwatchlogs::operation::filter_log_events::FilterLogEventsOutput;
    use aws_sdk_cloudwatchlogs::types::FilteredLogEvent;
    use std::collections::HashSet;

    fn event() -> FilteredLogEvent {
        FilteredLogEvent::builder()
            .event_id("EV-1")
            .timestamp(1_000)
            .log_stream_name("i-abc")
            .message("boundary line")
            .build()
    }
    // First poll: page 1 returns the event and a token; every following
    // page keeps handing back a token, so the page cap is reached and
    // the poll ends truncated with the watermark stalled at 1000.
    let first = mock!(aws_sdk_cloudwatchlogs::Client::filter_log_events)
        .match_requests(|req| req.start_time() == Some(500) && req.next_token().is_none())
        .then_output(|| {
            FilterLogEventsOutput::builder()
                .events(event())
                .next_token("MORE")
                .build()
        });
    let more = mock!(aws_sdk_cloudwatchlogs::Client::filter_log_events)
        .match_requests(|req| req.next_token() == Some("MORE"))
        .then_output(|| FilterLogEventsOutput::builder().next_token("MORE").build());
    // Later polls start at the stalled watermark and see only the same
    // event again — the group has gone quiet.
    let quiet = mock!(aws_sdk_cloudwatchlogs::Client::filter_log_events)
        .match_requests(|req| req.start_time() == Some(1_000) && req.next_token().is_none())
        .then_output(|| FilterLogEventsOutput::builder().events(event()).build());

    // MatchAny, not the default Sequential: these rules describe
    // request *shapes* that recur across polls, not a fixed call order.
    let cw_logs = mock_client!(
        aws_sdk_cloudwatchlogs,
        aws_smithy_mocks::RuleMode::MatchAny,
        [&first, &more, &quiet]
    );
    let client = client_with_cw_logs(cw_logs);

    // Poll 1 — truncated, stalls at 1000, carries EV-1.
    let (events, next_since, carry) = client
        .fetch_recent_log_events("/aws/eb/api-prod", 500, 1000, &HashSet::new())
        .await
        .unwrap();
    assert!(!events.is_empty(), "poll 1 delivers the line");
    assert_eq!(next_since, 1_000, "truncated poll must not skip the ms");
    assert!(carry.contains("EV-1"));

    // Poll 2 — quiet. Skips the known id, delivers nothing, watermark
    // still stalled, so the carry must survive.
    let (events, next_since, carry) = client
        .fetch_recent_log_events("/aws/eb/api-prod", next_since, 1000, &carry)
        .await
        .unwrap();
    assert!(events.is_empty(), "already-delivered line must be skipped");
    assert_eq!(next_since, 1_000);
    assert!(
        carry.contains("EV-1"),
        "the watermark did not move, so the skip set must be kept"
    );

    // Poll 3 — this is where the loop used to start.
    let (events, _, _) = client
        .fetch_recent_log_events("/aws/eb/api-prod", next_since, 1000, &carry)
        .await
        .unwrap();
    assert!(
        events.is_empty(),
        "the same line must not be re-emitted on every subsequent poll"
    );
}

#[tokio::test]
async fn log_tail_clean_poll_advances_past_the_boundary_and_carries_nothing() {
    // The complement: when the watermark does advance past everything
    // returned, nothing gets re-fetched, so nothing needs carrying.
    use aws_sdk_cloudwatchlogs::operation::filter_log_events::FilterLogEventsOutput;
    use aws_sdk_cloudwatchlogs::types::FilteredLogEvent;
    use std::collections::HashSet;

    let page = mock!(aws_sdk_cloudwatchlogs::Client::filter_log_events).then_output(|| {
        FilterLogEventsOutput::builder()
            .events(
                FilteredLogEvent::builder()
                    .event_id("EV-9")
                    .timestamp(2_000)
                    .log_stream_name("i-abc")
                    .message("line")
                    .build(),
            )
            .build()
    });
    let cw_logs = mock_client!(aws_sdk_cloudwatchlogs, [&page]);
    let client = client_with_cw_logs(cw_logs);

    let (events, next_since, carry) = client
        .fetch_recent_log_events("/aws/eb/api-prod", 500, 1000, &HashSet::new())
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(next_since, 2_001, "clean poll advances past the newest ms");
    assert!(
        carry.is_empty(),
        "nothing is re-fetched, so nothing carries"
    );
}

// ── multipart abort on the missing-ETag path ───────────────────────

#[tokio::test]
async fn upload_bundle_aborts_multipart_when_a_part_returns_no_etag() {
    // Every failure path after CreateMultipartUpload must abort, or S3
    // keeps the already-uploaded parts — billed, with no object in the
    // listing. This path bare-`?`d out instead, so a >64 MiB bundle
    // could leave gigabytes orphaned.
    use aws_sdk_s3::operation::abort_multipart_upload::AbortMultipartUploadOutput;
    use aws_sdk_s3::operation::create_multipart_upload::CreateMultipartUploadOutput;
    use aws_sdk_s3::operation::upload_part::UploadPartOutput;

    let cmu = mock!(aws_sdk_s3::Client::create_multipart_upload).then_output(|| {
        CreateMultipartUploadOutput::builder()
            .upload_id("UP-1")
            .build()
    });
    // Succeeds at the HTTP level but carries no ETag.
    let up_no_etag =
        mock!(aws_sdk_s3::Client::upload_part).then_output(|| UploadPartOutput::builder().build());
    let abort = mock!(aws_sdk_s3::Client::abort_multipart_upload)
        .then_output(|| AbortMultipartUploadOutput::builder().build());
    let s3 = mock_client!(
        aws_sdk_s3,
        aws_smithy_mocks::RuleMode::MatchAny,
        [&cmu, &up_no_etag, &abort]
    );

    // Same tempfile convention as the sibling multipart tests.
    let path = std::env::temp_dir().join(format!("ebman-test-no-etag-{}.bin", std::process::id()));
    std::fs::write(&path, vec![0u8; 12]).expect("write tempfile");

    let cfg = aws_config::SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    let client = AwsClient::for_tests(
        Client::new(&cfg),
        SqsClient::new(&cfg),
        CwClient::new(&cfg),
        CwLogsClient::new(&cfg),
        s3,
        Ec2Client::new(&cfg),
    );

    // Threshold 1 byte / part size 8 bytes forces two parts.
    let err = client
        .upload_bundle_with("bucket", "key.zip", &path, 1, 8)
        .await
        .expect_err("a part with no ETag must fail the upload");
    let _ = std::fs::remove_file(&path);
    let msg = format!("{err:#}");
    assert!(
        msg.contains("no ETag"),
        "error should name the cause: {msg}"
    );
    assert_eq!(
        abort.num_calls(),
        1,
        "AbortMultipartUpload must fire so the uploaded parts aren't orphaned"
    );
}

// ── alarm attribution ──────────────────────────────────────────────

#[tokio::test]
async fn list_alarms_for_env_ignores_a_same_named_resource_in_another_service() {
    // Matching on the dimension VALUE alone attributed any alarm whose
    // dimension happened to equal the env name — so an RDS instance,
    // SQS queue or ECS service called `payments` showed up in the EB
    // env `payments`'s Detail pane and in `:why`.
    use aws_sdk_cloudwatch::operation::describe_alarms::DescribeAlarmsOutput;
    use aws_sdk_cloudwatch::types::{Dimension, MetricAlarm};

    fn alarm(name: &str, ns: &str, dim_name: &str, dim_value: &str) -> MetricAlarm {
        MetricAlarm::builder()
            .alarm_name(name)
            .namespace(ns)
            .metric_name("m")
            .dimensions(Dimension::builder().name(dim_name).value(dim_value).build())
            .build()
    }
    let rule = mock!(aws_sdk_cloudwatch::Client::describe_alarms).then_output(|| {
        DescribeAlarmsOutput::builder()
            .metric_alarms(alarm(
                "eb-health",
                "AWS/ElasticBeanstalk",
                "EnvironmentName",
                "payments",
            ))
            .metric_alarms(alarm(
                "rds-cpu",
                "AWS/RDS",
                "DBInstanceIdentifier",
                "payments",
            ))
            .metric_alarms(alarm("sqs-depth", "AWS/SQS", "QueueName", "payments"))
            // An operator-authored alarm in a custom namespace, but
            // genuinely dimensioned by the environment — must be kept.
            .metric_alarms(alarm(
                "custom-slo",
                "Acme/Platform",
                "EnvironmentName",
                "payments",
            ))
            .build()
    });
    let cw = mock_client!(aws_sdk_cloudwatch, [&rule]);
    let client = client_with_cw(cw);

    let alarms = client.list_alarms_for_env("payments").await.unwrap();
    let names: Vec<&str> = alarms.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["eb-health", "custom-slo"],
        "only EnvironmentName-dimensioned alarms belong to an EB env"
    );
}

#[tokio::test]
async fn list_alarms_for_env_matches_when_env_is_not_the_first_dimension() {
    use aws_sdk_cloudwatch::operation::describe_alarms::DescribeAlarmsOutput;
    use aws_sdk_cloudwatch::types::{Dimension, MetricAlarm};

    let rule = mock!(aws_sdk_cloudwatch::Client::describe_alarms).then_output(|| {
        DescribeAlarmsOutput::builder()
            .metric_alarms(
                MetricAlarm::builder()
                    .alarm_name("multi-dim")
                    .namespace("AWS/ElasticBeanstalk")
                    .metric_name("m")
                    .dimensions(
                        Dimension::builder()
                            .name("InstanceId")
                            .value("i-123")
                            .build(),
                    )
                    .dimensions(
                        Dimension::builder()
                            .name("EnvironmentName")
                            .value("payments")
                            .build(),
                    )
                    .build(),
            )
            .build()
    });
    let cw = mock_client!(aws_sdk_cloudwatch, [&rule]);
    let client = client_with_cw(cw);

    let alarms = client.list_alarms_for_env("payments").await.unwrap();
    assert_eq!(alarms.len(), 1, "dimension order must not matter");
}

// ── Cost Explorer page cap is not silent ───────────────────────────

#[tokio::test]
async fn fetch_env_costs_flags_a_truncated_walk() {
    // Falling out of the page cap used to return the partial map with
    // no signal, and the caller cached it for 24 hours — so every env
    // past the cap read as unknown cost, indistinguishable from an
    // untagged one, until the cache expired.
    use aws_sdk_costexplorer::operation::get_cost_and_usage::GetCostAndUsageOutput;
    use aws_sdk_costexplorer::types::{Group, MetricValue, ResultByTime};
    use std::collections::HashMap;

    // Every page hands back another token, so the walk can only end by
    // hitting the cap.
    let endless = mock!(aws_sdk_costexplorer::Client::get_cost_and_usage).then_output(|| {
        let mut metrics = HashMap::new();
        metrics.insert(
            "UnblendedCost".to_string(),
            MetricValue::builder().amount("1.00").unit("USD").build(),
        );
        GetCostAndUsageOutput::builder()
            .results_by_time(
                ResultByTime::builder()
                    .groups(
                        Group::builder()
                            .keys("elasticbeanstalk:environment-name$api-prod")
                            .set_metrics(Some(metrics))
                            .build(),
                    )
                    .build(),
            )
            .next_page_token("MORE")
            .build()
    });
    let cost = mock_client!(
        aws_sdk_costexplorer,
        aws_smithy_mocks::RuleMode::MatchAny,
        [&endless]
    );
    let cfg = aws_config::SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    let mut client = AwsClient::for_tests(
        Client::new(&cfg),
        SqsClient::new(&cfg),
        CwClient::new(&cfg),
        CwLogsClient::new(&cfg),
        S3Client::new(&cfg),
        Ec2Client::new(&cfg),
    );
    client.cost = cost;

    let costs = client
        .fetch_env_costs()
        .await
        .expect("partial data still returned");
    assert!(
        costs.truncated,
        "a walk cut short by the page cap must say so"
    );
    assert!(
        !costs.rows.is_empty(),
        "partial data is still worth rendering — it just must not be cached"
    );
}

// ── multi-region row labelling ─────────────────────────────────────

#[test]
fn stamp_region_labels_every_row_with_the_resolved_region() {
    // Both multi-region entry points share this step. They diverged
    // once — one stamping the REQUESTED region, the other the resolved
    // one — and the difference only showed when a region string failed
    // to bind and the SDK fell back to its chain, at which point the
    // fan-out queried one region and labelled the rows with another.
    fn env(name: &str, region: Option<&str>) -> crate::aws::Environment {
        crate::aws::Environment {
            name: name.into(),
            application: "app".into(),
            status: "Ready".into(),
            health: "Green".into(),
            platform: String::new(),
            solution_stack: String::new(),
            tier: "WebServer".into(),
            cname: String::new(),
            version_label: String::new(),
            arn: None,
            updated: None,
            id: None,
            region: region.map(str::to_string),
        }
    }
    let mut envs = vec![
        env("api-prod", None),
        // A stale label from a previous pass must be overwritten, not
        // preserved.
        env("web-prod", Some("eu-west-1")),
    ];
    super::eb::stamp_region(&mut envs, "us-east-1");
    assert!(envs
        .iter()
        .all(|e| e.region.as_deref() == Some("us-east-1")));
}

// ── concurrent SSM polling keeps results attributed correctly ──────

#[tokio::test(start_paused = true)]
async fn run_shell_command_polls_instances_concurrently_without_mixing_results() {
    // The cycle now polls instances concurrently, because doing it
    // sequentially cost one round trip per instance with the deadline
    // checked only afterwards — so a large env could burn its whole
    // wall clock on one cycle and write every instance off as
    // `TimedOut(local)` while the command was running fine.
    //
    // The risk a concurrent cycle introduces is pairing the wrong
    // response to the wrong instance, so that is what this pins.
    use aws_sdk_ssm::operation::get_command_invocation::GetCommandInvocationOutput;
    use aws_sdk_ssm::operation::send_command::SendCommandOutput;
    use aws_sdk_ssm::types::{Command, CommandInvocationStatus};

    const CMD_ID: &str = "cmd-concurrent";
    let send_rule = mock!(aws_sdk_ssm::Client::send_command).then_output(|| {
        SendCommandOutput::builder()
            .command(Command::builder().command_id(CMD_ID).build())
            .build()
    });
    // One rule per instance, each returning that instance's own output.
    let mk = |id: &'static str, out: &'static str| {
        mock!(aws_sdk_ssm::Client::get_command_invocation)
            .match_requests(move |req| req.instance_id() == Some(id))
            .then_output(move || {
                GetCommandInvocationOutput::builder()
                    .command_id(CMD_ID)
                    .instance_id(id)
                    .status(CommandInvocationStatus::Success)
                    .response_code(0)
                    .standard_output_content(out)
                    .build()
            })
    };
    let a = mk("i-aaa", "host-a");
    let b = mk("i-bbb", "host-b");
    let c = mk("i-ccc", "host-c");
    let ssm = mock_client!(
        aws_sdk_ssm,
        aws_smithy_mocks::RuleMode::MatchAny,
        [&send_rule, &a, &b, &c]
    );
    let client = client_with_ssm(ssm);

    let handle = tokio::spawn(async move {
        client
            .run_shell_command(
                &[
                    "i-aaa".to_string(),
                    "i-bbb".to_string(),
                    "i-ccc".to_string(),
                ],
                "hostname",
                60,
            )
            .await
    });
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let results = handle.await.unwrap().expect("ok");

    assert_eq!(results.len(), 3, "every instance resolves in one cycle");
    // Results are sorted by instance id, so this also pins the pairing.
    let pairs: Vec<(&str, &str)> = results
        .iter()
        .map(|r| (r.instance_id.as_str(), r.stdout.as_str()))
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("i-aaa", "host-a"),
            ("i-bbb", "host-b"),
            ("i-ccc", "host-c")
        ],
        "each instance must carry its own output"
    );
    assert!(results.iter().all(|r| r.status == "Success"));
}
