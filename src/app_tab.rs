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
    /// One message per line (or per env, from [`AppTabs::finish`]) that
    /// could not be applied.
    pub errors: Vec<String>,
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

impl AppTabs {
    /// Apply one line. `rest` is the key after `app.`; `line` is the
    /// whole line, kept for `serialize` whether or not it applied.
    pub(crate) fn apply(&mut self, rest: &str, value: &str, line: &str) {
        self.lines.push(line.to_string());
        if let Err(problem) = self.apply_field(rest, value) {
            self.errors.push(problem);
        }
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
            if spec.namespace.is_none() && !spec.metrics.is_empty() {
                self.errors.push(format!(
                    "app.{env} defines metrics but no app.{env}.namespace to read them from"
                ));
            }
            for label in spec.thresholds.keys() {
                if !spec.metrics.iter().any(|m| &m.label == label) {
                    self.errors.push(format!(
                        "app.{env}.threshold.{label} has no app.{env}.metric.{label}"
                    ));
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
        tabs.errors[0].clone()
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
                tabs.errors[0].contains(expect),
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
