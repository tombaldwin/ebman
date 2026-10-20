//! The App tab's per-environment config: `app.ENV.field = "value"`
//! lines in `config.toml`. Design: `docs/design/app-view.md`.
//!
//! Pure. `config::parse` routes every `app.` line here and keeps the
//! line verbatim for `serialize`, so `:settings` never rewrites what the
//! operator wrote.
//!
//! A line that cannot be applied is an ERROR carried to the tab, never
//! skipped: a typo in a metric spec that silently dropped the metric
//! would read as "no data", which is the misread this tab exists to
//! prevent.

use std::collections::BTreeMap;

/// Everything `app.` lines configured, by environment.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct AppTabs {
    pub envs: BTreeMap<String, AppTabSpec>,
    /// Every `app.` line, verbatim and in order, for `serialize`.
    pub lines: Vec<String>,
    /// One per line (or per env, from [`AppTabs::finish`]) that could
    /// not be applied.
    pub errors: Vec<AppTabError>,
}

/// A line that could not be applied, with the env it belongs to so that
/// env's tab can show it. `None` when the line names no env at all
/// (`app..namespace`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AppTabError {
    pub env: Option<String>,
    pub message: String,
}

/// One environment's App tab.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct AppTabSpec {
    pub namespace: Option<String>,
    /// Dimensions selecting this env's series. Empty means
    /// `EnvironmentName=<env>`, the same default `:metric add` uses.
    pub dimensions: Vec<(String, String)>,
    /// The dimension naming an instance, for the per-instance breakdown.
    pub instance_dimension: Option<String>,
    /// In the order first configured; a repeated label replaces in place.
    pub metrics: Vec<AppMetric>,
    pub thresholds: BTreeMap<String, Threshold>,
    /// `DBInstanceIdentifier`s.
    pub rds: Vec<String>,
    /// `CacheClusterId`s.
    pub elasticache: Vec<String>,
    pub alarm_prefix: Option<String>,
    /// A log group name, or a suffix under `/aws/elasticbeanstalk/<env>/`.
    pub log_group: Option<String>,
    /// Logs Insights regex with a named group `fp`.
    pub error_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AppMetric {
    pub label: String,
    pub name: String,
    pub stat: String,
    pub unit: Option<String>,
}

/// Warn and critical levels for one metric label.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Threshold {
    pub warn: f64,
    pub critical: f64,
    /// `"<warn,critical"`: a value BELOW the levels is the bad side
    /// (free memory, active users).
    pub lower_is_worse: bool,
}

/// How a value sits against its [`Threshold`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Level {
    Ok,
    Warn,
    Critical,
}

impl Threshold {
    pub(crate) fn classify(&self, value: f64) -> Level {
        let past = |level: f64| {
            if self.lower_is_worse {
                value <= level
            } else {
                value >= level
            }
        };
        if past(self.critical) {
            Level::Critical
        } else if past(self.warn) {
            Level::Warn
        } else {
            Level::Ok
        }
    }
}

impl AppTabs {
    /// Apply one line. `rest` is the key after `app.`; `line` is the
    /// whole line, kept for `serialize` whether or not it applied.
    pub(crate) fn apply(&mut self, rest: &str, value: &str, line: &str) {
        self.lines.push(line.to_string());
        if let Err(message) = self.apply_field(rest, value) {
            let env = rest.split_once('.').map_or(rest, |(env, _)| env).trim();
            self.errors.push(AppTabError {
                env: Some(env.to_string()).filter(|e| !e.is_empty()),
                message,
            });
        }
    }

    /// The errors to show on `env`'s tab: its own, and any that name no
    /// env (those belong to no tab, so every tab shows them).
    pub(crate) fn errors_for<'a>(&'a self, env: &'a str) -> impl Iterator<Item = &'a str> {
        self.errors
            .iter()
            .filter(move |e| e.env.as_deref().is_none_or(|e| e == env))
            .map(|e| e.message.as_str())
    }

    fn apply_field(&mut self, rest: &str, value: &str) -> Result<(), String> {
        // EB environment names are letters, digits and hyphens, so the
        // env is everything up to the first dot.
        let Some((env, field)) = rest.split_once('.') else {
            return Err(format!(
                "app.{rest} is missing a field (expected app.ENV.namespace, \
                 app.ENV.metric.LABEL, …)"
            ));
        };
        let (env, field) = (env.trim(), field.trim());
        if env.is_empty() {
            return Err(format!("app..{field} has an empty environment name"));
        }
        let key = format!("app.{env}.{field}");
        let required = |what: &str| -> Result<String, String> {
            if value.is_empty() {
                Err(format!("{key} is empty (expected {what})"))
            } else {
                Ok(value.to_string())
            }
        };
        // Applied to a copy and stored only on success, so a rejected
        // line does not leave an empty spec behind for its env.
        let mut spec = self.envs.get(env).cloned().unwrap_or_default();
        match field {
            "namespace" => spec.namespace = Some(required("a CloudWatch namespace")?),
            "dimensions" => spec.dimensions = parse_dimensions(&key, value)?,
            "instance_dimension" => spec.instance_dimension = Some(required("a dimension name")?),
            "rds" => spec.rds = required_list(&key, value, "DB instance identifiers")?,
            "elasticache" => spec.elasticache = required_list(&key, value, "cache cluster ids")?,
            "alarm_prefix" => spec.alarm_prefix = Some(required("an alarm name prefix")?),
            "log_group" => spec.log_group = Some(required("a log group")?),
            "error_fingerprint" => {
                check_fingerprint(&key, value)?;
                spec.error_fingerprint = Some(value.to_string());
            }
            other => {
                if let Some(label) = other.strip_prefix("metric.") {
                    let metric = parse_metric(&key, check_label(&key, label)?, value)?;
                    match spec.metrics.iter_mut().find(|m| m.label == metric.label) {
                        Some(existing) => *existing = metric,
                        None => spec.metrics.push(metric),
                    }
                } else if let Some(label) = other.strip_prefix("threshold.") {
                    let label = check_label(&key, label)?;
                    spec.thresholds
                        .insert(label.to_string(), parse_threshold(&key, value)?);
                } else {
                    return Err(format!(
                        "{key} is not a setting this version understands \
                         (expected one of {})",
                        FIELDS.join(", ")
                    ));
                }
            }
        }
        self.envs.insert(env.to_string(), spec);
        Ok(())
    }

    /// Checks that need every line: a metric with no namespace to read
    /// it from, and a threshold for a label nothing defines.
    pub(crate) fn finish(&mut self) {
        for (env, spec) in &self.envs {
            let error = |message: String| AppTabError {
                env: Some(env.clone()),
                message,
            };
            if spec.namespace.is_none() && !spec.metrics.is_empty() {
                self.errors.push(error(format!(
                    "app.{env} defines metrics but no app.{env}.namespace to read them from"
                )));
            }
            for label in spec.thresholds.keys() {
                if !spec.metrics.iter().any(|m| &m.label == label) {
                    self.errors.push(error(format!(
                        "app.{env}.threshold.{label} has no app.{env}.metric.{label}"
                    )));
                }
            }
        }
    }
}

/// The fields `app.ENV.` accepts, for the error message and the docs
/// guard. `metric.LABEL` and `threshold.LABEL` are families.
pub(crate) const FIELDS: &[&str] = &[
    "namespace",
    "dimensions",
    "instance_dimension",
    "metric.LABEL",
    "threshold.LABEL",
    "rds",
    "elasticache",
    "alarm_prefix",
    "log_group",
    "error_fingerprint",
];

fn check_label<'a>(key: &str, label: &'a str) -> Result<&'a str, String> {
    let label = label.trim();
    if label.is_empty() || label.contains('.') {
        return Err(format!(
            "{key}: a label is one word with no dots (app.ENV.metric.LABEL)"
        ));
    }
    Ok(label)
}

fn required_list(key: &str, value: &str, what: &str) -> Result<Vec<String>, String> {
    let items = crate::util::split_csv(value);
    if items.is_empty() {
        return Err(format!("{key} is empty (expected {what}, comma-separated)"));
    }
    Ok(items)
}

/// `"Name=Value,Name=Value"`.
fn parse_dimensions(key: &str, value: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for pair in crate::util::split_csv(value) {
        match pair.split_once('=') {
            Some((k, v)) if !k.trim().is_empty() && !v.trim().is_empty() => {
                out.push((k.trim().to_string(), v.trim().to_string()));
            }
            _ => {
                return Err(format!(
                    "{key}: {pair:?} is not Name=Value (expected \"Name=Value,Name=Value\")"
                ))
            }
        }
    }
    if out.is_empty() {
        return Err(format!("{key} is empty (expected \"Name=Value,…\")"));
    }
    Ok(out)
}

/// `"MetricName|stat|unit"`; stat and unit are optional.
fn parse_metric(key: &str, label: &str, value: &str) -> Result<AppMetric, String> {
    let mut parts = value.split('|').map(str::trim);
    let name = parts.next().unwrap_or_default();
    if name.is_empty() {
        return Err(format!(
            "{key} has no metric name (expected \"MetricName|stat|unit\")"
        ));
    }
    let stat = match parts.next() {
        None | Some("") => "Average".to_string(),
        Some(s) if is_statistic(s) => s.to_string(),
        Some(s) => {
            return Err(format!(
                "{key}: {s:?} is not a CloudWatch statistic (expected Average, Sum, \
                 Minimum, Maximum, SampleCount or a percentile like p99)"
            ))
        }
    };
    let unit = parts.next().filter(|u| !u.is_empty()).map(str::to_string);
    if parts.next().is_some() {
        return Err(format!(
            "{key} has more than three parts (expected \"MetricName|stat|unit\")"
        ));
    }
    Ok(AppMetric {
        label: label.to_string(),
        name: name.to_string(),
        stat,
        unit,
    })
}

/// The statistics a metric line may name. A typo here would otherwise
/// reach CloudWatch and come back as an error on every fetch.
fn is_statistic(s: &str) -> bool {
    if ["Average", "Sum", "Minimum", "Maximum", "SampleCount"].contains(&s) {
        return true;
    }
    // Percentiles: p50, p99, p99.9.
    let Some(n) = s.strip_prefix('p') else {
        return false;
    };
    match n.split_once('.') {
        Some((whole, frac)) => is_digits(whole) && is_digits(frac),
        None => is_digits(n),
    }
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// `"warn,critical"`, or `"<warn,critical"` when lower is worse.
fn parse_threshold(key: &str, value: &str) -> Result<Threshold, String> {
    let (lower_is_worse, levels) = match value.trim().strip_prefix('<') {
        Some(rest) => (true, rest),
        None => (false, value.trim()),
    };
    let bad = || {
        format!(
            "{key} = {value:?} is not \"warn,critical\" (or \"<warn,critical\" \
             when lower is worse)"
        )
    };
    let (warn, critical) = levels.split_once(',').ok_or_else(bad)?;
    let warn: f64 = warn.trim().parse().map_err(|_| bad())?;
    let critical: f64 = critical.trim().parse().map_err(|_| bad())?;
    if !warn.is_finite() || !critical.is_finite() {
        return Err(bad());
    }
    let ordered = if lower_is_worse {
        critical <= warn
    } else {
        warn <= critical
    };
    if !ordered {
        return Err(format!(
            "{key}: critical ({critical}) is on the safe side of warn ({warn}); \
             for lower-is-worse, write \"<{warn},{critical}\""
        ));
    }
    Ok(Threshold {
        warn,
        critical,
        lower_is_worse,
    })
}

/// The fingerprint goes into a Logs Insights `parse @message /…/`, so
/// it needs the `fp` group the query counts by, and a bare `/` would
/// end the regex early.
fn check_fingerprint(key: &str, value: &str) -> Result<(), String> {
    if !value.contains("(?<fp>") {
        return Err(format!(
            "{key} has no named group fp, e.g. \"ERROR (?<fp>\\w+Exception)\""
        ));
    }
    let mut escaped = false;
    for c in value.chars() {
        match c {
            '\\' => escaped = !escaped,
            '/' if !escaped => {
                return Err(format!(
                    "{key} contains an unescaped / (write \\/), which would end \
                     the Logs Insights regex early"
                ))
            }
            _ => escaped = false,
        }
    }
    Ok(())
}

/// One `GetMetricData` query, before it becomes an SDK type: pure, so
/// the ids, dimensions and SEARCH text are testable without a client.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MetricQuery {
    Stat {
        id: String,
        namespace: String,
        metric: String,
        stat: String,
        dimensions: Vec<(String, String)>,
    },
    Expression {
        id: String,
        expression: String,
        /// A dynamic label, so each SEARCH result names its instance.
        label: Option<String>,
    },
}

/// The ids `GetMetricData` results come back under, one family per
/// query kind; the number is the metric's index in the spec.
const AGGREGATE: char = 'a';
const PER_INSTANCE: char = 'i';
const COMBINED: char = 'x';

/// The queries for an env's App metrics, in one `GetMetricData` call.
///
/// Per metric:
/// - `a{n}`: the env aggregate, on the configured dimensions.
/// - with `instance_dimension`, `i{n}`: a SEARCH returning one series
///   per instance, and `x{n}`: those combined (SUM, AVG, MAX, MIN by
///   stat), shown when the app publishes per-instance series only and
///   `a{n}` comes back empty. A percentile cannot be combined, so it
///   has no `x{n}`.
pub(crate) fn metric_queries(env: &str, spec: &AppTabSpec, period: i32) -> Vec<MetricQuery> {
    let Some(namespace) = &spec.namespace else {
        return Vec::new();
    };
    let dimensions = env_dimensions(env, spec);
    let mut out = Vec::new();
    for (n, m) in spec.metrics.iter().enumerate() {
        out.push(MetricQuery::Stat {
            id: format!("{AGGREGATE}{n}"),
            namespace: namespace.clone(),
            metric: m.name.clone(),
            stat: m.stat.clone(),
            dimensions: dimensions.clone(),
        });
        let Some(instance) = &spec.instance_dimension else {
            continue;
        };
        let Some(search) =
            search_expression(namespace, &dimensions, instance, &m.name, &m.stat, period)
        else {
            continue;
        };
        out.push(MetricQuery::Expression {
            id: format!("{PER_INSTANCE}{n}"),
            expression: search,
            label: Some(format!("${{PROP('Dim.{instance}')}}")),
        });
        if let Some(combine) = combiner(&m.stat) {
            out.push(MetricQuery::Expression {
                id: format!("{COMBINED}{n}"),
                expression: format!("{combine}({PER_INSTANCE}{n})"),
                label: None,
            });
        }
    }
    out
}

/// The dimensions selecting `env`'s series: configured, or
/// `EnvironmentName=<env>`.
pub(crate) fn env_dimensions(env: &str, spec: &AppTabSpec) -> Vec<(String, String)> {
    if spec.dimensions.is_empty() {
        vec![(crate::aws::ENV_DIMENSION.to_string(), env.to_string())]
    } else {
        spec.dimensions.clone()
    }
}

fn combiner(stat: &str) -> Option<&'static str> {
    match stat {
        "Sum" | "SampleCount" => Some("SUM"),
        "Average" => Some("AVG"),
        "Maximum" => Some("MAX"),
        "Minimum" => Some("MIN"),
        _ => None,
    }
}

/// `SEARCH('{NS,Dim,…,Instance} Dim="v" MetricName="m"', 'stat', P)`.
///
/// The schema in braces lists EVERY dimension name, which is what keeps
/// the env aggregate (no instance dimension) out of the per-instance
/// results. Each term is double-quoted with `\` and `"` escaped, which
/// is CloudWatch's exact match. `None` when a part contains `'`: the
/// search string sits inside single quotes, and a per-instance view is
/// not worth guessing an escape for.
pub(crate) fn search_expression(
    namespace: &str,
    dimensions: &[(String, String)],
    instance_dimension: &str,
    metric: &str,
    stat: &str,
    period: i32,
) -> Option<String> {
    let parts = std::iter::once(namespace)
        .chain(
            dimensions
                .iter()
                .flat_map(|(k, v)| [k.as_str(), v.as_str()]),
        )
        .chain([instance_dimension, metric, stat]);
    if parts.into_iter().any(|p| p.contains('\'')) {
        return None;
    }
    let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let schema: Vec<String> = std::iter::once(namespace)
        .chain(dimensions.iter().map(|(k, _)| k.as_str()))
        .chain([instance_dimension])
        .map(q)
        .collect();
    let mut terms: Vec<String> = dimensions
        .iter()
        .map(|(k, v)| format!("{}={}", q(k), q(v)))
        .collect();
    terms.push(format!("MetricName={}", q(metric)));
    Some(format!(
        "SEARCH('{{{}}} {}', '{stat}', {period})",
        schema.join(","),
        terms.join(" ")
    ))
}

/// A timestamped value, as CloudWatch returns them.
pub(crate) type Point = (chrono::DateTime<chrono::Utc>, f64);

/// One `GetMetricData` result, before it is matched to a metric.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawSeries {
    pub id: String,
    pub label: String,
    pub points: Vec<Point>,
}

/// An env's App metrics, ready to draw.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct AppMetrics {
    /// One per configured metric, in config order.
    pub series: Vec<AppSeries>,
    /// What CloudWatch said about the answer (partial data, a SEARCH
    /// that hit its limit). Shown, so a short answer does not read as a
    /// complete one.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AppSeries {
    pub label: String,
    pub unit: Option<String>,
    pub threshold: Option<Threshold>,
    /// The env aggregate, oldest first; the per-instance combination
    /// when the app publishes no aggregate.
    pub points: Vec<Point>,
    /// One per instance, by instance id, each oldest first.
    pub instances: Vec<(String, Vec<Point>)>,
}

/// Match results back to the spec's metrics by id.
pub(crate) fn series_from_results(
    spec: &AppTabSpec,
    results: Vec<RawSeries>,
    notes: Vec<String>,
) -> AppMetrics {
    let mut series: Vec<AppSeries> = spec
        .metrics
        .iter()
        .map(|m| AppSeries {
            label: m.label.clone(),
            unit: m.unit.clone(),
            threshold: spec.thresholds.get(&m.label).copied(),
            points: Vec::new(),
            instances: Vec::new(),
        })
        .collect();
    let mut combined: Vec<Vec<Point>> = vec![Vec::new(); series.len()];
    for mut r in results {
        let mut chars = r.id.chars();
        let kind = chars.next();
        let Some(n) = chars
            .as_str()
            .parse::<usize>()
            .ok()
            .filter(|n| *n < series.len())
        else {
            continue;
        };
        r.points.sort_by_key(|(t, _)| *t);
        match kind {
            Some(AGGREGATE) => series[n].points = r.points,
            Some(PER_INSTANCE) if !r.points.is_empty() => {
                series[n].instances.push((r.label, r.points))
            }
            Some(COMBINED) => combined[n] = r.points,
            _ => {}
        }
    }
    for (s, c) in series.iter_mut().zip(combined) {
        if s.points.is_empty() {
            s.points = c;
        }
        s.instances.sort_by(|a, b| a.0.cmp(&b.0));
    }
    AppMetrics { series, notes }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(lines: &[(&str, &str)]) -> AppTabs {
        let mut tabs = AppTabs::default();
        for (rest, value) in lines {
            tabs.apply(rest, value, &format!("app.{rest} = \"{value}\""));
        }
        tabs.finish();
        tabs
    }

    fn one_error(lines: &[(&str, &str)]) -> String {
        let tabs = parse(lines);
        assert_eq!(tabs.errors.len(), 1, "{:?}", tabs.errors);
        tabs.errors[0].message.clone()
    }

    #[test]
    fn a_full_spec_parses() {
        let tabs = parse(&[
            ("Shop-prod.namespace", "Shop"),
            ("Shop-prod.dimensions", "Environment=prod, Tier=web"),
            ("Shop-prod.instance_dimension", "InstanceId"),
            ("Shop-prod.metric.users", "usersActive|Maximum|count"),
            ("Shop-prod.metric.heap", "heapUsedMb"),
            ("Shop-prod.threshold.heap", "1500,1800"),
            ("Shop-prod.rds", "shop-db, shop-db-replica"),
            ("Shop-prod.elasticache", "shop-cache"),
            ("Shop-prod.alarm_prefix", "Shop-prod-"),
            ("Shop-prod.log_group", "var/log/app.log"),
            ("Shop-prod.error_fingerprint", r"ERROR (?<fp>\w+Exception)"),
        ]);
        assert!(tabs.errors.is_empty(), "{:?}", tabs.errors);
        let s = &tabs.envs["Shop-prod"];
        assert_eq!(s.namespace.as_deref(), Some("Shop"));
        assert_eq!(
            s.dimensions,
            [
                ("Environment".into(), "prod".into()),
                ("Tier".into(), "web".into())
            ]
        );
        assert_eq!(s.instance_dimension.as_deref(), Some("InstanceId"));
        assert_eq!(
            s.metrics,
            [
                AppMetric {
                    label: "users".into(),
                    name: "usersActive".into(),
                    stat: "Maximum".into(),
                    unit: Some("count".into()),
                },
                AppMetric {
                    label: "heap".into(),
                    name: "heapUsedMb".into(),
                    stat: "Average".into(),
                    unit: None,
                },
            ]
        );
        assert_eq!(
            s.thresholds["heap"],
            Threshold {
                warn: 1500.0,
                critical: 1800.0,
                lower_is_worse: false
            }
        );
        assert_eq!(s.rds, ["shop-db", "shop-db-replica"]);
        assert_eq!(s.elasticache, ["shop-cache"]);
        assert_eq!(s.alarm_prefix.as_deref(), Some("Shop-prod-"));
        assert_eq!(s.log_group.as_deref(), Some("var/log/app.log"));
        assert_eq!(
            s.error_fingerprint.as_deref(),
            Some(r"ERROR (?<fp>\w+Exception)")
        );
        assert_eq!(tabs.lines.len(), 11, "every line is kept for serialize");
    }

    #[test]
    fn envs_are_kept_apart() {
        let tabs = parse(&[("a.namespace", "A"), ("b.namespace", "B")]);
        assert_eq!(tabs.envs["a"].namespace.as_deref(), Some("A"));
        assert_eq!(tabs.envs["b"].namespace.as_deref(), Some("B"));
    }

    #[test]
    fn a_repeated_metric_label_replaces_in_place() {
        let tabs = parse(&[
            ("e.namespace", "N"),
            ("e.metric.a", "A1"),
            ("e.metric.b", "B"),
            ("e.metric.a", "A2|Sum"),
        ]);
        let names: Vec<_> = tabs.envs["e"]
            .metrics
            .iter()
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(names, ["A2", "B"]);
    }

    #[test]
    fn percentile_statistics_are_accepted() {
        for stat in ["p50", "p99", "p99.9"] {
            let tabs = parse(&[("e.namespace", "N"), ("e.metric.x", &format!("X|{stat}"))]);
            assert!(tabs.errors.is_empty(), "{stat}: {:?}", tabs.errors);
        }
    }

    #[test]
    fn each_malformed_line_is_an_error_not_a_skip() {
        // One case per rule: each must produce exactly one error naming
        // the key, and the line must still be kept.
        let cases: &[(&str, &str, &str)] = &[
            ("e", "N", "missing a field"),
            (".namespace", "N", "empty environment name"),
            ("e.namespace", "", "is empty"),
            ("e.instance_dimension", "", "is empty"),
            ("e.alarm_prefix", "", "is empty"),
            ("e.log_group", "", "is empty"),
            ("e.rds", " , ", "is empty"),
            ("e.elasticache", "", "is empty"),
            ("e.dimensions", "Environment", "is not Name=Value"),
            ("e.dimensions", "=prod", "is not Name=Value"),
            ("e.dimensions", "", "is empty"),
            ("e.metric.", "X", "a label is one word"),
            ("e.metric.a.b", "X", "a label is one word"),
            ("e.metric.x", "", "no metric name"),
            ("e.metric.x", "X|Avg", "not a CloudWatch statistic"),
            ("e.metric.x", "X|p", "not a CloudWatch statistic"),
            ("e.metric.x", "X|p9.", "not a CloudWatch statistic"),
            ("e.metric.x", "X|Sum|ms|extra", "more than three parts"),
            ("e.threshold.x", "5", "is not \"warn,critical\""),
            ("e.threshold.x", "a,b", "is not \"warn,critical\""),
            ("e.threshold.x", "inf,9", "is not \"warn,critical\""),
            ("e.threshold.x", "9,5", "on the safe side"),
            ("e.threshold.x", "<5,9", "on the safe side"),
            ("e.error_fingerprint", "ERROR (\\w+)", "no named group fp"),
            ("e.error_fingerprint", "a/(?<fp>b)", "unescaped /"),
            // An escaped backslash does not escape the slash after it.
            ("e.error_fingerprint", r"a\\/(?<fp>b)", "unescaped /"),
            ("e.namspace", "N", "not a setting this version understands"),
        ];
        for (rest, value, expect) in cases {
            // No `finish`: these are the per-line rules.
            let mut tabs = AppTabs::default();
            tabs.apply(rest, value, "the line");
            assert_eq!(
                tabs.errors.len(),
                1,
                "app.{rest} = {value:?}: {:?}",
                tabs.errors
            );
            assert!(
                tabs.errors[0].message.contains(expect),
                "app.{rest} = {value:?}: {:?} should say {expect:?}",
                tabs.errors[0]
            );
            assert_eq!(tabs.lines, ["the line"], "kept for serialize");
            assert!(
                tabs.envs.is_empty(),
                "app.{rest}: a rejected line configures nothing"
            );
        }
    }

    #[test]
    fn each_tab_shows_its_own_errors_and_the_unattributable_ones() {
        let tabs = parse(&[
            ("a.namspace", "N"),
            ("b.metric.x", "X"),
            (".namespace", "N"),
            ("c", "N"),
        ]);
        let for_a: Vec<_> = tabs.errors_for("a").collect();
        assert_eq!(for_a.len(), 2, "{for_a:?}");
        assert!(for_a[0].contains("app.a.namspace"), "{for_a:?}");
        assert!(for_a[1].contains("empty environment name"), "{for_a:?}");
        // `b`'s error is from `finish`; `c` named an env with no field.
        assert!(tabs
            .errors_for("b")
            .any(|e| e.contains("no app.b.namespace")));
        assert!(tabs.errors_for("c").any(|e| e.contains("missing a field")));
        assert!(!tabs
            .errors_for("a")
            .any(|e| e.contains("app.b") || e.contains("app.c")));
    }

    #[test]
    fn an_escaped_slash_in_a_fingerprint_is_fine() {
        let tabs = parse(&[("e.error_fingerprint", r"at (?<fp>[\w\/.]+)")]);
        assert!(tabs.errors.is_empty(), "{:?}", tabs.errors);
    }

    #[test]
    fn a_metric_with_no_namespace_is_reported() {
        let e = one_error(&[("e.metric.x", "X")]);
        assert!(e.contains("no app.e.namespace"), "{e}");
    }

    #[test]
    fn a_threshold_for_an_undefined_label_is_reported() {
        let e = one_error(&[
            ("e.namespace", "N"),
            ("e.metric.x", "X"),
            ("e.threshold.y", "1,2"),
        ]);
        assert!(e.contains("threshold.y has no app.e.metric.y"), "{e}");
    }

    #[test]
    fn thresholds_classify_in_both_directions() {
        let up = Threshold {
            warn: 10.0,
            critical: 20.0,
            lower_is_worse: false,
        };
        assert_eq!(up.classify(9.9), Level::Ok);
        assert_eq!(up.classify(10.0), Level::Warn);
        assert_eq!(up.classify(19.9), Level::Warn);
        assert_eq!(up.classify(20.0), Level::Critical);
        let down = Threshold {
            warn: 20.0,
            critical: 10.0,
            lower_is_worse: true,
        };
        assert_eq!(down.classify(20.1), Level::Ok);
        assert_eq!(down.classify(20.0), Level::Warn);
        assert_eq!(down.classify(10.0), Level::Critical);
        assert_eq!(down.classify(3.0), Level::Critical);
    }

    fn spec(lines: &[(&str, &str)]) -> AppTabSpec {
        let tabs = parse(lines);
        assert!(tabs.errors.is_empty(), "{:?}", tabs.errors);
        tabs.envs.into_values().next().expect("one env")
    }

    #[test]
    fn without_an_instance_dimension_each_metric_is_one_aggregate_query() {
        let s = spec(&[
            ("e.namespace", "Shop"),
            ("e.metric.u", "users|Maximum"),
            ("e.metric.h", "heap"),
        ]);
        assert_eq!(
            metric_queries("Shop-prod", &s, 60),
            [
                MetricQuery::Stat {
                    id: "a0".into(),
                    namespace: "Shop".into(),
                    metric: "users".into(),
                    stat: "Maximum".into(),
                    dimensions: vec![("EnvironmentName".into(), "Shop-prod".into())],
                },
                MetricQuery::Stat {
                    id: "a1".into(),
                    namespace: "Shop".into(),
                    metric: "heap".into(),
                    stat: "Average".into(),
                    dimensions: vec![("EnvironmentName".into(), "Shop-prod".into())],
                },
            ]
        );
    }

    #[test]
    fn no_namespace_means_no_queries() {
        let mut s = AppTabSpec::default();
        s.metrics.push(AppMetric {
            label: "x".into(),
            name: "X".into(),
            stat: "Sum".into(),
            unit: None,
        });
        assert_eq!(metric_queries("e", &s, 60), []);
    }

    #[test]
    fn an_instance_dimension_adds_a_search_and_its_combination() {
        let s = spec(&[
            ("e.namespace", "Shop"),
            ("e.dimensions", "Environment=prod"),
            ("e.instance_dimension", "InstanceId"),
            ("e.metric.u", "users|Sum"),
            ("e.metric.l", "latency|p99"),
        ]);
        let q = metric_queries("e", &s, 300);
        let ids: Vec<&str> = q
            .iter()
            .map(|q| match q {
                MetricQuery::Stat { id, .. } | MetricQuery::Expression { id, .. } => id.as_str(),
            })
            .collect();
        assert_eq!(
            ids,
            ["a0", "i0", "x0", "a1", "i1"],
            "a percentile has no combination"
        );
        assert_eq!(
            q[1],
            MetricQuery::Expression {
                id: "i0".into(),
                expression: r#"SEARCH('{"Shop","Environment","InstanceId"} "Environment"="prod" MetricName="users"', 'Sum', 300)"#.into(),
                label: Some("${PROP('Dim.InstanceId')}".into()),
            }
        );
        assert_eq!(
            q[2],
            MetricQuery::Expression {
                id: "x0".into(),
                expression: "SUM(i0)".into(),
                label: None,
            }
        );
    }

    #[test]
    fn each_statistic_combines_its_own_way() {
        assert_eq!(combiner("Sum"), Some("SUM"));
        assert_eq!(combiner("SampleCount"), Some("SUM"));
        assert_eq!(combiner("Average"), Some("AVG"));
        assert_eq!(combiner("Maximum"), Some("MAX"));
        assert_eq!(combiner("Minimum"), Some("MIN"));
        assert_eq!(combiner("p99"), None);
    }

    #[test]
    fn search_terms_are_quoted_and_escaped() {
        let e = search_expression(
            "My App",
            &[("Env".into(), r#"a"b\c"#.into())],
            "Host",
            "m",
            "Sum",
            60,
        )
        .unwrap();
        assert_eq!(
            e,
            r#"SEARCH('{"My App","Env","Host"} "Env"="a\"b\\c" MetricName="m"', 'Sum', 60)"#
        );
    }

    #[test]
    fn a_single_quote_anywhere_drops_the_per_instance_search() {
        let dims = [("Env".to_string(), "prod".to_string())];
        for (ns, dim, inst, metric) in [
            ("N's", "prod", "Host", "m"),
            ("N", "pr'od", "Host", "m"),
            ("N", "prod", "Ho'st", "m"),
            ("N", "prod", "Host", "m'"),
        ] {
            let dims = [("Env".to_string(), dim.to_string())];
            assert_eq!(
                search_expression(ns, &dims, inst, metric, "Sum", 60),
                None,
                "{ns} {dim} {inst} {metric}"
            );
        }
        assert!(search_expression("N", &dims, "Host", "m", "Sum", 60).is_some());
    }

    fn at(secs: i64, v: f64) -> Point {
        (chrono::DateTime::from_timestamp(secs, 0).unwrap(), v)
    }

    fn raw(id: &str, label: &str, points: &[(i64, f64)]) -> RawSeries {
        RawSeries {
            id: id.into(),
            label: label.into(),
            points: points.iter().map(|(t, v)| at(*t, *v)).collect(),
        }
    }

    #[test]
    fn results_are_matched_to_their_metric_by_id() {
        let s = spec(&[
            ("e.namespace", "N"),
            ("e.instance_dimension", "InstanceId"),
            ("e.metric.u", "users|Sum|count"),
            ("e.metric.h", "heap|Average|MB"),
            ("e.threshold.h", "1,2"),
        ]);
        let m = series_from_results(
            &s,
            vec![
                raw("i1", "i-b", &[(2, 5.0), (1, 4.0)]),
                raw("a1", "", &[(2, 9.0), (1, 8.0)]),
                raw("i1", "i-a", &[(1, 3.0)]),
                raw("i1", "i-empty", &[]),
                raw("x0", "", &[(1, 7.0)]),
                raw("a7", "", &[(1, 1.0)]),
                raw("zz", "", &[(1, 1.0)]),
            ],
            vec!["note".into()],
        );
        assert_eq!(m.notes, ["note"]);
        let [u, h] = &m.series[..] else {
            panic!("{m:?}")
        };
        assert_eq!(
            (u.label.as_str(), u.unit.as_deref(), u.threshold),
            ("u", Some("count"), None)
        );
        assert_eq!(
            u.points,
            [at(1, 7.0)],
            "no aggregate: the combination stands in"
        );
        assert_eq!(h.points, [at(1, 8.0), at(2, 9.0)], "oldest first");
        assert_eq!(h.threshold.map(|t| t.critical), Some(2.0));
        let instances: Vec<&str> = h.instances.iter().map(|(i, _)| i.as_str()).collect();
        assert_eq!(
            instances,
            ["i-a", "i-b"],
            "sorted, and an empty series is dropped"
        );
        assert_eq!(h.instances[1].1, [at(1, 4.0), at(2, 5.0)]);
    }

    #[test]
    fn an_aggregate_wins_over_the_combination() {
        let s = spec(&[("e.namespace", "N"), ("e.metric.u", "users|Sum")]);
        let m = series_from_results(
            &s,
            vec![raw("x0", "", &[(1, 7.0)]), raw("a0", "", &[(1, 2.0)])],
            vec![],
        );
        assert_eq!(m.series[0].points, [at(1, 2.0)]);
    }

    #[test]
    fn a_lower_is_worse_threshold_parses() {
        let tabs = parse(&[
            ("e.namespace", "N"),
            ("e.metric.free", "Free"),
            ("e.threshold.free", "<500, 100"),
        ]);
        assert!(tabs.errors.is_empty(), "{:?}", tabs.errors);
        assert!(tabs.envs["e"].thresholds["free"].lower_is_worse);
    }
}
