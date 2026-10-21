//! Tests for `aws`: S3 bundle uploads.

use super::*;

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
