//! The MCP `app_metrics` tool: the TUI's App tab as one read.
//!
//! Each section is fetched on its own and, if it fails, comes back
//! `null` with the reason in `errors` — "could not look" must never read
//! as "nothing there". The fetches are the App tab's, so an agent and an
//! operator looking at the same env see the same thing.

use super::*;
use crate::app_tab::{AppMetrics, AppTabSpec, Point, TopErrors};

/// How long top errors may run inside the 30s tool bound. The sections
/// run concurrently, so this bounds the query, not the bundle; past it
/// the query is stopped on the server.
const TOP_ERRORS_BOUND_SECS: u64 = 20;

pub(super) const DESCRIPTION: &str = "The App tab for one environment: the operator's own CloudWatch metrics and the RDS / ElastiCache resources named under `app.ENV.*` in config.toml, enhanced health's figures per instance, and the env's alarms (matched by dimension or by `app.ENV.alarm_prefix`, ALARM first). CAVEATS: only what config names is read, so a clean result says nothing about metrics or resources it was not told about; `configured: false` means the env has no App tab config and `metrics` / `resources` are empty for that reason, not because nothing is wrong. A metric is only as fresh as the application publishes it: read `latest_at`, not just `latest`. Reads go to the `region` argument (or the profile's default), not the env's own region: for an env elsewhere, pass `region`, since a wrong one answers with empty series (`latest: null`), not an error. Instance figures are enhanced health's last window (about 10s), null without enhanced health. `top_errors` is NOT REDACTED: each fingerprint is the raw text of the application's ERROR lines (by default the first 100 characters of each), so anything the application logged there (tokens, connection strings, customer data) reaches the client verbatim. It is null unless you pass `top_errors: true` AND the env has `app.ENV.log_group` (asked for without one, `notes` says so): it runs a Logs Insights query, which is billed per GB scanned, over the last 15 minutes, and counts only lines containing ERROR. `notes` carries anything short of a complete CloudWatch answer, `config_errors` the env's config lines that did not apply, and `errors` each section that failed (that section is null).";

pub(super) fn schema() -> Value {
    json!({
        "name": "app_metrics",
        "description": DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "env": {"type": "string", "description": "Environment name (required)"},
                "minutes": {"type": "integer", "description": "Metric window (default 60, 15 to 1440)"},
                "top_errors": {"type": "boolean", "description": "Also run the Logs Insights top-errors query (default false; billed per GB scanned)"},
                "profile": {"type": "string", "description": "AWS profile (default: ambient)"},
                "region": {"type": "string", "description": "AWS region (default: profile/env default)"}
            },
            "required": ["env"]
        }
    })
}

impl Server {
    pub(super) async fn tool_app_metrics(&self, args: &Value) -> Result<String, String> {
        let env = arg_str(args, "env").ok_or("'env' is required")?;
        let minutes = arg_u64(args, "minutes").unwrap_or(60).clamp(15, 1440) as i64;
        let want_top = args
            .get("top_errors")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let live = self.live_config();
        let tabs = &live.app_tabs;
        let spec = tabs.envs.get(&env).cloned();
        let config_errors: Vec<String> = tabs.errors_for(&env).map(str::to_string).collect();
        let mut errors: Vec<(String, String)> = Vec::new();

        if matches!(self.backend, Backend::Demo) {
            let metrics = spec.as_ref().map(crate::app_tab::demo_series);
            return Ok(render(&Bundle {
                env: &env,
                minutes,
                spec: spec.as_ref(),
                metrics: metrics.as_ref(),
                instances: Some(&crate::demo_fixture::instances_for(&env)),
                alarms: Some(&crate::demo_fixture::alarms_for_env(&env)),
                top_errors: None,
                config_errors: &config_errors,
                top_errors_note: want_top
                    .then_some("top_errors: demo mode makes no Logs Insights call"),
                errors: &errors,
            })
            .to_string());
        }

        let client = self.client(args).await?;
        let has_log_group = spec.as_ref().is_some_and(|s| s.log_group.is_some());
        let log_group = spec
            .as_ref()
            .and_then(|s| s.log_group.as_deref())
            .map(|g| crate::app_tab::top_errors_log_group(&env, g))
            .filter(|_| want_top);
        // Concurrently, so the slowest section and not their sum meets
        // the 30s tool bound; top errors has its own, shorter one.
        let (metrics, instances, alarms, top_errors) = tokio::join!(
            async {
                match &spec {
                    None => Ok(None),
                    Some(s) => client
                        .fetch_app_metrics(&env, minutes * 60, s)
                        .await
                        .map(Some),
                }
            },
            client.list_instances(&env),
            async {
                // A prefix needs the fleet, so another env's alarms that
                // share it stay out (`AlarmMatch::shadowed_by`). Listed
                // inside this section, so its time counts against the
                // section rather than delaying all of them; and only when
                // a prefix is set.
                let fleet: Vec<String> = if spec.as_ref().is_some_and(|s| s.alarm_prefix.is_some())
                {
                    match self.fetch_envs(args).await {
                        Ok(envs) => envs.into_iter().map(|e| e.name).collect(),
                        // Without the fleet a prefix could claim another
                        // env's alarms: fail the section, don't guess.
                        Err(e) => {
                            return Err(color_eyre::eyre::eyre!(
                                "could not list environments to keep other envs' alarms \
                                 out of the alarm_prefix match: {e}"
                            ))
                        }
                    }
                } else {
                    Vec::new()
                };
                let matching = crate::alarms::AlarmMatch::for_env(
                    &live.alarm_dimensions,
                    tabs,
                    &env,
                    fleet.iter().map(String::as_str),
                );
                client.list_alarms_for_env(&env, &matching).await
            },
            async {
                let Some(group) = log_group else {
                    return Ok(None);
                };
                client
                    .fetch_top_errors(
                        group,
                        spec.as_ref().and_then(|s| s.error_fingerprint.as_deref()),
                        std::time::Duration::from_secs(TOP_ERRORS_BOUND_SECS),
                    )
                    .await
                    .map(Some)
            },
        );
        let metrics = keep(&mut errors, "metrics", metrics);
        let instances = keep(&mut errors, "instances", instances.map(Some));
        let alarms = keep(&mut errors, "alarms", alarms.map(Some));
        let top_errors = keep(&mut errors, "top_errors", top_errors);
        Ok(render(&Bundle {
            env: &env,
            minutes,
            spec: spec.as_ref(),
            metrics: metrics.as_ref(),
            instances: instances.as_deref(),
            alarms: alarms.as_deref(),
            top_errors: top_errors.as_ref(),
            config_errors: &config_errors,
            top_errors_note: (want_top && !has_log_group).then_some(
                "top_errors: this env has no app.ENV.log_group, so there is no query to run",
            ),
            errors: &errors,
        })
        .to_string())
    }
}

/// A section's value, or `None` with its failure recorded in `errors`.
fn keep<T>(
    errors: &mut Vec<(String, String)>,
    section: &str,
    r: color_eyre::eyre::Result<Option<T>>,
) -> Option<T> {
    r.unwrap_or_else(|e| {
        errors.push((section.to_string(), e.to_string()));
        None
    })
}

/// Everything the tool answers with. A `None` section is `null` in the
/// JSON: not configured, not asked for, or failed (then in `errors`).
pub(super) struct Bundle<'a> {
    pub env: &'a str,
    pub minutes: i64,
    pub spec: Option<&'a AppTabSpec>,
    pub metrics: Option<&'a AppMetrics>,
    pub instances: Option<&'a [aws::Instance]>,
    pub alarms: Option<&'a [aws::CwAlarm]>,
    pub top_errors: Option<&'a TopErrors>,
    pub config_errors: &'a [String],
    /// Why `top_errors` is null although it was asked for, when no
    /// section failed: said in `notes` so the null is not mistaken for
    /// "no errors".
    pub top_errors_note: Option<&'a str>,
    pub errors: &'a [(String, String)],
}

fn latest(points: &[Point]) -> Value {
    points.last().map_or(Value::Null, |(_, v)| json!(v))
}

fn latest_at(points: &[Point]) -> Value {
    points
        .last()
        .map_or(Value::Null, |(t, _)| json!(t.to_rfc3339()))
}

/// Latest, min, max and how many points: enough to read a trend from
/// without shipping a day of one-minute samples into a transcript.
fn summary(points: &[Point]) -> Value {
    let values = points.iter().map(|(_, v)| *v);
    let min = values
        .clone()
        .fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.min(v))));
    let max = values.fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))));
    json!({
        "latest": latest(points),
        "latest_at": latest_at(points),
        "min": min,
        "max": max,
        "points": points.len(),
    })
}

pub(super) fn render(b: &Bundle) -> Value {
    let metrics: Value = match b.metrics {
        None if b.spec.is_some() => Value::Null,
        None => json!([]),
        Some(m) => m
            .series
            .iter()
            .map(|s| {
                let mut v = summary(&s.points);
                v["label"] = json!(s.label);
                v["unit"] = json!(s.unit);
                v["level"] = match (s.points.last(), s.threshold) {
                    (Some((_, latest)), Some(t)) => json!(match t.classify(*latest) {
                        crate::app_tab::Level::Ok => "ok",
                        crate::app_tab::Level::Warn => "warn",
                        crate::app_tab::Level::Critical => "critical",
                    }),
                    _ => Value::Null,
                };
                v["instances"] = s
                    .instances
                    .iter()
                    .map(|(id, points)| json!({"id": id, "latest": latest(points)}))
                    .collect();
                v
            })
            .collect(),
    };
    let resources: Value = match b.metrics {
        None if b.spec.is_some() => Value::Null,
        None => json!([]),
        Some(m) => m
            .resources
            .iter()
            .map(|r| {
                json!({
                    "kind": r.kind.label(),
                    "id": r.id,
                    "metrics": r.metrics.iter().map(|(m, points)| {
                        let mut v = summary(points);
                        v["name"] = json!(m.name);
                        v
                    }).collect::<Vec<_>>(),
                })
            })
            .collect(),
    };
    let instances = b.instances.map(|list| {
        list.iter()
            .map(|i| {
                let s = i.app_stats.as_ref();
                json!({
                    "id": i.id,
                    "health": i.health,
                    "color": i.color,
                    "requests_per_sec": s.and_then(|s| s.requests_per_sec),
                    "p99_secs": s.and_then(|s| s.p99_secs),
                    "pct_4xx": s.and_then(|s| s.pct_4xx),
                    "pct_5xx": s.and_then(|s| s.pct_5xx),
                    "cpu_pct": s.and_then(|s| s.cpu_pct),
                })
            })
            .collect::<Vec<_>>()
    });
    let alarms = b.alarms.map(|list| {
        list.iter()
            .map(|a| json!({"name": a.name, "state": a.state, "reason": a.state_reason}))
            .collect::<Vec<_>>()
    });
    let top_errors = b.top_errors.map(|t| {
        json!({
            "log_group": t.log_group,
            "window_minutes": crate::app_tab::TOP_ERRORS_WINDOW_MINS,
            "rows": t.rows.iter().map(|(fp, n)| json!({"fingerprint": fp, "count": n})).collect::<Vec<_>>(),
            "more_fingerprints": t.more.0,
            "more_lines": t.more.1,
            "truncated": t.truncated,
            "unmatched": t.unmatched,
            "records_scanned": t.records_scanned,
        })
    });
    json!({
        "env": b.env,
        "minutes": b.minutes,
        "configured": b.spec.is_some(),
        "metrics": metrics,
        "resources": resources,
        "instances": instances,
        "alarms": alarms,
        "top_errors": top_errors,
        "notes": b
            .metrics
            .map(|m| m.notes.clone())
            .unwrap_or_default()
            .into_iter()
            .chain(b.top_errors_note.map(str::to_string))
            .collect::<Vec<_>>(),
        "config_errors": b.config_errors,
        "errors": b.errors.iter().map(|(section, error)| json!({"section": section, "error": error})).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_smithy_mocks::{mock, mock_client, RuleMode};

    fn at(secs: i64, v: f64) -> Point {
        (chrono::DateTime::from_timestamp(secs, 0).unwrap(), v)
    }

    fn bundle<'a>(
        spec: Option<&'a AppTabSpec>,
        metrics: Option<&'a AppMetrics>,
        errors: &'a [(String, String)],
    ) -> Value {
        render(&Bundle {
            env: "e",
            minutes: 60,
            spec,
            metrics,
            instances: None,
            alarms: None,
            top_errors: None,
            config_errors: &[],
            top_errors_note: None,
            errors,
        })
    }

    #[test]
    fn the_description_says_top_errors_is_raw_log_text() {
        // As recent_logs does: an agent must be told a field carries what
        // the application logged, verbatim.
        assert!(DESCRIPTION.contains("`top_errors` is NOT REDACTED"));
    }

    #[test]
    fn no_config_is_said_so_and_reads_as_empty_not_failed() {
        let v = bundle(None, None, &[]);
        assert_eq!(v["configured"], false);
        assert_eq!(v["metrics"], json!([]));
        assert_eq!(v["resources"], json!([]));
        assert_eq!(v["top_errors"], Value::Null);
    }

    #[test]
    fn a_failed_metrics_fetch_is_null_with_its_reason() {
        let spec = AppTabSpec::default();
        let errors = [("metrics".to_string(), "AccessDenied".to_string())];
        let v = bundle(Some(&spec), None, &errors);
        assert_eq!(v["configured"], true);
        assert_eq!(
            v["metrics"],
            Value::Null,
            "could not look is not nothing there"
        );
        assert_eq!(v["resources"], Value::Null);
        assert_eq!(
            v["errors"][0],
            json!({"section": "metrics", "error": "AccessDenied"})
        );
    }

    #[test]
    fn a_series_carries_latest_its_time_range_and_level() {
        let spec = crate::config::parse(
            "app.e.namespace = \"N\"\n\
             app.e.metric.heap = \"heap|Average|MB\"\n\
             app.e.threshold.heap = \"10,20\"\n",
        )
        .app_tabs
        .envs
        .remove("e")
        .unwrap();
        let mut m = crate::app_tab::series_from_results(&spec, vec![], vec!["n1".into()]);
        m.series[0].points = vec![at(1, 5.0), at(2, 25.0), at(3, 12.0)];
        m.series[0].instances = vec![("i-a".into(), vec![at(3, 11.0)])];
        let v = bundle(Some(&spec), Some(&m), &[]);
        let s = &v["metrics"][0];
        assert_eq!(s["label"], "heap");
        assert_eq!(s["unit"], "MB");
        assert_eq!(s["latest"], 12.0);
        assert_eq!(s["latest_at"], "1970-01-01T00:00:03+00:00");
        assert_eq!(
            (s["min"].clone(), s["max"].clone()),
            (json!(5.0), json!(25.0))
        );
        assert_eq!(s["points"], 3);
        assert_eq!(
            s["level"], "warn",
            "12 is past warn 10, short of critical 20"
        );
        assert_eq!(s["instances"], json!([{"id": "i-a", "latest": 11.0}]));
        assert_eq!(v["notes"], json!(["n1"]));
    }

    /// End to end against mocked AWS: the alarms take the env's prefix,
    /// and top errors runs only when asked for.
    #[tokio::test]
    async fn the_tool_reads_the_app_tab_and_queries_insights_only_when_asked() {
        use aws_sdk_cloudwatch::types::{Dimension, MetricAlarm, MetricDataResult, StateValue};
        let metric = mock!(aws_sdk_cloudwatch::Client::get_metric_data).then_output(|| {
            aws_sdk_cloudwatch::operation::get_metric_data::GetMetricDataOutput::builder()
                .metric_data_results(
                    MetricDataResult::builder()
                        .id("a0")
                        .timestamps(aws_smithy_types::DateTime::from_secs(100))
                        .values(42.0)
                        .build(),
                )
                .build()
        });
        let alarms = mock!(aws_sdk_cloudwatch::Client::describe_alarms).then_output(|| {
            aws_sdk_cloudwatch::operation::describe_alarms::DescribeAlarmsOutput::builder()
                .metric_alarms(
                    MetricAlarm::builder()
                        .alarm_name("api-prod-db")
                        .state_value(StateValue::Alarm)
                        .dimensions(
                            Dimension::builder()
                                .name("DBInstanceIdentifier")
                                .value("db")
                                .build(),
                        )
                        .build(),
                )
                .metric_alarms(
                    MetricAlarm::builder()
                        .alarm_name("api-prod-wk-health")
                        .state_value(StateValue::Alarm)
                        .build(),
                )
                // The env's only under the `alarm_dimensions` a file
                // edited after start adds.
                .metric_alarms(
                    MetricAlarm::builder()
                        .alarm_name("own-spelling")
                        .state_value(StateValue::Alarm)
                        .dimensions(
                            Dimension::builder()
                                .name("Environment")
                                .value("api-prod")
                                .build(),
                        )
                        .build(),
                )
                .build()
        });
        // The fleet, for the prefix: the worker's alarms are its own.
        let listing = mock!(aws_sdk_elasticbeanstalk::Client::describe_environments).then_output(|| {
            use aws_sdk_elasticbeanstalk::types::EnvironmentDescription;
            aws_sdk_elasticbeanstalk::operation::describe_environments::DescribeEnvironmentsOutput::builder()
                .environments(EnvironmentDescription::builder().environment_name("api-prod").build())
                .environments(EnvironmentDescription::builder().environment_name("api-prod-wk").build())
                .build()
        });
        let health = mock!(aws_sdk_elasticbeanstalk::Client::describe_instances_health).then_output(|| {
            aws_sdk_elasticbeanstalk::operation::describe_instances_health::DescribeInstancesHealthOutput::builder()
                .build()
        });
        let start = mock!(aws_sdk_cloudwatchlogs::Client::start_query).then_output(|| {
            aws_sdk_cloudwatchlogs::operation::start_query::StartQueryOutput::builder()
                .query_id("q")
                .build()
        });
        let results = mock!(aws_sdk_cloudwatchlogs::Client::get_query_results).then_output(|| {
            use aws_sdk_cloudwatchlogs::types::{QueryStatus, ResultField};
            let field = |k: &str, v: &str| ResultField::builder().field(k).value(v).build();
            aws_sdk_cloudwatchlogs::operation::get_query_results::GetQueryResultsOutput::builder()
                .status(QueryStatus::Complete)
                .results(vec![field("fp", "Boom"), field("n", "3")])
                .build()
        });
        let server = |cfg: crate::config::Config| {
            let sdk = aws_config::SdkConfig::builder()
                .region(aws_config::Region::new("us-west-1"))
                .behavior_version(aws_config::BehaviorVersion::latest())
                .build();
            let client = crate::aws::AwsClient::for_tests(
                mock_client!(
                    aws_sdk_elasticbeanstalk,
                    RuleMode::MatchAny,
                    [&health, &listing]
                ),
                aws_sdk_sqs::Client::new(&sdk),
                mock_client!(aws_sdk_cloudwatch, RuleMode::MatchAny, [&metric, &alarms]),
                mock_client!(
                    aws_sdk_cloudwatchlogs,
                    RuleMode::MatchAny,
                    [&start, &results]
                ),
                aws_sdk_s3::Client::new(&sdk),
                aws_sdk_ec2::Client::new(&sdk),
            );
            Server::with_injected_client(WriteScope::None, cfg, client)
        };
        let cfg = || {
            crate::config::parse(
                "app.api-prod.namespace = \"N\"\n\
                 app.api-prod.metric.users = \"users|Sum\"\n\
                 app.api-prod.alarm_prefix = \"api-prod-\"\n\
                 app.api-prod.log_group = \"var/log/app.log\"\n",
            )
        };

        let out = server(cfg())
            .call_tool("app_metrics", &json!({"env": "api-prod"}))
            .await
            .expect("answers");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["errors"], json!([]), "{v}");
        assert_eq!(v["metrics"][0]["latest"], 42.0);
        assert_eq!(
            v["alarms"],
            json!([{"name": "api-prod-db", "state": "ALARM", "reason": ""}])
        );
        assert_eq!(v["instances"], json!([]));
        assert_eq!(v["top_errors"], Value::Null);
        assert_eq!(start.num_calls(), 0, "no Insights query unless asked");

        // `alarm_dimensions` edited while the server runs: the alarms are
        // matched by what the file has now, with the same read's prefix.
        let dir = std::env::temp_dir().join(format!(
            "ebman-test-{}-mcp-app-metrics-dims",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "app.api-prod.namespace = \"N\"\n\
             app.api-prod.metric.users = \"users|Sum\"\n\
             app.api-prod.alarm_prefix = \"api-prod-\"\n\
             alarm_dimensions = \"Environment\"\n",
        )
        .unwrap();
        let out = server(cfg())
            .reading_config_from(&path)
            .call_tool("app_metrics", &json!({"env": "api-prod"}))
            .await
            .expect("answers");
        let v: Value = serde_json::from_str(&out).unwrap();
        let mut names: Vec<&str> = v["alarms"]
            .as_array()
            .unwrap_or_else(|| panic!("{v}"))
            .iter()
            .filter_map(|a| a["name"].as_str())
            .collect();
        names.sort_unstable();
        assert_eq!(names, ["api-prod-db", "own-spelling"], "{v}");

        // Asked for, but this env has no log_group: null, and said why.
        let out = server(crate::config::parse("app.api-prod.namespace = \"N\"\n"))
            .call_tool(
                "app_metrics",
                &json!({"env": "api-prod", "top_errors": true}),
            )
            .await
            .expect("answers");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["top_errors"], Value::Null);
        let notes = v["notes"].as_array().cloned().unwrap_or_default();
        assert!(
            notes.iter().any(|n| n
                .as_str()
                .is_some_and(|n| n.contains("no app.ENV.log_group"))),
            "{v}"
        );
        assert_eq!(start.num_calls(), 0);

        let out = server(cfg())
            .call_tool(
                "app_metrics",
                &json!({"env": "api-prod", "top_errors": true}),
            )
            .await
            .expect("answers");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(start.num_calls(), 1);
        assert_eq!(
            v["top_errors"]["log_group"],
            "/aws/elasticbeanstalk/api-prod/var/log/app.log"
        );
        assert_eq!(
            v["top_errors"]["rows"],
            json!([{"fingerprint": "Boom", "count": 3}])
        );
    }

    /// With a prefix, the fleet must be known to keep other envs' alarms
    /// out; if it cannot be listed, the alarms section fails rather than
    /// risk claiming another env's alarms.
    #[tokio::test]
    async fn alarms_fail_rather_than_guess_when_the_fleet_cannot_be_listed() {
        let listing = mock!(aws_sdk_elasticbeanstalk::Client::describe_environments).then_error(|| {
            aws_sdk_elasticbeanstalk::operation::describe_environments::DescribeEnvironmentsError::unhandled(
                "AccessDenied",
            )
        });
        let health = mock!(aws_sdk_elasticbeanstalk::Client::describe_instances_health).then_output(|| {
            aws_sdk_elasticbeanstalk::operation::describe_instances_health::DescribeInstancesHealthOutput::builder().build()
        });
        let alarms = mock!(aws_sdk_cloudwatch::Client::describe_alarms).then_output(|| {
            aws_sdk_cloudwatch::operation::describe_alarms::DescribeAlarmsOutput::builder().build()
        });
        let sdk = aws_config::SdkConfig::builder()
            .region(aws_config::Region::new("us-west-1"))
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build();
        let client = crate::aws::AwsClient::for_tests(
            mock_client!(
                aws_sdk_elasticbeanstalk,
                RuleMode::MatchAny,
                [&listing, &health]
            ),
            aws_sdk_sqs::Client::new(&sdk),
            mock_client!(aws_sdk_cloudwatch, RuleMode::MatchAny, [&alarms]),
            aws_sdk_cloudwatchlogs::Client::new(&sdk),
            aws_sdk_s3::Client::new(&sdk),
            aws_sdk_ec2::Client::new(&sdk),
        );
        let cfg = crate::config::parse("app.e.alarm_prefix = \"e-\"\n");
        let out = Server::with_injected_client(WriteScope::None, cfg, client)
            .call_tool("app_metrics", &json!({"env": "e"}))
            .await
            .expect("the bundle still answers");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["alarms"], Value::Null);
        assert_eq!(v["errors"][0]["section"], "alarms");
        assert!(
            v["errors"][0]["error"]
                .as_str()
                .unwrap_or("")
                .contains("could not list environments"),
            "{v}"
        );
        assert!(
            v["instances"].is_array(),
            "the other sections still came back"
        );
    }

    /// Top errors is bounded inside the call: past 20s the query is
    /// stopped on the server and the section fails, not the bundle.
    #[tokio::test(start_paused = true)]
    async fn a_slow_top_errors_query_fails_its_section_and_is_stopped() {
        use aws_sdk_cloudwatchlogs::types::QueryStatus;
        let health = mock!(aws_sdk_elasticbeanstalk::Client::describe_instances_health).then_output(|| {
            aws_sdk_elasticbeanstalk::operation::describe_instances_health::DescribeInstancesHealthOutput::builder().build()
        });
        let alarms = mock!(aws_sdk_cloudwatch::Client::describe_alarms).then_output(|| {
            aws_sdk_cloudwatch::operation::describe_alarms::DescribeAlarmsOutput::builder().build()
        });
        let start = mock!(aws_sdk_cloudwatchlogs::Client::start_query).then_output(|| {
            aws_sdk_cloudwatchlogs::operation::start_query::StartQueryOutput::builder()
                .query_id("q")
                .build()
        });
        let running = mock!(aws_sdk_cloudwatchlogs::Client::get_query_results).then_output(|| {
            aws_sdk_cloudwatchlogs::operation::get_query_results::GetQueryResultsOutput::builder()
                .status(QueryStatus::Running)
                .build()
        });
        let stop = mock!(aws_sdk_cloudwatchlogs::Client::stop_query).then_output(|| {
            aws_sdk_cloudwatchlogs::operation::stop_query::StopQueryOutput::builder().build()
        });
        let sdk = aws_config::SdkConfig::builder()
            .region(aws_config::Region::new("us-west-1"))
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build();
        let client = crate::aws::AwsClient::for_tests(
            mock_client!(aws_sdk_elasticbeanstalk, RuleMode::MatchAny, [&health]),
            aws_sdk_sqs::Client::new(&sdk),
            mock_client!(aws_sdk_cloudwatch, RuleMode::MatchAny, [&alarms]),
            mock_client!(
                aws_sdk_cloudwatchlogs,
                RuleMode::MatchAny,
                [&start, &running, &stop]
            ),
            aws_sdk_s3::Client::new(&sdk),
            aws_sdk_ec2::Client::new(&sdk),
        );
        let cfg = crate::config::parse("app.e.log_group = \"g\"\n");
        // Virtual time; bounded so an unbounded query fails the test
        // rather than hanging it.
        let server = Server::with_injected_client(WriteScope::None, cfg, client);
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            server.call_tool("app_metrics", &json!({"env": "e", "top_errors": true})),
        )
        .await
        .expect("answered within the tool's own bound")
        .expect("the bundle still answers");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["top_errors"], Value::Null);
        assert_eq!(v["errors"][0]["section"], "top_errors");
        assert!(
            v["errors"][0]["error"]
                .as_str()
                .unwrap_or("")
                .contains("ran past 20s"),
            "{v}"
        );
        assert_eq!(v["alarms"], json!([]), "the other sections came back");
        assert_eq!(stop.num_calls(), 1);
    }

    /// An edit made while the server runs (by hand, or through the
    /// TUI's App tab editor) is what the next call answers for; a file
    /// removed is no config; one unreadable keeps the startup copy.
    #[tokio::test]
    async fn the_app_lines_are_read_as_the_file_has_them_now() {
        let dir =
            std::env::temp_dir().join(format!("ebman-test-{}-mcp-app-lines", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        // The startup copy configures another env, so `e` is configured
        // only when the call read the file.
        let startup = crate::config::parse("app.other.namespace = \"Startup\"\n");
        let server =
            Server::with_config(true, false, WriteScope::None, startup).reading_config_from(&path);
        let namespace = |server: &Server, env: &str| {
            server
                .live_config()
                .app_tabs
                .envs
                .get(env)
                .and_then(|s| s.namespace.clone())
        };

        std::fs::write(&path, "app.e.namespace = \"Edited\"\n").unwrap();
        assert_eq!(namespace(&server, "e").as_deref(), Some("Edited"));
        let out = server
            .call_tool("app_metrics", &json!({"env": "e"}))
            .await
            .expect("answers");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["configured"], true, "the tool reads it too: {v}");

        std::fs::remove_file(&path).unwrap();
        assert_eq!(namespace(&server, "e"), None, "removed: no config");
        assert_eq!(namespace(&server, "other"), None, "not the startup copy");

        let server = server.reading_config_from(&dir.join("config.toml").join("x"));
        std::fs::write(dir.join("config.toml"), "").unwrap();
        assert_eq!(
            namespace(&server, "other").as_deref(),
            Some("Startup"),
            "unreadable: the startup copy"
        );
    }

    #[test]
    fn a_real_server_reads_config_toml_and_a_demo_one_does_not() {
        let real = Server::with_scope(false, false, WriteScope::None);
        assert_eq!(real.config_file, Some(crate::config::config_path()));
        let demo = Server::with_scope(true, false, WriteScope::None);
        assert_eq!(demo.config_file, None, "hermetic");
    }

    #[tokio::test]
    async fn demo_mode_answers_without_aws() {
        let out = Server::with_scope(true, false, WriteScope::None)
            .call_tool(
                "app_metrics",
                &json!({"env": "poly-prod-api", "top_errors": true}),
            )
            .await
            .expect("answers");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["configured"], false);
        assert!(v["instances"].is_array() && v["alarms"].is_array(), "{v}");
        assert_eq!(v["top_errors"], Value::Null);
        assert_eq!(
            v["notes"],
            json!(["top_errors: demo mode makes no Logs Insights call"]),
            "a null asked for is explained"
        );
    }
}
