//! Tests for `aws`: CloudWatch metrics and alarms, and which alarms are an env's.

use super::*;

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
        .fetch_env_metrics("poly-prod", 900)
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
async fn fetch_app_metrics_follows_pages_and_reports_a_short_answer() {
    // One series split across two pages must come back whole, and a
    // series still short once paging is over (here Forbidden), or a
    // CloudWatch message, must surface as a note rather than as a
    // quietly shorter chart. PartialData on a page with a next one is
    // just paging, and must NOT become a note.
    use aws_sdk_cloudwatch::operation::get_metric_data::GetMetricDataOutput;
    use aws_sdk_cloudwatch::types::{MessageData, MetricDataResult, StatusCode};
    use aws_smithy_types::DateTime as SdkDateTime;

    let result = |id: &str, label: &str, secs: i64, v: f64, code: StatusCode| {
        MetricDataResult::builder()
            .id(id)
            .label(label)
            .timestamps(SdkDateTime::from_secs(secs))
            .values(v)
            .status_code(code)
            .build()
    };
    let first = mock!(aws_sdk_cloudwatch::Client::get_metric_data)
        .match_requests(|req| {
            let ids: Vec<&str> = req
                .metric_data_queries()
                .iter()
                .filter_map(|q| q.id())
                .collect();
            req.next_token().is_none() && ids == ["a0", "i0", "x0"]
        })
        .then_output(move || {
            GetMetricDataOutput::builder()
                .metric_data_results(result("a0", "users", 2, 9.0, StatusCode::Complete))
                .metric_data_results(result("i0", "i-a", 2, 4.0, StatusCode::PartialData))
                .messages(
                    MessageData::builder()
                        .code("MaxQueryResults")
                        .value("500 limit")
                        .build(),
                )
                .next_token("page2")
                .build()
        });
    let second = mock!(aws_sdk_cloudwatch::Client::get_metric_data)
        .match_requests(|req| req.next_token() == Some("page2"))
        .then_output(move || {
            GetMetricDataOutput::builder()
                .metric_data_results(result("a0", "users", 1, 8.0, StatusCode::Complete))
                .metric_data_results(result("i0", "i-a", 1, 3.0, StatusCode::Complete))
                .metric_data_results(result("x0", "", 1, 3.0, StatusCode::Forbidden))
                // A sibling SEARCH series still short on the last page:
                // i-a's Complete must not hide it.
                .metric_data_results(result("i0", "i-b", 1, 5.0, StatusCode::PartialData))
                .build()
        });
    let cw = mock_client!(
        aws_sdk_cloudwatch,
        aws_smithy_mocks::RuleMode::MatchAny,
        [&first, &second]
    );
    let client = client_with_cw(cw);

    let spec = crate::config::parse(
        "app.e.namespace = \"N\"\n\
         app.e.instance_dimension = \"InstanceId\"\n\
         app.e.metric.users = \"users|Sum\"\n",
    )
    .app_tabs
    .envs
    .remove("e")
    .unwrap();
    let m = client
        .fetch_app_metrics("e", 900, &spec)
        .await
        .expect("fetch");

    assert_eq!((first.num_calls(), second.num_calls()), (1, 1));
    let values: Vec<f64> = m.series[0].points.iter().map(|(_, v)| *v).collect();
    assert_eq!(values, [8.0, 9.0], "both pages, oldest first");
    assert_eq!(
        m.series[0].instances.len(),
        2,
        "i-a across two pages, and i-b"
    );
    assert_eq!(m.series[0].instances[0].1.len(), 2);
    assert_eq!(
        m.notes,
        [
            "MaxQueryResults: 500 limit",
            "x0: Forbidden",
            "i0 i-b: PartialData"
        ],
        "i0 i-a was PartialData only on a page with a next one; i-b on the last"
    );
}

#[tokio::test]
async fn fetch_app_metrics_refuses_more_queries_than_one_call_takes() {
    let rule = mock!(aws_sdk_cloudwatch::Client::get_metric_data).then_output(|| {
        aws_sdk_cloudwatch::operation::get_metric_data::GetMetricDataOutput::builder().build()
    });
    let client = client_with_cw(mock_client!(aws_sdk_cloudwatch, [&rule]));
    let mut cfg = String::from("app.e.namespace = \"N\"\napp.e.instance_dimension = \"I\"\n");
    for i in 0..167 {
        cfg.push_str(&format!("app.e.metric.m{i} = \"X|Sum\"\n"));
    }
    let spec = crate::config::parse(&cfg)
        .app_tabs
        .envs
        .remove("e")
        .unwrap();
    let err = client
        .fetch_app_metrics("e", 900, &spec)
        .await
        .expect_err("501 queries");
    assert!(err.to_string().contains("501 queries"), "{err}");
    assert_eq!(rule.num_calls(), 0, "not sent to be refused");
}

#[tokio::test]
async fn fetch_app_metrics_makes_no_call_without_a_namespace() {
    let rule = mock!(aws_sdk_cloudwatch::Client::get_metric_data).then_output(|| {
        aws_sdk_cloudwatch::operation::get_metric_data::GetMetricDataOutput::builder().build()
    });
    let client = client_with_cw(mock_client!(aws_sdk_cloudwatch, [&rule]));
    let m = client
        .fetch_app_metrics("e", 900, &crate::app_tab::AppTabSpec::default())
        .await
        .expect("fetch");
    assert_eq!(rule.num_calls(), 0);
    assert!(m.series.is_empty());
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

    let alarms = client
        .list_alarms_for_env("payments", &by_dimensions(&[super::ENV_DIMENSION]))
        .await
        .unwrap();
    let names: Vec<&str> = alarms.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(
        names,
        // Same state, so by name.
        vec!["custom-slo", "eb-health"],
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

    let alarms = client
        .list_alarms_for_env("payments", &by_dimensions(&[super::ENV_DIMENSION]))
        .await
        .unwrap();
    assert_eq!(alarms.len(), 1, "dimension order must not matter");
}

#[tokio::test]
async fn a_truncated_scan_errors_rather_than_reporting_no_match() {
    // `list_alarms_for_env` and `list_secrets` scan the whole account
    // and filter afterwards, so a walk cut short by the runaway cap is
    // indistinguishable from "nothing matched" — and during triage
    // "no alarms" reads as a finding. They must refuse instead.
    use aws_sdk_cloudwatch::operation::describe_alarms::DescribeAlarmsOutput;

    let endless = mock!(aws_sdk_cloudwatch::Client::describe_alarms)
        .then_output(|| DescribeAlarmsOutput::builder().next_token("MORE").build());
    let cw = mock_client!(
        aws_sdk_cloudwatch,
        aws_smithy_mocks::RuleMode::MatchAny,
        [&endless]
    );
    let client = client_with_cw(cw);

    let err = client
        .list_alarms_for_env("payments", &by_dimensions(&[super::ENV_DIMENSION]))
        .await
        .expect_err("a truncated scan must not be reported as 'no alarms'");
    let msg = format!("{err}");
    assert!(
        msg.contains("partial scan looks identical to no match"),
        "the error must explain why it refused: {msg}"
    );
}

#[tokio::test]
async fn alarm_dimension_names_are_configurable() {
    // Tightening the match to `EnvironmentName` fixed a real false
    // positive (an RDS alarm named after the env) but silently dropped
    // operator-authored alarms that spell the dimension differently.
    // `alarm_dimensions` is the way back, without reinstating the
    // false positive: the VALUE still has to be the env name, and an
    // RDS `DBInstanceIdentifier` still won't match unless the operator
    // explicitly asks for it.
    use aws_sdk_cloudwatch::operation::describe_alarms::DescribeAlarmsOutput;
    use aws_sdk_cloudwatch::types::{Dimension, MetricAlarm};

    fn alarm(name: &str, dim_name: &str) -> MetricAlarm {
        MetricAlarm::builder()
            .alarm_name(name)
            .namespace("Acme/Platform")
            .metric_name("m")
            .dimensions(
                Dimension::builder()
                    .name(dim_name)
                    .value("payments")
                    .build(),
            )
            .build()
    }
    let make = || {
        mock!(aws_sdk_cloudwatch::Client::describe_alarms).then_output(|| {
            DescribeAlarmsOutput::builder()
                .metric_alarms(alarm("canonical", "EnvironmentName"))
                .metric_alarms(alarm("operator-spelling", "Environment"))
                .metric_alarms(alarm("rds", "DBInstanceIdentifier"))
                .build()
        })
    };

    // Default: only the canonical dimension.
    let rule = make();
    let client = client_with_cw(mock_client!(aws_sdk_cloudwatch, [&rule]));
    let names: Vec<String> = client
        .list_alarms_for_env("payments", &by_dimensions(&[super::ENV_DIMENSION]))
        .await
        .unwrap()
        .into_iter()
        .map(|a| a.name)
        .collect();
    assert_eq!(names, vec!["canonical"]);

    // Widened: the operator's own spelling is included, the unrelated
    // RDS alarm still isn't.
    let rule = make();
    let client = client_with_cw(mock_client!(aws_sdk_cloudwatch, [&rule]));
    let names: Vec<String> = client
        .list_alarms_for_env(
            "payments",
            &by_dimensions(&["EnvironmentName", "Environment"]),
        )
        .await
        .unwrap()
        .into_iter()
        .map(|a| a.name)
        .collect();
    assert_eq!(names, vec!["canonical", "operator-spelling"]);
}

#[tokio::test]
async fn the_fleet_alarm_check_asks_only_for_firing_alarms() {
    // One call per region per refresh: filtered by CloudWatch, so a
    // quiet account is one small page rather than every alarm it has.
    use aws_sdk_cloudwatch::operation::describe_alarms::DescribeAlarmsOutput;
    use aws_sdk_cloudwatch::types::{Dimension, MetricAlarm, StateValue};
    let rule = mock!(aws_sdk_cloudwatch::Client::describe_alarms)
        .match_requests(|req| req.state_value() == Some(&StateValue::Alarm))
        .then_output(|| {
            DescribeAlarmsOutput::builder()
                .metric_alarms(
                    MetricAlarm::builder()
                        .alarm_name("shop-prod-5xx")
                        .dimensions(
                            Dimension::builder()
                                .name("EnvironmentName")
                                .value("shop-prod")
                                .build(),
                        )
                        .build(),
                )
                .build()
        });
    let client = client_with_cw(mock_client!(aws_sdk_cloudwatch, [&rule]));
    let firing = client
        .list_firing_alarms()
        .await
        .expect("matched the ALARM filter");
    assert_eq!(
        firing,
        vec![FiringAlarm {
            name: "shop-prod-5xx".into(),
            dimensions: vec![("EnvironmentName".into(), "shop-prod".into())],
        }]
    );
}

#[tokio::test]
async fn an_alarm_prefix_adds_alarms_by_name_and_all_come_back_firing_first() {
    // Alarms dimensioned by something other than the env (an RDS or ALB
    // alarm named for it) belong to it by name. The prefix ADDS to the
    // dimension match, and an empty one matches nothing.
    use aws_sdk_cloudwatch::operation::describe_alarms::DescribeAlarmsOutput;
    use aws_sdk_cloudwatch::types::{Dimension, MetricAlarm, StateValue};
    let alarm = |name: &str, dim: &str, value: &str, state: StateValue| {
        MetricAlarm::builder()
            .alarm_name(name)
            .state_value(state)
            .dimensions(Dimension::builder().name(dim).value(value).build())
            .build()
    };
    let make = move || {
        mock!(aws_sdk_cloudwatch::Client::describe_alarms).then_output(move || {
            DescribeAlarmsOutput::builder()
                .metric_alarms(alarm(
                    "payments-db-cpu",
                    "DBInstanceIdentifier",
                    "pay-db",
                    StateValue::Ok,
                ))
                .metric_alarms(alarm(
                    "by-dimension",
                    "EnvironmentName",
                    "payments",
                    StateValue::Ok,
                ))
                .metric_alarms(alarm(
                    "payments-5xx",
                    "LoadBalancer",
                    "app/x",
                    StateValue::Alarm,
                ))
                .metric_alarms(alarm(
                    "other-env-cpu",
                    "DBInstanceIdentifier",
                    "x",
                    StateValue::Alarm,
                ))
                .metric_alarms(alarm(
                    "payments-lag",
                    "DBInstanceIdentifier",
                    "r",
                    StateValue::InsufficientData,
                ))
                .build()
        })
    };
    let names = |matching: crate::alarms::AlarmMatch| {
        let rule = make();
        let client = client_with_cw(mock_client!(aws_sdk_cloudwatch, [&rule]));
        async move {
            client
                .list_alarms_for_env("payments", &matching)
                .await
                .unwrap()
                .into_iter()
                .map(|a| a.name)
                .collect::<Vec<String>>()
        }
    };
    let mut with_prefix = by_dimensions(&["EnvironmentName"]);
    with_prefix.name_prefix = Some("payments-".into());
    assert_eq!(
        names(with_prefix).await,
        [
            "payments-5xx",
            "payments-lag",
            "by-dimension",
            "payments-db-cpu"
        ],
        "ALARM, then INSUFFICIENT_DATA, then OK by name; another env's alarm stays out"
    );
    let mut empty = by_dimensions(&["EnvironmentName"]);
    empty.name_prefix = Some(String::new());
    assert_eq!(
        names(empty).await,
        ["by-dimension"],
        "an empty prefix matches nothing"
    );
}

#[test]
fn the_alarm_match_takes_each_envs_own_prefix() {
    let cfg = crate::config::parse("app.a.alarm_prefix = \"A-\"\napp.b.namespace = \"N\"\n");
    let dims = ["EnvironmentName".to_string()];
    let a = crate::alarms::AlarmMatch::for_env(&dims, &cfg.app_tabs, "a", ["a", "b"]);
    assert_eq!(a.name_prefix.as_deref(), Some("A-"));
    assert_eq!(a.dimension_names, dims);
    assert_eq!(
        crate::alarms::AlarmMatch::for_env(&dims, &cfg.app_tabs, "b", ["a", "b"]).name_prefix,
        None
    );
    assert_eq!(
        crate::alarms::AlarmMatch::for_env(&dims, &cfg.app_tabs, "c", ["a", "b"]).name_prefix,
        None
    );
}

#[test]
fn a_prefix_gives_way_to_a_longer_env_name_it_also_covers() {
    // `Shop-prod-` covers the worker `Shop-prod-wk`'s alarms too. They
    // are the worker's: the prefix must not take them.
    let cfg = crate::config::parse("app.Shop-prod.alarm_prefix = \"Shop-prod-\"\n");
    let dims = ["EnvironmentName".to_string()];
    let m = crate::alarms::AlarmMatch::for_env(
        &dims,
        &cfg.app_tabs,
        "Shop-prod",
        ["Shop-prod", "Shop-prod-wk", "Shop-staging"],
    );
    assert_eq!(m.shadowed_by, ["Shop-prod-wk"]);
    let none: [(Option<&str>, Option<&str>); 0] = [];
    assert!(m.matches("Shop-prod", "Shop-prod-app-5xx", &none));
    assert!(
        m.matches("Shop-prod", "Shop-prod-wkld-cpu", &none),
        "Shop-prod's own wkld alarm, not the worker's"
    );
    assert!(
        !m.matches("Shop-prod", "Shop-prod-wk-health", &none),
        "the worker's alarm"
    );
    // A dimension match is not affected: the alarm says whose it is.
    assert!(m.matches(
        "Shop-prod",
        "Shop-prod-wk-odd",
        &[(Some("EnvironmentName"), Some("Shop-prod"))]
    ));
}
