//! CloudWatch metrics and alarms: the per-environment metric series
//! behind the Metrics tab, alarm CRUD, and alarm history.

use super::*;

#[derive(Clone, Debug)]
pub struct CwAlarm {
    pub name: String,
    pub state: String, // OK / ALARM / INSUFFICIENT_DATA
    pub state_reason: String,
    pub metric_name: String,
    pub namespace: String,
}

/// One row in a CloudWatch alarm's recent history, surfaced by
/// `:alarm-history`. `kind` is the API's HistoryItemType string —
/// `StateUpdate` (the transitions an operator usually wants),
/// `ConfigurationUpdate` (someone edited the threshold), or `Action`
/// (the alarm fired its SNS / autoscaling action). `summary` is the
/// short human-readable line CloudWatch emits per item.
#[derive(Clone, Debug, PartialEq)]
pub struct AlarmHistoryEntry {
    pub at: Option<DateTime<Utc>>,
    pub kind: String,
    pub summary: String,
}

#[derive(Clone, Debug, Default)]
pub struct MetricSeries {
    pub id: String,    // stable, e.g. "health"
    pub label: String, // CloudWatch label
    pub points: Vec<(DateTime<Utc>, f64)>,
}

/// One row passed to `fetch_custom_env_metrics`. The shape is wide enough
/// that clippy complains if used inline (`type_complexity` lint), so this
/// alias keeps call-sites tidy.
pub type CustomMetricQuery = (String, String, String, String, Vec<(String, String)>);

/// `chrono` → the CloudWatch SDK's own timestamp type. Second
/// granularity is all the metric APIs accept.
pub(super) fn to_smithy(d: DateTime<Utc>) -> aws_sdk_cloudwatch::primitives::DateTime {
    aws_sdk_cloudwatch::primitives::DateTime::from_secs(d.timestamp())
}

impl AwsClient {
    /// Describe metric alarms whose first dimension references the given env.
    /// CloudWatch doesn't expose a server-side filter by dimension, so we pull
    /// alarms in the AWS/ElasticBeanstalk namespace and filter client-side.
    pub async fn list_alarms_for_env(&self, env_name: &str) -> Result<Vec<CwAlarm>> {
        let mut out = Vec::new();
        let mut next_token: Option<String> = None;
        loop {
            let mut req = self.cw.describe_alarms();
            if let Some(t) = next_token.take() {
                req = req.next_token(t);
            }
            let resp = req.send().await.wrap_err("DescribeAlarms failed")?;
            for a in resp.metric_alarms.unwrap_or_default() {
                let dims = a.dimensions.clone().unwrap_or_default();
                let touches = dims.iter().any(|d| d.value.as_deref() == Some(env_name));
                if !touches {
                    continue;
                }
                out.push(CwAlarm {
                    name: a.alarm_name.unwrap_or_default(),
                    state: a
                        .state_value
                        .map(|s| s.as_str().to_string())
                        .unwrap_or_default(),
                    state_reason: a.state_reason.unwrap_or_default(),
                    metric_name: a.metric_name.unwrap_or_default(),
                    namespace: a.namespace.unwrap_or_default(),
                });
            }
            match resp.next_token {
                Some(t) if !t.is_empty() => next_token = Some(t),
                _ => break,
            }
        }
        Ok(out)
    }

    /// Create or update a CloudWatch metric alarm in the
    /// `AWS/ElasticBeanstalk` namespace, dimensioned by `EnvironmentName`.
    /// `metric_name` should be one of the env-scoped metrics already in our
    /// Metrics tab (EnvironmentHealth / ApplicationRequests4xx /
    /// ApplicationRequests5xx / ApplicationLatencyP90) — anything else and
    /// the alarm will be created with no datapoints. No alarm actions are
    /// attached; operators can wire SNS via the console or CLI later.
    #[allow(clippy::too_many_arguments)]
    pub async fn put_env_metric_alarm(
        &self,
        alarm_name: &str,
        env_name: &str,
        metric_name: &str,
        threshold: f64,
        comparison_operator: &str,
        period_secs: i32,
        evaluation_periods: i32,
        statistic: &str,
    ) -> Result<()> {
        use aws_sdk_cloudwatch::types::{ComparisonOperator, Dimension, Statistic};
        // The smithy enums round-trip "unknown" inputs through their Unknown
        // variant; checking `as_str()` against the original input is the
        // documented way to detect that case without matching on the
        // deprecated variant.
        let op = ComparisonOperator::from(comparison_operator);
        if op.as_str() != comparison_operator {
            return Err(eyre!(
                "unknown comparison operator '{comparison_operator}' \
                 (valid: GreaterThanThreshold, GreaterThanOrEqualToThreshold, \
                 LessThanThreshold, LessThanOrEqualToThreshold)"
            ));
        }
        let stat = Statistic::from(statistic);
        if stat.as_str() != statistic {
            return Err(eyre!(
                "unknown statistic '{statistic}' (valid: Average, Sum, Maximum, Minimum, SampleCount)"
            ));
        }
        let dim = Dimension::builder()
            .name("EnvironmentName")
            .value(env_name)
            .build();
        self.cw
            .put_metric_alarm()
            .alarm_name(alarm_name)
            .alarm_description(format!("ebman: {metric_name} alarm on {env_name}"))
            .namespace("AWS/ElasticBeanstalk")
            .metric_name(metric_name)
            .dimensions(dim)
            .comparison_operator(op)
            .threshold(threshold)
            .period(period_secs)
            .evaluation_periods(evaluation_periods)
            .statistic(stat)
            .treat_missing_data("notBreaching")
            .send()
            .await
            .wrap_err("PutMetricAlarm failed")?;
        Ok(())
    }

    /// Fetch the recent history for a single CloudWatch alarm. Returns
    /// rows newest-first (matches the SDK's default ordering). `kind`
    /// distinguishes StateUpdate / ConfigurationUpdate / Action so the
    /// renderer can colour or filter by entry type. `max_records` caps
    /// the page size — the SDK enforces a server-side max of 100, so
    /// callers wanting more would need to follow the `next_token`
    /// (deferred until anyone needs it).
    pub async fn fetch_alarm_history(
        &self,
        alarm_name: &str,
        max_records: i32,
    ) -> Result<Vec<AlarmHistoryEntry>> {
        let resp = self
            .cw
            .describe_alarm_history()
            .alarm_name(alarm_name)
            .max_records(max_records)
            .send()
            .await
            .wrap_err("DescribeAlarmHistory failed")?;
        let mut out = Vec::new();
        for item in resp.alarm_history_items.unwrap_or_default() {
            let at = item
                .timestamp
                .and_then(|ts| DateTime::<Utc>::from_timestamp(ts.secs(), ts.subsec_nanos()));
            let kind = item
                .history_item_type
                .map(|t| t.as_str().to_string())
                .unwrap_or_else(|| "?".into());
            let summary = item.history_summary.unwrap_or_default();
            out.push(AlarmHistoryEntry { at, kind, summary });
        }
        Ok(out)
    }

    /// Delete one or more CloudWatch alarms by name.
    pub async fn delete_alarms(&self, names: &[String]) -> Result<()> {
        if names.is_empty() {
            return Ok(());
        }
        let mut req = self.cw.delete_alarms();
        for n in names {
            req = req.alarm_names(n);
        }
        req.send().await.wrap_err("DeleteAlarms failed")?;
        Ok(())
    }

    /// Pull a handful of useful EB metrics for one env, from CloudWatch.
    /// Returns an empty Vec for queries the API filtered out.
    pub async fn fetch_env_metrics(
        &self,
        env_name: &str,
        range_secs: i64,
    ) -> Result<Vec<MetricSeries>> {
        use aws_sdk_cloudwatch::types::{Dimension, Metric, MetricDataQuery, MetricStat};

        let end = Utc::now();
        let start = end - chrono::Duration::seconds(range_secs);

        let dim = Dimension::builder()
            .name("EnvironmentName")
            .value(env_name)
            .build();

        let make_query = |id: &str, name: &str, stat: &str| -> MetricDataQuery {
            let metric = Metric::builder()
                .namespace("AWS/ElasticBeanstalk")
                .metric_name(name)
                .dimensions(dim.clone())
                .build();
            let ms = MetricStat::builder()
                .metric(metric)
                .period(60)
                .stat(stat)
                .build();
            MetricDataQuery::builder().id(id).metric_stat(ms).build()
        };

        let resp = self
            .cw
            .get_metric_data()
            .start_time(to_smithy(start))
            .end_time(to_smithy(end))
            .metric_data_queries(make_query("health", "EnvironmentHealth", "Maximum"))
            .metric_data_queries(make_query("req4xx", "ApplicationRequests4xx", "Sum"))
            .metric_data_queries(make_query("req5xx", "ApplicationRequests5xx", "Sum"))
            .metric_data_queries(make_query("p90", "ApplicationLatencyP90", "Average"))
            .send()
            .await?;

        let order = ["health", "req4xx", "req5xx", "p90"];
        let labels: std::collections::HashMap<&str, (&str, &str)> = [
            ("health", ("Env Health (0–25)", "score")),
            ("req4xx", ("4xx Requests / min", "count")),
            ("req5xx", ("5xx Requests / min", "count")),
            ("p90", ("Latency P90", "s")),
        ]
        .into_iter()
        .collect();

        let mut by_id: std::collections::HashMap<String, MetricSeries> =
            std::collections::HashMap::new();
        for r in resp.metric_data_results.unwrap_or_default() {
            let id = r.id.unwrap_or_default();
            let display = labels
                .get(id.as_str())
                .copied()
                .map(|(d, _)| d.to_string())
                .unwrap_or_else(|| id.clone());
            let timestamps = r.timestamps.unwrap_or_default();
            let values = r.values.unwrap_or_default();
            let mut points: Vec<(DateTime<Utc>, f64)> = timestamps
                .iter()
                .zip(values.iter())
                .filter_map(|(ts, v)| {
                    DateTime::<Utc>::from_timestamp(ts.secs(), ts.subsec_nanos()).map(|t| (t, *v))
                })
                .collect();
            points.sort_by_key(|(t, _)| *t);
            by_id.insert(
                id.clone(),
                MetricSeries {
                    id,
                    label: display,
                    points,
                },
            );
        }

        Ok(order.iter().filter_map(|id| by_id.remove(*id)).collect())
    }

    /// Fetch user-defined metric series for one env. Each spec is
    /// `(label, namespace, name, stat, dimensions)` — `dimensions` are
    /// explicit overrides; when empty the call falls back to the env-scoped
    /// `EnvironmentName=env_name` dimension (the common case for
    /// `AWS/ElasticBeanstalk` metrics). Returns the series in the same
    /// order as `specs` so operators see their additions in add-order.
    pub async fn fetch_custom_env_metrics(
        &self,
        env_name: &str,
        range_secs: i64,
        specs: &[CustomMetricQuery],
    ) -> Result<Vec<MetricSeries>> {
        use aws_sdk_cloudwatch::types::{Dimension, Metric, MetricDataQuery, MetricStat};
        if specs.is_empty() {
            return Ok(Vec::new());
        }
        let end = Utc::now();
        let start = end - chrono::Duration::seconds(range_secs);

        let mut req = self
            .cw
            .get_metric_data()
            .start_time(to_smithy(start))
            .end_time(to_smithy(end));
        // CloudWatch's GetMetricData requires the `id` field to be a valid
        // metric reference (lowercase alpha + numeric + underscore, starts
        // with a letter). We use `m{i}` to dodge label-vs-id concerns.
        let mut id_to_label: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for (i, (label, namespace, name, stat, dims)) in specs.iter().enumerate() {
            let id = format!("m{i}");
            let mut metric_builder = Metric::builder().namespace(namespace).metric_name(name);
            if dims.is_empty() {
                metric_builder = metric_builder.dimensions(
                    Dimension::builder()
                        .name("EnvironmentName")
                        .value(env_name)
                        .build(),
                );
            } else {
                for (k, v) in dims {
                    metric_builder =
                        metric_builder.dimensions(Dimension::builder().name(k).value(v).build());
                }
            }
            let ms = MetricStat::builder()
                .metric(metric_builder.build())
                .period(60)
                .stat(stat)
                .build();
            id_to_label.insert(id.clone(), label.clone());
            req =
                req.metric_data_queries(MetricDataQuery::builder().id(id).metric_stat(ms).build());
        }

        let resp = req.send().await?;
        let mut by_id: std::collections::HashMap<String, MetricSeries> =
            std::collections::HashMap::new();
        for r in resp.metric_data_results.unwrap_or_default() {
            let id = r.id.unwrap_or_default();
            let label = id_to_label.get(&id).cloned().unwrap_or_else(|| id.clone());
            let timestamps = r.timestamps.unwrap_or_default();
            let values = r.values.unwrap_or_default();
            let mut points: Vec<(DateTime<Utc>, f64)> = timestamps
                .iter()
                .zip(values.iter())
                .filter_map(|(ts, v)| {
                    DateTime::<Utc>::from_timestamp(ts.secs(), ts.subsec_nanos()).map(|t| (t, *v))
                })
                .collect();
            points.sort_by_key(|(t, _)| *t);
            by_id.insert(id.clone(), MetricSeries { id, label, points });
        }
        // Return in the spec order so operators see the charts in the order
        // they added them.
        Ok((0..specs.len())
            .filter_map(|i| by_id.remove(&format!("m{i}")))
            .collect())
    }
}
