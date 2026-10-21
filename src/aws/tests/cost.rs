//! Tests for `aws`: Cost Explorer.

use super::*;

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
                                .keys("elasticbeanstalk:environment-name$poly-prod")
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
    assert_eq!(costs.rows[0].env_name, "poly-prod");
    assert!(!costs.truncated, "a single complete page is not truncated");
    assert!(
        (costs.rows[0].cost_usd - 150.25).abs() < f64::EPSILON,
        "amount parsed from string"
    );
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
    let client = AwsClient::for_tests(
        Client::new(&cfg),
        SqsClient::new(&cfg),
        CwClient::new(&cfg),
        CwLogsClient::new(&cfg),
        S3Client::new(&cfg),
        Ec2Client::new(&cfg),
    );
    assert!(
        client.cost.set(cost).is_ok(),
        "mock injection must win the cell"
    );

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
