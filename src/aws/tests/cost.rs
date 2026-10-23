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
                // Tag first, then region: the response's keys follow
                // the GroupBy order, and the parse reads them by position.
                && req.group_by().len() == 2
                && req.group_by()[0].key() == Some("elasticbeanstalk:environment-name")
                && req.group_by()[1].key() == Some("REGION")
        })
        .then_output(|| {
            let group = |region: &str, amount: &str| {
                let mut metrics = std::collections::HashMap::new();
                metrics.insert(
                    "UnblendedCost".to_string(),
                    MetricValue::builder().amount(amount).unit("USD").build(),
                );
                Group::builder()
                    .keys("elasticbeanstalk:environment-name$poly-prod")
                    .keys(region)
                    .set_metrics(Some(metrics))
                    .build()
            };
            GetCostAndUsageOutput::builder()
                .results_by_time(
                    ResultByTime::builder()
                        .groups(group("us-east-1", "150.25"))
                        .groups(group("eu-west-2", "20.00"))
                        .build(),
                )
                .build()
        });
    let cost = mock_client!(aws_sdk_costexplorer, [&rule]);
    let client = client_with_sub!(cost = cost);

    let costs = client.fetch_env_costs().await.expect("ok");
    assert!(!costs.truncated, "a single complete page is not truncated");
    let mut rows: Vec<(String, String, f64)> = costs
        .rows
        .iter()
        .map(|r| (r.env_name.clone(), r.region.clone(), r.cost_usd))
        .collect();
    rows.sort_by(|a, b| a.1.cmp(&b.1));
    assert_eq!(
        rows,
        vec![
            ("poly-prod".into(), "eu-west-2".into(), 20.0),
            ("poly-prod".into(), "us-east-1".into(), 150.25),
        ],
        "same-named envs in two regions are two rows, amounts parsed from strings"
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
                            .keys("us-east-1")
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

#[test]
fn spend_in_no_region_goes_to_the_one_env_of_that_name() {
    use std::collections::HashMap;
    let key = |n: &str, r: &str| (n.to_string(), r.to_string());
    let totals: HashMap<(String, String), f64> = [
        (key("solo", "us-east-1"), 10.0),
        (key("solo", "global"), 2.5),
        (key("twin", "us-east-1"), 5.0),
        (key("twin", "eu-west-2"), 7.0),
        (key("twin", "NoRegion"), 1.0),
        (key("orphan", "global"), 3.0),
        (key("gov", "us-gov-west-1"), 4.0),
    ]
    .into_iter()
    .collect();
    let folded = crate::aws::fold_regionless(totals);
    let mut got: Vec<_> = folded.into_iter().collect();
    got.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        got,
        vec![
            (key("gov", "us-gov-west-1"), 4.0),
            (key("solo", "us-east-1"), 12.5),
            (key("twin", "eu-west-2"), 7.0),
            (key("twin", "us-east-1"), 5.0),
        ],
        "folded where one region names the env; never guessed between twins; \
         no row in a region no env is in"
    );
}

#[test]
fn region_names_are_told_from_cost_explorers_other_buckets() {
    for r in ["us-east-1", "us-gov-west-1", "cn-north-1", "ap-southeast-4"] {
        assert!(crate::aws::is_region(r), "{r}");
    }
    // One per clause: too few parts, an empty part, a non-numeric tail.
    for r in ["global", "NoRegion", "", "eu-1", "us--1", "us-east-x"] {
        assert!(!crate::aws::is_region(r), "{r}");
    }
}

/// Local Zone spend is its parent region's: kept as its own key it
/// matched no row and vanished from the env's figure.
#[test]
fn local_zone_spend_goes_to_its_parent_region() {
    assert_eq!(crate::aws::parent_region("us-west-2-lax-1"), "us-west-2");
    assert_eq!(
        crate::aws::parent_region("us-east-1-wl1-bos-wlz-1"),
        "us-east-1"
    );
    assert_eq!(crate::aws::parent_region("us-gov-west-1"), "us-gov-west-1");
    assert_eq!(crate::aws::parent_region("global"), "global");
    let key = |n: &str, r: &str| (n.to_string(), r.to_string());
    let totals: std::collections::HashMap<(String, String), f64> = [
        (key("api", "us-west-2"), 10.0),
        (key("api", "us-west-2-lax-1"), 2.0),
    ]
    .into_iter()
    .collect();
    let folded = crate::aws::fold_regionless(totals);
    assert_eq!(folded.get(&key("api", "us-west-2")), Some(&12.0));
    assert_eq!(folded.len(), 1);
}
