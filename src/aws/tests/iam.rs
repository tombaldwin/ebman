//! Tests for `aws`: IAM.

use super::*;

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
    assert!(!rows.truncated, "two pages, then a clean stop");
    let rows = rows.items();
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
    assert!(!rows.truncated);
    let rows = rows.items();
    assert_eq!(rows.len(), 1);
    assert_eq!(page1.num_calls(), 1, "must not loop on a stale marker");
}

#[test]
fn seeding_a_lazy_client_wins_over_get_or_init() {
    // How the mock-AWS tests inject: seed the cell, and `get_or_init`
    // hands back the mock rather than constructing a real client.
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
        Ec2Client::new(&cfg),
    );
    let seeded = aws_sdk_iam::Client::new(&cfg);
    assert!(client.iam.set(seeded).is_ok());
    // Accessor returns the seeded instance, not a fresh one.
    let got = client.iam() as *const _;
    let stored = client.iam.get().unwrap() as *const _;
    assert_eq!(got, stored);
}
