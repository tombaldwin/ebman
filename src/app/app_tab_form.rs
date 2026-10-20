//! The App tab's editor: `e` on the App tab opens a form over the env's
//! `app.ENV.*` config, pre-filled with what discovery finds, and saving
//! writes that env's lines to `config.toml` and applies them at once.
//!
//! Discovery suggests; it never decides. A field the config already
//! sets keeps its value, and an RDS instance is only ever pre-selected
//! when one of the env's own variables names its endpoint: the rest
//! are offered, not chosen.

use super::*;
use crate::app_discover::Discovery;
use crate::app_tab::AppTabSpec;
use crate::form::{FieldKind, FormField};

/// The `log_group` option meaning "no top errors".
const NO_LOG_GROUP: &str = "(none)";

/// Separates the entries of the metrics and thresholds fields.
const ENTRY_SEP: char = ';';

/// What discovery found, and which of its lookups failed.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Discovered {
    pub discovery: Discovery,
    /// `(what, why)` for each lookup that failed: the form says so,
    /// rather than offering fewer options as if that were all there was.
    pub failed: Vec<(String, String)>,
}

/// The form's fields, filled from `spec` (the env's config, if any).
pub(crate) fn fields(spec: Option<&AppTabSpec>) -> Vec<FormField> {
    let spec = spec.cloned().unwrap_or_default();
    let text = |key: &str, label: &str, value: String, help: &str| {
        let mut f = FormField::text(key, label, Some(help));
        f.value = value;
        f
    };
    let metrics: Vec<String> = spec
        .metrics
        .iter()
        .map(|m| match &m.unit {
            Some(u) => format!("{}={}|{}|{u}", m.label, m.name, m.stat),
            None => format!("{}={}|{}", m.label, m.name, m.stat),
        })
        .collect();
    let thresholds: Vec<String> = spec
        .thresholds
        .iter()
        .map(|(label, t)| {
            let lower = if t.lower_is_worse { "<" } else { "" };
            format!("{label}={lower}{},{}", t.warn, t.critical)
        })
        .collect();
    let dims: Vec<String> = spec
        .dimensions
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    let sep = format!("{ENTRY_SEP} ");
    let mut log_group = FormField::select(
        "log_group",
        "Log group (top errors)",
        vec![NO_LOG_GROUP.to_string()],
        Some("under /aws/elasticbeanstalk/<env>/; turns on the Logs Insights top-errors count"),
    );
    if let Some(g) = &spec.log_group {
        log_group.kind = FieldKind::Select {
            options: vec![NO_LOG_GROUP.to_string(), g.clone()],
        };
        log_group.value = g.clone();
    }
    vec![
        text(
            "namespace",
            "Namespace",
            spec.namespace.unwrap_or_default(),
            "CloudWatch namespace your app publishes to",
        ),
        text(
            "dimensions",
            "Dimensions",
            dims.join(","),
            "Name=Value,… selecting this env (default EnvironmentName=<env>)",
        ),
        text(
            "instance_dimension",
            "Instance dimension",
            spec.instance_dimension.unwrap_or_default(),
            "dimension naming an instance, e.g. InstanceId",
        ),
        text(
            "metrics",
            "Metrics",
            metrics.join(&sep),
            "label=MetricName|stat|unit; label=…",
        ),
        text(
            "thresholds",
            "Thresholds",
            thresholds.join(&sep),
            "label=warn,critical; label=<warn,critical when lower is worse",
        ),
        FormField::multi_select(
            "rds",
            "RDS instances",
            spec.rds.clone(),
            spec.rds.clone(),
            Some("space to choose; none is assumed"),
        ),
        FormField::multi_select(
            "elasticache",
            "Cache nodes",
            spec.elasticache.clone(),
            spec.elasticache.clone(),
            Some("space to choose"),
        ),
        text(
            "alarm_prefix",
            "Alarm prefix",
            spec.alarm_prefix.unwrap_or_default(),
            "alarms named with this belong to the env",
        ),
        log_group,
        text(
            "error_fingerprint",
            "Error fingerprint",
            spec.error_fingerprint.unwrap_or_default(),
            "Logs Insights regex with a named group fp (default: 100 chars from ERROR)",
        ),
    ]
}

fn field<'a>(fields: &'a mut [FormField], key: &str) -> Option<&'a mut FormField> {
    fields.iter_mut().find(|f| f.key == key)
}

/// Merge discovery into the fields. A value already set (by config, or
/// typed while discovery ran) is kept; options are added, not replaced;
/// what was found says where it came from in the field's help.
pub(crate) fn apply_discovery(fields: &mut [FormField], found: &Discovered) {
    let d = &found.discovery;
    if let Some(f) = field(fields, "log_group") {
        let mut options = vec![NO_LOG_GROUP.to_string()];
        options.extend(d.log_groups.iter().cloned());
        if !options.contains(&f.value) {
            options.push(f.value.clone());
        }
        // Offered, not chosen: a log group turns on a Logs Insights
        // query billed per GB scanned, which is the operator's call.
        if f.value == NO_LOG_GROUP {
            if let Some(app_log) = &d.app_log {
                f.help = Some(format!(
                    "found: {app_log} is the env's only non-platform log; choose it \
                     to turn on top errors (Logs Insights, billed per GB scanned)"
                ));
            }
        }
        f.kind = FieldKind::Select { options };
    }
    if let Some(f) = field(fields, "alarm_prefix") {
        if let (true, Some((prefix, n))) = (f.value.is_empty(), &d.alarm_prefix) {
            f.value = prefix.clone();
            f.help = Some(format!("found: {n} alarms named {prefix}…"));
        }
    }
    resources(
        fields,
        "rds",
        &d.rds_options,
        &d.rds_found,
        "RDS instances: none is assumed",
    );
    resources(
        fields,
        "elasticache",
        &d.cache_options,
        &d.cache_found,
        "cache nodes",
    );
    if let Some(app) = &d.app_metrics {
        let mut set = |key: &str, value: String, why: String| {
            if let Some(f) = field(fields, key) {
                if f.value.is_empty() {
                    f.value = value;
                    f.help = Some(why);
                }
            }
        };
        let why = format!(
            "found: metrics in {} dimensioned {}={}",
            app.namespace, app.dimension.0, app.dimension.1
        );
        set("namespace", app.namespace.clone(), why.clone());
        set(
            "dimensions",
            format!("{}={}", app.dimension.0, app.dimension.1),
            why.clone(),
        );
        if let Some(inst) = &app.instance_dimension {
            set("instance_dimension", inst.clone(), why.clone());
        }
        let mut labels: Vec<String> = Vec::new();
        let entries: Vec<String> = app
            .names
            .iter()
            .map(|n| {
                // `heap.used` and `heap_used` share a label; a second
                // would replace the first, so it is numbered instead.
                let base = label_for(n);
                let mut label = base.clone();
                let mut k = 2;
                while labels.contains(&label) {
                    label = format!("{base}_{k}");
                    k += 1;
                }
                labels.push(label.clone());
                format!("{label}={n}|Average")
            })
            .collect();
        set("metrics", entries.join(&format!("{ENTRY_SEP} ")), why);
    }
}

/// A metric label from a metric name: a label is one word with no dots.
fn label_for(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// A resource multi-select: every option offered, what is configured
/// kept, and what an env variable names pre-selected only when nothing
/// is configured, with the variable named.
fn resources(
    fields: &mut [FormField],
    key: &str,
    options: &[String],
    found: &[(String, String)],
    what: &str,
) {
    let Some(f) = field(fields, key) else {
        return;
    };
    let selected = crate::form::parse_multi_value(&f.value);
    let mut all: Vec<String> = options.to_vec();
    for s in &selected {
        if !all.contains(s) {
            all.push(s.clone());
        }
    }
    all.sort();
    let chosen = if selected.is_empty() {
        found.iter().map(|(id, _)| id.clone()).collect()
    } else {
        selected
    };
    let annotations: Vec<String> = all
        .iter()
        .map(|o| {
            found
                .iter()
                .find(|(id, _)| id == o)
                .map(|(_, var)| format!("named by env var {var}"))
                .unwrap_or_default()
        })
        .collect();
    f.value = chosen
        .iter()
        .filter(|c| all.contains(c))
        .cloned()
        .collect::<Vec<_>>()
        .join(",");
    f.help = Some(if all.is_empty() {
        format!("no {what} with recent CloudWatch metrics in this region")
    } else {
        format!("{} {what} with recent metrics; space to choose", all.len())
    });
    f.kind = FieldKind::MultiSelect { options: all };
    f.option_annotations = Some(annotations);
}

/// The fields as `app.ENV.*` lines, checked by the config parser
/// itself: the errors it reports, or the lines to write.
pub(crate) fn lines_from_fields(
    env: &str,
    fields: &[FormField],
) -> Result<Vec<String>, Vec<String>> {
    let value = |key: &str| {
        fields
            .iter()
            .find(|f| f.key == key)
            .map(|f| f.value.trim().to_string())
            .unwrap_or_default()
    };
    let mut lines = Vec::new();
    let mut line = |field: &str, v: &str| {
        if !v.is_empty() {
            lines.push(format!("app.{env}.{field} = \"{v}\""));
        }
    };
    line("namespace", &value("namespace"));
    line("dimensions", &value("dimensions"));
    line("instance_dimension", &value("instance_dimension"));
    let mut entry_errors = Vec::new();
    for (family, key) in [("metric", "metrics"), ("threshold", "thresholds")] {
        for entry in value(key)
            .split(ENTRY_SEP)
            .map(str::trim)
            .filter(|e| !e.is_empty())
        {
            match entry.split_once('=') {
                Some((label, v)) => line(&format!("{family}.{}", label.trim()), v.trim()),
                None => entry_errors.push(format!("{key}: {entry:?} is not label=value")),
            }
        }
    }
    line("rds", &value("rds"));
    line("elasticache", &value("elasticache"));
    line("alarm_prefix", &value("alarm_prefix"));
    let log_group = value("log_group");
    if log_group != NO_LOG_GROUP {
        line("log_group", &log_group);
    }
    line("error_fingerprint", &value("error_fingerprint"));
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    let errors: Vec<String> = entry_errors
        .into_iter()
        .chain(
            crate::config::parse(&text)
                .app_tabs
                .errors
                .into_iter()
                .map(|e| e.message),
        )
        .collect();
    if errors.is_empty() {
        Ok(lines)
    } else {
        Err(errors)
    }
}

/// The form's lines, plus the env's lines that did not apply: the form
/// cannot show those (it is built from what parsed), and dropping them
/// would delete config the operator wrote, a field a newer ebman reads
/// included. One the form now writes the same key for is replaced.
pub(crate) fn with_rejected_lines<'a>(
    mut lines: Vec<String>,
    rejected: impl Iterator<Item = &'a str>,
) -> Vec<String> {
    let key = |l: &str| l.split_once('=').map(|(k, _)| k.trim().to_string());
    let written: Vec<Option<String>> = lines.iter().map(|l| key(l)).collect();
    for r in rejected {
        if !written.contains(&key(r)) {
            lines.push(r.to_string());
        }
    }
    lines
}

impl App {
    /// `e` on the App tab: open the editor for the Detail view's env.
    pub(crate) fn open_app_tab_form(&mut self) {
        let Some(detail) = self.detail.as_ref() else {
            return;
        };
        let env = detail.env_name.clone();
        let app_name = detail.env_snapshot.application.clone();
        // Built from the file as it is now: a hand edit since the last
        // refresh would otherwise be missing from the form, and saving
        // the form replaces the env's lines on disk.
        self.reload_app_tabs_if_changed(crate::config::config_mtime(), || {
            std::fs::read_to_string(crate::config::config_path())
        });
        let form = crate::form::Form {
            title: format!("App tab: {env}"),
            fields: fields(self.cfg.app_tabs.envs.get(&env)),
            cursor: 0,
            state: crate::form::FormState::Loading,
            submit: crate::form::FormSubmit::AppTab,
            summary: "App tab config".into(),
            env_name: env.clone(),
            banner: format!(
                "app.{env}.* in {}; finding what this env uses…",
                crate::config::config_path().display()
            ),
            scroll: 0,
        };
        self.form = Some(form);
        self.mode = Mode::Form;
        if self.demo_mode {
            let _ = self.msg_tx.send(AppMsg::AppTabDiscovered {
                gen: self.generation,
                env_name: env,
                found: Discovered::default(),
            });
            return;
        }
        let fleet: Vec<String> = self.environments.iter().map(|e| e.name.clone()).collect();
        let client = self.detail_client();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            let found = match client.resolve().await {
                Ok(aws) => discover(&aws, &app_name, &env, fleet).await,
                Err(e) => Discovered {
                    failed: vec![("AWS client".into(), flatten_err("cached_client", e))],
                    ..Default::default()
                },
            };
            let _ = tx.send(AppMsg::AppTabDiscovered {
                gen,
                env_name: env,
                found,
            });
        });
    }

    pub(super) fn handle_app_tab_discovered(&mut self, env_name: String, found: Discovered) {
        let Some(form) = self.form.as_mut() else {
            return;
        };
        if form.env_name != env_name || !matches!(form.submit, crate::form::FormSubmit::AppTab) {
            return;
        }
        apply_discovery(&mut form.fields, &found);
        form.banner = if found.failed.is_empty() {
            format!("app.{env_name}.* in config.toml; suggestions say what found them")
        } else {
            let failed: Vec<String> = found
                .failed
                .iter()
                .map(|(what, why)| format!("{what}: {why}"))
                .collect();
            format!(
                "some lookups failed, so some options may be missing: {}",
                failed.join("; ")
            )
        };
        form.state = crate::form::FormState::Ready;
    }

    /// Save: check the lines with the config parser, write only this
    /// env's lines, and apply them to the running tab.
    pub(super) fn submit_app_tab_form(&mut self) {
        let Some(form) = self.form.as_mut() else {
            return;
        };
        let env = form.env_name.clone();
        let lines = match lines_from_fields(&env, &form.fields) {
            Ok(lines) => with_rejected_lines(lines, self.cfg.app_tabs.rejected_lines(&env)),
            Err(errors) => {
                form.banner = format!("not saved: {}", errors.join("; "));
                return;
            }
        };
        if let Err(e) = crate::config::save_app_lines(&env, &lines) {
            form.banner = format!("not saved: {e}");
            return;
        }
        let mtime = crate::config::config_mtime();
        let text = std::fs::read_to_string(crate::config::config_path()).unwrap_or_default();
        self.cfg.app_tabs = crate::config::parse(&text).app_tabs;
        self.config_seen = Some(mtime);
        self.form = None;
        self.mode = Mode::Detail;
        self.pin_status(format!("App tab saved for {env}"));
        self.detail_refresh_active_tab();
    }
}

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
async fn discover(
    aws: &crate::aws::AwsClient,
    app_name: &str,
    env: &str,
    fleet: Vec<String>,
) -> Discovered {
    let prefix = format!("{env}-");
    // Each lookup pages within its own deadline; `bounded` also caps a
    // single request that hangs, so the form never waits past it.
    let (groups, vars, rds, cache, alarms, custom) = tokio::join!(
        bounded(aws.discover_env_log_groups(env)),
        bounded(aws.fetch_env_vars(app_name, env)),
        bounded(aws.list_dimension_values("AWS/RDS", "CPUUtilization", "DBInstanceIdentifier")),
        bounded(aws.list_dimension_values("AWS/ElastiCache", "CPUUtilization", "CacheClusterId")),
        bounded(aws.list_alarm_names_with_prefix(&prefix)),
        bounded(aws.list_custom_metrics()),
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
    Discovered {
        discovery: crate::app_discover::discover(env, &inputs),
        failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_discover::{AppMetricsFound, Discovery};

    fn value<'a>(fields: &'a [FormField], key: &str) -> &'a str {
        &fields.iter().find(|f| f.key == key).unwrap().value
    }

    fn found() -> Discovered {
        Discovered {
            discovery: Discovery {
                log_groups: vec!["var/log/eb-engine.log".into(), "var/log/app.log".into()],
                app_log: Some("var/log/app.log".into()),
                alarm_prefix: Some(("Shop-prod-".into(), 3)),
                rds_options: vec!["other-db".into(), "shop-db".into()],
                rds_found: vec![],
                cache_options: vec!["sessions-001".into()],
                cache_found: vec![("sessions-001".into(), "REDIS".into())],
                app_metrics: Some(AppMetricsFound {
                    namespace: "Shop".into(),
                    dimension: ("Environment".into(), "Shop-prod".into()),
                    instance_dimension: Some("InstanceId".into()),
                    names: vec!["usersActive".into(), "heap.used".into(), "heap_used".into()],
                }),
            },
            failed: vec![],
        }
    }

    #[test]
    fn discovery_fills_empty_fields_and_assumes_no_database() {
        let mut f = fields(None);
        apply_discovery(&mut f, &found());
        assert_eq!(
            value(&f, "log_group"),
            NO_LOG_GROUP,
            "offered, not chosen: it is billed"
        );
        let lg = f.iter().find(|x| x.key == "log_group").unwrap();
        assert!(lg
            .help
            .as_deref()
            .unwrap_or("")
            .contains("var/log/app.log is the env's only"));
        assert!(
            matches!(&lg.kind, FieldKind::Select { options } if options.contains(&"var/log/app.log".to_string()))
        );
        assert_eq!(value(&f, "alarm_prefix"), "Shop-prod-");
        assert_eq!(value(&f, "rds"), "", "offered, none chosen");
        let rds = f.iter().find(|x| x.key == "rds").unwrap();
        assert_eq!(
            rds.kind,
            FieldKind::MultiSelect {
                options: vec!["other-db".into(), "shop-db".into()]
            }
        );
        assert_eq!(
            value(&f, "elasticache"),
            "sessions-001",
            "named by an env var"
        );
        let cache = f.iter().find(|x| x.key == "elasticache").unwrap();
        assert_eq!(
            cache.option_annotations.as_deref(),
            Some(&["named by env var REDIS".to_string()][..])
        );
        assert_eq!(value(&f, "namespace"), "Shop");
        assert_eq!(value(&f, "dimensions"), "Environment=Shop-prod");
        assert_eq!(value(&f, "instance_dimension"), "InstanceId");
        assert_eq!(
            value(&f, "metrics"),
            "usersActive=usersActive|Average; heap_used=heap.used|Average; heap_used_2=heap_used|Average",
            "colliding labels are numbered, not lost"
        );
    }

    #[test]
    fn configured_values_win_over_discovery() {
        let spec = crate::config::parse(
            "app.e.namespace = \"Mine\"\napp.e.rds = \"other-db\"\n\
             app.e.alarm_prefix = \"x-\"\napp.e.log_group = \"var/log/eb-engine.log\"\n",
        )
        .app_tabs
        .envs
        .remove("e")
        .unwrap();
        let mut f = fields(Some(&spec));
        apply_discovery(&mut f, &found());
        assert_eq!(value(&f, "namespace"), "Mine");
        assert_eq!(value(&f, "rds"), "other-db");
        assert_eq!(value(&f, "alarm_prefix"), "x-");
        assert_eq!(value(&f, "log_group"), "var/log/eb-engine.log");
    }

    #[test]
    fn an_env_var_named_database_is_preselected_and_said_so() {
        let mut d = found();
        d.discovery.rds_found = vec![("shop-db".into(), "DB_HOST".into())];
        let mut f = fields(None);
        apply_discovery(&mut f, &d);
        assert_eq!(value(&f, "rds"), "shop-db");
        let rds = f.iter().find(|x| x.key == "rds").unwrap();
        assert_eq!(
            rds.option_annotations.as_deref(),
            Some(&[String::new(), "named by env var DB_HOST".to_string()][..])
        );
    }

    #[test]
    fn fields_round_trip_through_lines_and_the_parser() {
        let spec = crate::config::parse(
            "app.e.namespace = \"N\"\napp.e.metric.u = \"users|Sum|count\"\n\
             app.e.threshold.u = \"<5,1\"\napp.e.rds = \"a,b\"\napp.e.log_group = \"var/log/x.log\"\n",
        )
        .app_tabs
        .envs
        .remove("e")
        .unwrap();
        let f = fields(Some(&spec));
        let lines = lines_from_fields("e", &f).expect("valid");
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        assert_eq!(crate::config::parse(&text).app_tabs.envs["e"], spec);
    }

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
        let found = discover(&aws, "shop", "e", vec!["e".into()]).await;
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

    #[test]
    fn a_save_keeps_the_envs_lines_that_did_not_apply() {
        let tabs = crate::config::parse(
            "app.e.namespace = \"N\"\n\
             app.e.threshold.heap = \"1800,1500\"\n\
             app.e.future_field = \"x\"\n\
             app.e.alarm_prefix = \"\"\n\
             app.other.future_field = \"y\"\n",
        )
        .app_tabs;
        let lines = with_rejected_lines(
            vec![
                "app.e.namespace = \"N\"".to_string(),
                // The form now writes alarm_prefix: the empty, rejected one goes.
                "app.e.alarm_prefix = \"e-\"".to_string(),
            ],
            tabs.rejected_lines("e"),
        );
        assert_eq!(
            lines,
            [
                "app.e.namespace = \"N\"",
                "app.e.alarm_prefix = \"e-\"",
                "app.e.threshold.heap = \"1800,1500\"",
                "app.e.future_field = \"x\"",
            ],
            "another env's lines are not this save's"
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

    #[test]
    fn a_bad_entry_is_refused_with_the_parsers_own_words() {
        let mut f = fields(None);
        f.iter_mut().find(|x| x.key == "metrics").unwrap().value = "u=users|Avg; junk".into();
        let errors = lines_from_fields("e", &f).expect_err("refused");
        assert!(
            errors
                .iter()
                .any(|e| e.contains("\"junk\" is not label=value")),
            "{errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("not a CloudWatch statistic")),
            "{errors:?}"
        );
        // "(none)" is no log group, not one named "(none)".
        let f = fields(None);
        assert_eq!(
            lines_from_fields("e", &f).expect("empty is valid"),
            Vec::<String>::new()
        );
    }
}
