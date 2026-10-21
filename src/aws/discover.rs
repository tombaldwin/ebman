//! The App tab editor's discovery, as AWS calls: every lookup at once,
//! each bounded, each failure recorded rather than fatal. What the
//! answers mean is `app_discover`'s, which is pure.

/// The longest the editor waits on any one discovery lookup.
const LOOKUP_BOUND: std::time::Duration = std::time::Duration::from_secs(15);

/// A lookup within [`LOOKUP_BOUND`]: past it, an error saying so.
async fn bounded<T>(
    f: impl std::future::Future<Output = color_eyre::eyre::Result<T>>,
) -> color_eyre::eyre::Result<T> {
    tokio::time::timeout(LOOKUP_BOUND, f)
        .await
        .unwrap_or_else(|_| {
            Err(color_eyre::eyre::eyre!(
                "no answer within {}s",
                LOOKUP_BOUND.as_secs()
            ))
        })
}

/// A lookup's result, or nothing with its failure recorded.
fn take<T>(
    failed: &mut Vec<(String, String)>,
    what: &str,
    r: color_eyre::eyre::Result<Vec<T>>,
) -> Vec<T> {
    r.unwrap_or_else(|e| {
        failed.push((what.to_string(), e.to_string()));
        Vec::new()
    })
}

/// Every lookup at once; each failure is recorded, not fatal.
pub(crate) async fn discover_app(
    aws: &crate::aws::AwsClient,
    app_name: &str,
    env: &str,
    fleet: Vec<String>,
    dimension_names: Vec<String>,
) -> crate::app_discover::Discovered {
    let prefix = format!("{env}-");
    // Each lookup pages within its own deadline; `bounded` also caps a
    // single request that hangs, so the form never waits past it.
    let (groups, vars, rds, cache, alarms, custom) = tokio::join!(
        bounded(aws.discover_env_log_groups(env)),
        bounded(aws.fetch_env_vars(app_name, env)),
        bounded(aws.list_dimension_values("AWS/RDS", "CPUUtilization", "DBInstanceIdentifier")),
        bounded(aws.list_dimension_values("AWS/ElastiCache", "CPUUtilization", "CacheClusterId")),
        bounded(aws.list_alarm_names_with_prefix(&prefix)),
        bounded(aws.list_metrics_dimensioned_by(&dimension_names, env)),
    );
    let mut failed = Vec::new();
    let inputs = crate::app_discover::Inputs {
        log_groups: take(&mut failed, "log groups", groups),
        // Only endpoint hostnames survive this line; no other value.
        endpoint_hosts: crate::app_discover::endpoint_hosts(&take(
            &mut failed,
            "environment variables",
            vars,
        )),
        rds_ids: take(&mut failed, "RDS instances", rds),
        cache_ids: take(&mut failed, "cache nodes", cache),
        prefixed_alarms: take(&mut failed, "alarms", alarms),
        fleet,
        custom_metrics: take(&mut failed, "custom metrics", custom),
    };
    crate::app_discover::Discovered {
        discovery: crate::app_discover::discover(env, &inputs),
        failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Discovery end to end against mocked AWS: each lookup's answer
    /// reaches the suggestions, and a failed one is named, not hidden.
    #[tokio::test]
    async fn discovery_reads_each_lookup_and_names_the_one_that_failed() {
        use aws_smithy_mocks::{mock, mock_client, RuleMode};
        let groups = mock!(aws_sdk_cloudwatchlogs::Client::describe_log_groups).then_output(|| {
            use aws_sdk_cloudwatchlogs::types::LogGroup;
            aws_sdk_cloudwatchlogs::operation::describe_log_groups::DescribeLogGroupsOutput::builder()
                .log_groups(LogGroup::builder().log_group_name("/aws/elasticbeanstalk/e/var/log/eb-engine.log").build())
                .log_groups(LogGroup::builder().log_group_name("/aws/elasticbeanstalk/e/var/log/app.log").build())
                .build()
        });
        let settings = mock!(aws_sdk_elasticbeanstalk::Client::describe_configuration_settings).then_output(|| {
            use aws_sdk_elasticbeanstalk::types::{ConfigurationOptionSetting, ConfigurationSettingsDescription};
            let var = |name: &str, value: &str| {
                ConfigurationOptionSetting::builder()
                    .namespace("aws:elasticbeanstalk:application:environment")
                    .option_name(name)
                    .value(value)
                    .build()
            };
            aws_sdk_elasticbeanstalk::operation::describe_configuration_settings::DescribeConfigurationSettingsOutput::builder()
                .configuration_settings(
                    ConfigurationSettingsDescription::builder()
                        .option_settings(var("REDIS", "master.sess.x.usw1.cache.amazonaws.com"))
                        .option_settings(var("DB_PASSWORD", "hunter2"))
                        .build(),
                )
                .build()
        });
        let metrics = |ns: &'static str, dim: &'static str, value: &'static str| {
            mock!(aws_sdk_cloudwatch::Client::list_metrics)
                .match_requests(move |r| r.namespace() == Some(ns))
                .then_output(move || {
                    use aws_sdk_cloudwatch::types::{Dimension, Metric};
                    aws_sdk_cloudwatch::operation::list_metrics::ListMetricsOutput::builder()
                        .metrics(
                            Metric::builder()
                                .dimensions(Dimension::builder().name(dim).value(value).build())
                                .build(),
                        )
                        .build()
                })
        };
        let rds = metrics("AWS/RDS", "DBInstanceIdentifier", "db-a");
        let cache = metrics("AWS/ElastiCache", "CacheClusterId", "sess-001");
        let custom = mock!(aws_sdk_cloudwatch::Client::list_metrics)
            .match_requests(|r| r.namespace().is_none())
            .then_error(|| {
                aws_sdk_cloudwatch::operation::list_metrics::ListMetricsError::unhandled(
                    "AccessDenied",
                )
            });
        let alarms = mock!(aws_sdk_cloudwatch::Client::describe_alarms)
            .match_requests(|r| r.alarm_name_prefix() == Some("e-"))
            .then_output(|| {
                use aws_sdk_cloudwatch::types::MetricAlarm;
                aws_sdk_cloudwatch::operation::describe_alarms::DescribeAlarmsOutput::builder()
                    .metric_alarms(MetricAlarm::builder().alarm_name("e-5xx").build())
                    .build()
            });
        let sdk = aws_config::SdkConfig::builder()
            .region(aws_config::Region::new("us-east-1"))
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build();
        let aws = crate::aws::AwsClient::for_tests(
            mock_client!(aws_sdk_elasticbeanstalk, RuleMode::MatchAny, [&settings]),
            aws_sdk_sqs::Client::new(&sdk),
            mock_client!(
                aws_sdk_cloudwatch,
                RuleMode::MatchAny,
                [&rds, &cache, &custom, &alarms]
            ),
            mock_client!(aws_sdk_cloudwatchlogs, RuleMode::MatchAny, [&groups]),
            aws_sdk_s3::Client::new(&sdk),
            aws_sdk_ec2::Client::new(&sdk),
        );
        let found = discover_app(
            &aws,
            "shop",
            "e",
            vec!["e".into()],
            vec!["Environment".into()],
        )
        .await;
        let d = &found.discovery;
        assert_eq!(d.app_log.as_deref(), Some("var/log/app.log"));
        assert_eq!(d.alarm_prefix, Some(("e-".to_string(), 1)));
        assert_eq!(d.rds_options, ["db-a"]);
        assert!(d.rds_found.is_empty(), "no env var names it: offered only");
        assert_eq!(
            d.cache_found,
            [("sess-001".to_string(), "REDIS".to_string())]
        );
        assert_eq!(found.failed.len(), 1, "{:?}", found.failed);
        assert_eq!(found.failed[0].0, "custom metrics");
        assert!(
            !format!("{found:?}").contains("hunter2"),
            "no other env var value is kept"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_lookup_that_never_answers_gives_up_at_the_bound() {
        let started = tokio::time::Instant::now();
        let r: color_eyre::eyre::Result<()> = bounded(std::future::pending()).await;
        assert_eq!(
            started.elapsed(),
            LOOKUP_BOUND,
            "gave up at the bound, not later"
        );
        let e = r.expect_err("gave up");
        assert!(e.to_string().contains("no answer within 15s"), "{e}");
        assert!(bounded(async { Ok(1) }).await.is_ok());
    }
}
