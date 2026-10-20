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
    /// The line that failed to apply, verbatim; `None` for a check over
    /// several lines (a metric with no namespace). The App tab's editor
    /// writes these back, so saving the form never deletes a line the
    /// operator wrote and the parser could not use.
    pub line: Option<String>,
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

impl AppMetric {
    /// The `app.ENV.metric.LABEL` value that parses back to this metric.
    pub(crate) fn to_value(&self) -> String {
        match &self.unit {
            Some(u) => format!("{}|{}|{u}", self.name, self.stat),
            None => format!("{}|{}", self.name, self.stat),
        }
    }
}

/// The `app.ENV.dimensions` value that parses back to `dims`.
pub(crate) fn dimensions_value(dims: &[(String, String)]) -> String {
    dims.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",")
}

impl Threshold {
    /// The `app.ENV.threshold.LABEL` value that parses back to this.
    pub(crate) fn to_value(self) -> String {
        let lower = if self.lower_is_worse { "<" } else { "" };
        format!("{lower}{},{}", self.warn, self.critical)
    }

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
            self.record_error(rest, message, Some(line));
        }
    }

    /// Attribute `message` to the env named in `rest`, if it names one.
    fn record_error(&mut self, rest: &str, message: String, line: Option<&str>) {
        let (env, _) = split_rest(rest);
        self.errors.push(AppTabError {
            env: Some(env.to_string()).filter(|e| !e.is_empty()),
            message,
            line: line.map(str::to_string),
        });
    }

    /// `env`'s lines that did not apply, verbatim.
    pub(crate) fn rejected_lines<'a>(&'a self, env: &'a str) -> impl Iterator<Item = &'a str> {
        self.errors
            .iter()
            .filter(move |e| e.env.as_deref() == Some(env))
            .filter_map(|e| e.line.as_deref())
    }

    /// Keep `line` for `serialize` but record `message` instead of
    /// applying it: for a line `config::parse` could already tell was
    /// malformed.
    pub(crate) fn reject(&mut self, rest: &str, line: &str, message: String) {
        self.lines.push(line.to_string());
        self.record_error(rest, message, Some(line));
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
        let Some((env, field)) = rest.split_once('.').map(|_| split_rest(rest)) else {
            return Err(format!(
                "app.{rest} is missing a field (expected app.ENV.namespace, \
                 app.ENV.metric.LABEL, …)"
            ));
        };
        let (env, field) = (env.as_str(), field.as_str());
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
                line: None,
            };
            if spec.namespace.is_none() && !spec.metrics.is_empty() {
                self.errors.push(error(format!(
                    "app.{env} defines metrics but no app.{env}.namespace to read them from"
                )));
            }
            let queries = metric_queries(env, spec, 60).len();
            if queries > MAX_QUERIES {
                self.errors.push(error(format!(
                    "app.{env} needs {queries} CloudWatch queries and one call takes \
                     {MAX_QUERIES}: name fewer metrics or resources, or drop \
                     instance_dimension (it adds two per metric)"
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

/// How `config::parse` reads an `app.` line's key, as `(env, field)`:
/// each part trimmed, and a family's label too (`metric. heap` is
/// `metric.heap`). `None` when the parser does not read the line as an
/// `app.` line at all (`app .e.x` is passed through). The one place this
/// is decided: the parser, the editor's save and its rejected-line
/// matching all go through it, so they cannot disagree about which
/// line is whose.
pub(crate) fn key_of(line: &str) -> Option<(String, String)> {
    let (key, _) = line.trim().split_once('=')?;
    Some(split_rest(key.trim().strip_prefix("app.")?))
}

/// `ENV.FIELD` after `app.`, as [`key_of`] describes. EB environment
/// names are letters, digits and hyphens, so the env is everything up
/// to the first dot.
fn split_rest(rest: &str) -> (String, String) {
    let (env, field) = rest.split_once('.').unwrap_or((rest, ""));
    let field = field.trim();
    let field = match field.split_once('.') {
        Some((family @ ("metric" | "threshold"), label)) => format!("{family}.{}", label.trim()),
        _ => field.to_string(),
    };
    (env.trim().to_string(), field)
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
    if escaped {
        // `…\` + the closing `/` reads as an escaped slash, and the
        // regex never ends.
        return Err(format!(
            "{key} ends in a lone backslash, which would escape the end of \
             the Logs Insights regex"
        ));
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
const RESOURCE: char = 'r';

/// `GetMetricData`'s limit on queries in one call. Past it the whole
/// call is refused, so every row would fail together.
pub(crate) const MAX_QUERIES: usize = 500;

/// A linked resource's kind: which namespace and dimension name it, and
/// the fixed metrics read for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResourceKind {
    Rds,
    Cache,
}

/// How a linked metric's value is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ValueUnit {
    Percent,
    Count,
    Bytes,
    Seconds,
}

/// One fixed metric of a linked resource. The first in each kind's list
/// is the row's headline value and sparkline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LinkedMetric {
    pub label: &'static str,
    pub name: &'static str,
    pub stat: &'static str,
    pub unit: ValueUnit,
}

const fn linked(
    label: &'static str,
    name: &'static str,
    stat: &'static str,
    unit: ValueUnit,
) -> LinkedMetric {
    LinkedMetric {
        label,
        name,
        stat,
        unit,
    }
}

const RDS_METRICS: &[LinkedMetric] = &[
    linked("cpu", "CPUUtilization", "Average", ValueUnit::Percent),
    linked("conns", "DatabaseConnections", "Average", ValueUnit::Count),
    linked("free", "FreeableMemory", "Average", ValueUnit::Bytes),
    // Replicas only: a primary returns no series and the row omits it.
    linked("lag", "ReplicaLag", "Average", ValueUnit::Seconds),
];

const CACHE_METRICS: &[LinkedMetric] = &[
    linked(
        "mem",
        "DatabaseMemoryUsagePercentage",
        "Average",
        ValueUnit::Percent,
    ),
    linked("cpu", "EngineCPUUtilization", "Average", ValueUnit::Percent),
    linked("conns", "CurrConnections", "Average", ValueUnit::Count),
    linked("evict", "Evictions", "Sum", ValueUnit::Count),
];

impl ResourceKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Rds => "rds",
            Self::Cache => "cache",
        }
    }

    fn namespace(self) -> &'static str {
        match self {
            Self::Rds => "AWS/RDS",
            Self::Cache => "AWS/ElastiCache",
        }
    }

    fn dimension(self) -> &'static str {
        match self {
            Self::Rds => "DBInstanceIdentifier",
            Self::Cache => "CacheClusterId",
        }
    }

    pub(crate) fn metrics(self) -> &'static [LinkedMetric] {
        match self {
            Self::Rds => RDS_METRICS,
            Self::Cache => CACHE_METRICS,
        }
    }

    /// Said when a named resource returns nothing at all, which is
    /// almost always a name CloudWatch does not know.
    fn no_data_hint(self) -> &'static str {
        match self {
            Self::Rds => "is it a DB instance identifier, not a cluster name?",
            Self::Cache => {
                "is it a node id (e.g. name-001)? Serverless caches are \
                 dimensioned by clusterId and are not read"
            }
        }
    }
}

impl AppTabSpec {
    /// The named resources, RDS first, each in config order.
    pub(crate) fn resources(&self) -> impl Iterator<Item = (ResourceKind, &str)> {
        let rds = self.rds.iter().map(|id| (ResourceKind::Rds, id.as_str()));
        let cache = self
            .elasticache
            .iter()
            .map(|id| (ResourceKind::Cache, id.as_str()));
        rds.chain(cache)
    }

    /// Anything for the tab to fetch.
    pub(crate) fn is_empty(&self) -> bool {
        self.metrics.is_empty() && self.rds.is_empty() && self.elasticache.is_empty()
    }
}

/// The queries for an env's App metrics, in one `GetMetricData` call.
///
/// Per metric:
/// - `a{n}`: the env aggregate, on the configured dimensions.
/// - with `instance_dimension`, `i{n}`: a SEARCH returning one series
///   per instance, and `x{n}`: those combined (SUM, AVG, MAX, MIN by
///   stat), shown when the app publishes per-instance series only and
///   `a{n}` comes back empty. A percentile cannot be combined, so it
///   has no `x{n}`.
///
/// Then, per named resource, `r{k}_{m}`: metric `m` of resource `k` in
/// [`AppTabSpec::resources`] order.
pub(crate) fn metric_queries(env: &str, spec: &AppTabSpec, period: i32) -> Vec<MetricQuery> {
    let mut out = app_queries(env, spec, period);
    for (k, (kind, id)) in spec.resources().enumerate() {
        for (m, metric) in kind.metrics().iter().enumerate() {
            out.push(MetricQuery::Stat {
                id: format!("{RESOURCE}{k}_{m}"),
                namespace: kind.namespace().to_string(),
                metric: metric.name.to_string(),
                stat: metric.stat.to_string(),
                dimensions: vec![(kind.dimension().to_string(), id.to_string())],
            });
        }
    }
    out
}

fn app_queries(env: &str, spec: &AppTabSpec, period: i32) -> Vec<MetricQuery> {
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
    /// One per named resource, in [`AppTabSpec::resources`] order.
    pub resources: Vec<ResourceRow>,
    /// What CloudWatch said about the answer (partial data, a SEARCH
    /// that hit its limit). Shown, so a short answer does not read as a
    /// complete one.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ResourceRow {
    pub kind: ResourceKind,
    pub id: String,
    /// Each of the kind's metrics, in its fixed order, oldest first.
    pub metrics: Vec<(LinkedMetric, Vec<Point>)>,
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
    let mut resources: Vec<ResourceRow> = spec
        .resources()
        .map(|(kind, id)| ResourceRow {
            kind,
            id: id.to_string(),
            metrics: kind.metrics().iter().map(|m| (*m, Vec::new())).collect(),
        })
        .collect();
    // CloudWatch answers every query, if only with an empty series, so
    // no results at all means no call was made (demo mode): there is
    // nothing to diagnose a resource name from.
    let answered = !results.is_empty();
    for mut r in results {
        r.points.sort_by_key(|(t, _)| *t);
        if let Some((k, m)) =
            r.id.strip_prefix(RESOURCE)
                .and_then(|rest| rest.split_once('_'))
                .and_then(|(k, m)| Some((k.parse::<usize>().ok()?, m.parse::<usize>().ok()?)))
        {
            if let Some(slot) = resources.get_mut(k).and_then(|row| row.metrics.get_mut(m)) {
                slot.1 = r.points;
            }
            continue;
        }
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
    let mut notes = notes;
    for row in resources.iter().filter(|_| answered) {
        if row.metrics.iter().all(|(_, points)| points.is_empty()) {
            notes.push(format!(
                "no data for {} {}: {}",
                row.kind.label(),
                row.id,
                row.kind.no_data_hint()
            ));
        }
    }
    AppMetrics {
        series,
        resources,
        notes,
    }
}

/// How far back top errors looks. Fixed rather than the tab's range:
/// Logs Insights bills per GB scanned, and a 24h range would scan a
/// day of logs on every `^R`.
pub(crate) const TOP_ERRORS_WINDOW_MINS: i64 = 15;

/// How long the TUI waits for a top-errors query before stopping it on
/// the server: well past a 15-minute scan of one group, short of
/// leaving `^R` blocked.
pub(crate) const TOP_ERRORS_BOUND: std::time::Duration = std::time::Duration::from_secs(120);

/// Fingerprint groups the query returns. Far more than are shown, so
/// the unmatched group (an empty `fp`) is not cut off by `limit` when
/// there are many distinct fingerprints: rows cost nothing to scan, and
/// the top few are picked here.
const TOP_ERRORS_FETCHED: usize = 1000;

/// Fingerprints shown, most frequent first.
pub(crate) const TOP_ERRORS_SHOWN: usize = 10;

/// Used when `app.ENV.error_fingerprint` is unset: the first hundred
/// characters from `ERROR`, which groups identical messages and keeps
/// distinct ones apart without knowing the app's log format.
const DEFAULT_FINGERPRINT: &str = "(?<fp>ERROR.{0,100})";

/// The Logs Insights query counting ERROR lines by fingerprint, most
/// frequent first. The fingerprint was checked at parse time for its
/// `fp` group and for an unescaped `/`.
pub(crate) fn top_errors_query(fingerprint: Option<&str>) -> String {
    let fp = fingerprint.unwrap_or(DEFAULT_FINGERPRINT);
    format!(
        "fields @message\n\
         | filter @message like /ERROR/\n\
         | parse @message /{fp}/\n\
         | stats count(*) as n by fp\n\
         | sort n desc\n\
         | limit {TOP_ERRORS_FETCHED}"
    )
}

/// `app.ENV.log_group` as a log group name: a full name as given, or a
/// suffix under `/aws/elasticbeanstalk/<env>/`, which is where Elastic
/// Beanstalk streams instance logs.
pub(crate) fn top_errors_log_group(env: &str, configured: &str) -> String {
    if configured.starts_with('/') {
        configured.to_string()
    } else {
        format!(
            "/aws/elasticbeanstalk/{env}/{}",
            configured.trim_start_matches('/')
        )
    }
}

/// The env's most frequent ERROR fingerprints over the last window.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TopErrors {
    pub log_group: String,
    /// `(fingerprint, count)`, most frequent first, at most
    /// [`TOP_ERRORS_SHOWN`].
    pub rows: Vec<(String, u64)>,
    /// Distinct fingerprints past those in `rows`, and the lines they
    /// account for.
    pub more: (usize, u64),
    /// The query hit its own row limit: there may be still more.
    pub truncated: bool,
    /// ERROR lines the fingerprint did not match: counted, not hidden.
    pub unmatched: u64,
    pub records_scanned: i64,
    pub fetched_at: chrono::DateTime<chrono::Utc>,
}

/// A spec's rows with no values, for demo mode: the tab (and the MCP
/// tool) still show what is configured, and say why nothing is there.
pub(crate) fn demo_series(spec: &AppTabSpec) -> AppMetrics {
    series_from_results(
        spec,
        Vec::new(),
        vec!["demo mode makes no CloudWatch calls".into()],
    )
}

/// Read the query's rows. A row with no `fp` is the ERROR lines the
/// fingerprint did not match, which Insights groups under an empty key.
pub(crate) fn top_errors_from(
    log_group: String,
    rows: &[Vec<(String, String)>],
    records_scanned: i64,
    fetched_at: chrono::DateTime<chrono::Utc>,
) -> TopErrors {
    let mut out = TopErrors {
        log_group,
        rows: Vec::new(),
        more: (0, 0),
        truncated: rows.len() >= TOP_ERRORS_FETCHED,
        unmatched: 0,
        records_scanned,
        fetched_at,
    };
    for row in rows {
        let field = |name: &str| {
            row.iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.trim().to_string())
        };
        let Some(n) = field("n").and_then(|n| n.parse::<u64>().ok()) else {
            continue;
        };
        match field("fp").filter(|fp| !fp.is_empty()) {
            Some(fp) => out.rows.push((fp, n)),
            None => out.unmatched += n,
        }
    }
    out.rows
        .sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    if out.rows.len() > TOP_ERRORS_SHOWN {
        let rest = out.rows.split_off(TOP_ERRORS_SHOWN);
        out.more = (rest.len(), rest.iter().map(|(_, n)| n).sum());
    }
    out
}

/// Which alarms belong to an environment: a dimension naming it, or a
/// name starting with the env's `app.ENV.alarm_prefix`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AlarmMatch {
    /// Dimension names that identify an env (`alarm_dimensions`).
    pub dimension_names: Vec<String>,
    pub name_prefix: Option<String>,
    /// Other environments whose names start with the prefix: an alarm
    /// named for one of them is theirs, not this env's. `Shop-prod-`
    /// would otherwise take `Shop-prod-wk-…`, the worker's alarms.
    pub shadowed_by: Vec<String>,
}

impl AlarmMatch {
    /// The match for `env`: the configured dimension names, and that
    /// env's App tab alarm prefix if it has one.
    ///
    /// `fleet` is every environment name ebman knows of, so a prefix
    /// can give way to a longer env name it also covers.
    pub(crate) fn for_env<'a>(
        dimension_names: &[String],
        app_tabs: &crate::app_tab::AppTabs,
        env: &str,
        fleet: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let name_prefix = app_tabs.envs.get(env).and_then(|s| s.alarm_prefix.clone());
        let shadowed_by = match &name_prefix {
            Some(p) if !p.is_empty() => fleet
                .into_iter()
                .filter(|other| *other != env && other.starts_with(p.as_str()))
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        };
        Self {
            dimension_names: dimension_names.to_vec(),
            name_prefix,
            shadowed_by,
        }
    }

    pub(crate) fn matches(
        &self,
        env: &str,
        name: &str,
        dimensions: &[(Option<&str>, Option<&str>)],
    ) -> bool {
        let by_name = self
            .name_prefix
            .as_deref()
            .is_some_and(|p| !p.is_empty() && name.starts_with(p))
            && !self
                .shadowed_by
                .iter()
                .any(|other| alarm_names_env(name, other));
        let by_dimension = dimensions.iter().any(|(n, v)| {
            *v == Some(env) && n.is_some_and(|n| self.dimension_names.iter().any(|w| w == n))
        });
        by_name || by_dimension
    }
}

/// Whether alarm `name` is named for environment `env`: the env's name
/// itself, or it followed by `-`. A bare prefix would take
/// `Shop-prod-wkld-cpu` as the worker `Shop-prod-wk`'s.
pub(crate) fn alarm_names_env(name: &str, env: &str) -> bool {
    name == env
        || name
            .strip_prefix(env)
            .is_some_and(|rest| rest.starts_with('-'))
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
    fn a_key_is_read_as_the_parser_reads_it() {
        let k = |l: &str| key_of(l).map(|(e, f)| format!("{e}|{f}"));
        assert_eq!(k("app.e.namespace = \"N\""), Some("e|namespace".into()));
        assert_eq!(
            k("  app. e .metric. heap = \"X\""),
            Some("e|metric.heap".into())
        );
        assert_eq!(
            k("app.e.threshold.lag=\"1,2\""),
            Some("e|threshold.lag".into())
        );
        assert_eq!(
            k("app .e.namespace = \"N\""),
            None,
            "the parser passes this through"
        );
        assert_eq!(k("theme = \"dark\""), None);
        assert_eq!(
            k("app.e = \"x\""),
            Some("e|".into()),
            "an env with no field"
        );
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
            ("e.error_fingerprint", r"(?<fp>x)\", "lone backslash"),
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
        // And an escaped backslash at the end is two, not a lone one.
        let tabs = parse(&[
            ("e.error_fingerprint", r"at (?<fp>[\w\/.]+)"),
            ("f.error_fingerprint", r"(?<fp>x)\\"),
        ]);
        assert!(tabs.errors.is_empty(), "{:?}", tabs.errors);
    }

    #[test]
    fn a_metric_with_no_namespace_is_reported() {
        let e = one_error(&[("e.metric.x", "X")]);
        assert!(e.contains("no app.e.namespace"), "{e}");
    }

    #[test]
    fn more_queries_than_one_call_takes_is_a_config_error() {
        // 167 metrics with an instance dimension is 501 queries.
        let mut lines = vec![
            ("e.namespace".to_string(), "N".to_string()),
            ("e.instance_dimension".to_string(), "InstanceId".to_string()),
        ];
        for i in 0..167 {
            lines.push((format!("e.metric.m{i}"), "X|Sum".to_string()));
        }
        let refs: Vec<(&str, &str)> = lines
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let e = one_error(&refs);
        assert!(e.contains("needs 501 CloudWatch queries"), "{e}");
        let fits: Vec<(&str, &str)> = refs[..refs.len() - 1].to_vec();
        assert!(parse(&fits).errors.is_empty(), "498 fits");
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
    fn each_named_resource_gets_its_kinds_metrics() {
        let s = spec(&[("e.rds", "db-a, db-b"), ("e.elasticache", "cache-001")]);
        let q = metric_queries("e", &s, 60);
        type Dims = Vec<(String, String)>;
        let stats: Vec<(String, String, String, Dims)> = q
            .into_iter()
            .map(|q| match q {
                MetricQuery::Stat {
                    id,
                    namespace,
                    metric,
                    dimensions,
                    ..
                } => (id, namespace, metric, dimensions),
                other => panic!("a resource query is a plain stat: {other:?}"),
            })
            .collect();
        assert_eq!(
            stats.len(),
            12,
            "four each for db-a, db-b, cache-001: {stats:?}"
        );
        assert_eq!(
            stats[0],
            (
                "r0_0".into(),
                "AWS/RDS".into(),
                "CPUUtilization".into(),
                vec![("DBInstanceIdentifier".into(), "db-a".into())]
            )
        );
        assert_eq!(stats[4].0, "r1_0");
        assert_eq!(
            stats[4].3,
            [("DBInstanceIdentifier".to_string(), "db-b".to_string())]
        );
        assert_eq!(
            stats[8],
            (
                "r2_0".into(),
                "AWS/ElastiCache".into(),
                "DatabaseMemoryUsagePercentage".into(),
                vec![("CacheClusterId".into(), "cache-001".into())]
            )
        );
        assert!(!s.is_empty(), "resources alone are something to show");
        assert!(AppTabSpec::default().is_empty());
    }

    #[test]
    fn resource_results_land_on_their_row_and_metric() {
        let s = spec(&[("e.rds", "db"), ("e.elasticache", "c1, c2")]);
        let m = series_from_results(
            &s,
            vec![
                raw("r0_2", "", &[(2, 9.0), (1, 8.0)]),
                raw("r1_0", "", &[(1, 44.0)]),
                raw("r2_9", "", &[(1, 1.0)]),
                raw("r7_0", "", &[(1, 1.0)]),
                raw("r2_", "", &[(1, 1.0)]),
            ],
            vec![],
        );
        let ids: Vec<&str> = m.resources.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["db", "c1", "c2"]);
        assert_eq!(m.resources[0].metrics[2].0.name, "FreeableMemory");
        assert_eq!(m.resources[0].metrics[2].1, [at(1, 8.0), at(2, 9.0)]);
        assert_eq!(m.resources[1].metrics[0].1, [at(1, 44.0)]);
        // c2 got only out-of-range ids: nothing, and a note saying so.
        assert!(m.resources[2].metrics.iter().all(|(_, p)| p.is_empty()));
        assert_eq!(m.notes.len(), 1, "{:?}", m.notes);
        assert!(
            m.notes[0].starts_with("no data for cache c2: is it a node id"),
            "{:?}",
            m.notes
        );
    }

    #[test]
    fn without_any_answer_no_resource_is_diagnosed() {
        // Demo mode makes no call: an unknown-name hint would be wrong.
        let s = spec(&[("e.rds", "db")]);
        let m = series_from_results(&s, vec![], vec![]);
        assert_eq!(m.resources.len(), 1);
        assert!(m.notes.is_empty(), "{:?}", m.notes);
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
    fn the_top_errors_query_counts_by_fingerprint() {
        assert_eq!(
            top_errors_query(Some(r"ERROR (?<fp>\w+Exception)")),
            "fields @message\n\
             | filter @message like /ERROR/\n\
             | parse @message /ERROR (?<fp>\\w+Exception)/\n\
             | stats count(*) as n by fp\n\
             | sort n desc\n\
             | limit 1000"
        );
        assert!(top_errors_query(None).contains("parse @message /(?<fp>ERROR.{0,100})/"));
    }

    #[test]
    fn a_log_group_is_a_full_name_or_a_suffix_under_the_env() {
        assert_eq!(
            top_errors_log_group("Shop-prod", "var/log/app.log"),
            "/aws/elasticbeanstalk/Shop-prod/var/log/app.log"
        );
        assert_eq!(top_errors_log_group("e", "/custom/group"), "/custom/group");
    }

    #[test]
    fn top_errors_rows_are_read_and_unmatched_lines_counted() {
        let at = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let row = |fields: &[(&str, &str)]| -> Vec<(String, String)> {
            fields
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let t = top_errors_from(
            "g".into(),
            &[
                row(&[("fp", "NPE"), ("n", "4")]),
                row(&[("fp", "PoolTimeout"), ("n", "12")]),
                row(&[("n", "3")]),
                row(&[("fp", ""), ("n", "2")]),
                row(&[("fp", "Odd"), ("n", "x")]),
                row(&[("fp", "Also4"), ("n", "4")]),
            ],
            900,
            at,
        );
        assert_eq!(
            t.rows,
            [
                ("PoolTimeout".to_string(), 12),
                ("Also4".to_string(), 4),
                ("NPE".to_string(), 4)
            ],
            "most frequent first, ties by name, a non-numeric count dropped"
        );
        assert_eq!(t.unmatched, 5);
        assert_eq!((t.records_scanned, t.fetched_at), (900, at));
        assert_eq!((t.more, t.truncated), ((0, 0), false));
    }

    #[test]
    fn many_fingerprints_keep_the_unmatched_count_and_say_how_many_more() {
        // The unmatched group comes back from a query that is not cut to
        // the ten shown, so it is counted however many fingerprints there
        // are; the ones past ten are summarised, not dropped silently.
        let at = chrono::DateTime::from_timestamp(0, 0).unwrap();
        let mut rows: Vec<Vec<(String, String)>> = (1..=14)
            .map(|i| {
                vec![
                    ("fp".to_string(), format!("E{i:02}")),
                    ("n".to_string(), i.to_string()),
                ]
            })
            .collect();
        rows.push(vec![("n".to_string(), "100".to_string())]);
        let t = top_errors_from("g".into(), &rows, 0, at);
        assert_eq!(t.rows.len(), TOP_ERRORS_SHOWN);
        assert_eq!(t.rows[0], ("E14".to_string(), 14));
        assert_eq!(t.more, (4, 1 + 2 + 3 + 4), "E01..E04 past the ten");
        assert_eq!(t.unmatched, 100);
        assert!(!t.truncated);
        let full: Vec<Vec<(String, String)>> = (0..TOP_ERRORS_FETCHED)
            .map(|i| {
                vec![
                    ("fp".to_string(), format!("F{i}")),
                    ("n".to_string(), "1".into()),
                ]
            })
            .collect();
        assert!(
            top_errors_from("g".into(), &full, 0, at).truncated,
            "the query's own limit was hit"
        );
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
