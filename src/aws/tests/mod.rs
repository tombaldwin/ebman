//! Unit tests for the `aws` module, one file per service (`core` is the
//! client itself). This file holds what they share: the imports and the
//! mocked-client helpers.
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
// Explicit, because `aws.rs` no longer re-exports `s3::*`. That glob
// existed only to widen the public API; nothing in the lib used it, so
// it read as dead the moment the module went `pub(crate)` — but the
// tests DID use it, and they don't compile in the lib build where that
// was measured. Importing what they need says so.
use super::s3::{plan_part_lengths, should_multipart};

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
/// seed the lazy cell on the constructed AwsClient, which
/// `get_or_init` then hands back in place of a real client.
fn client_with_ssm(ssm: aws_sdk_ssm::Client) -> AwsClient {
    let cfg = aws_config::SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    let c = AwsClient::for_tests(
        Client::new(&cfg),
        SqsClient::new(&cfg),
        CwClient::new(&cfg),
        CwLogsClient::new(&cfg),
        S3Client::new(&cfg),
        Ec2Client::new(&cfg),
    );
    assert!(c.ssm.set(ssm).is_ok(), "mock injection must win the cell");
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
        let c = AwsClient::for_tests(
            Client::new(&cfg),
            SqsClient::new(&cfg),
            CwClient::new(&cfg),
            CwLogsClient::new(&cfg),
            S3Client::new(&cfg),
            Ec2Client::new(&cfg),
        );
        // Seed the lazy cell rather than overwriting a field —
        // `get_or_init` will then hand back the mock.
        assert!(
            c.$field.set($value).is_ok(),
            "mock injection must win the cell"
        );
        c
    }};
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

// ── IAM simulate pagination ────────────────────────────────────────

fn client_with_iam(iam: aws_sdk_iam::Client) -> AwsClient {
    let cfg = aws_config::SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    let c = AwsClient::for_tests(
        Client::new(&cfg),
        SqsClient::new(&cfg),
        CwClient::new(&cfg),
        CwLogsClient::new(&cfg),
        S3Client::new(&cfg),
        Ec2Client::new(&cfg),
    );
    assert!(c.iam.set(iam).is_ok(), "mock injection must win the cell");
    c
}

fn by_dimensions(names: &[&str]) -> crate::alarms::AlarmMatch {
    crate::alarms::AlarmMatch {
        dimension_names: names.iter().map(|n| n.to_string()).collect(),
        name_prefix: None,
        shadowed_by: Vec::new(),
    }
}

// After the helpers: `macro_rules!` is visible only to what follows it.
mod acm;
mod cloudwatch;
mod core;
mod cost;
mod eb;
mod ec2;
mod iam;
mod logs;
mod org;
mod s3;
mod secrets;
mod sqs;
mod ssm;
