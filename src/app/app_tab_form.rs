//! The App tab's editor: `e` on the App tab opens a form over the env's
//! `app.ENV.*` config, pre-filled with what discovery finds, and saving
//! writes that env's lines to `config.toml` and applies them at once.
//!
//! Discovery suggests; it never decides. A field the config already
//! sets keeps its value, and an RDS instance is only ever pre-selected
//! when one of the env's own variables names its endpoint: the rest
//! are offered, not chosen.

use super::*;
use crate::app_discover::Discovered;
use crate::app_tab::AppTabSpec;
use crate::form::{FieldKind, FormField};

/// The `log_group` option meaning "no top errors".
const NO_LOG_GROUP: &str = "(none)";

/// Separates the entries of the metrics and thresholds fields.
const ENTRY_SEP: char = ';';

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
        .map(|m| format!("{}={}", m.label, m.to_value()))
        .collect();
    let thresholds: Vec<String> = spec
        .thresholds
        .iter()
        .map(|(label, t)| format!("{label}={}", t.to_value()))
        .collect();
    let dims = crate::app_tab::dimensions_value(&spec.dimensions);
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
            dims,
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
    // As the parser reads a key: `app.e.metric. heap` is
    // `app.e.metric.heap`.
    let key = crate::app_tab::key_of;
    let written: Vec<_> = lines.iter().map(|l| key(l)).collect();
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
        // Built from the file as it is now: a hand edit since the last
        // refresh would otherwise be missing from the form, and saving
        // the form replaces the env's lines on disk.
        self.reload_app_tabs_and_refetch();
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
                self.config_file.display()
            ),
            scroll: 0,
        };
        self.form = Some(form);
        self.mode = Mode::Form;
        // Found already this session, and nothing failed: use it.
        if let Some(found) = self
            .app_discovered
            .get(&self.discovery_key(&env))
            .filter(|f| f.failed.is_empty())
            .cloned()
        {
            self.fill_app_tab_form(&env, &found);
            return;
        }
        self.spawn_app_discovery(env);
    }

    /// Start discovery for `env` unless it is already running. Demo mode
    /// answers at once with nothing, without calling AWS.
    pub(crate) fn spawn_app_discovery(&mut self, env: String) {
        let key = self.discovery_key(&env);
        if self.demo_mode {
            // Not cached: demo mode found nothing because it looked at
            // nothing, and the tab should keep saying what to configure.
            let _ = self.msg_tx.send(AppMsg::AppTabDiscovered {
                gen: self.generation,
                region: key.0,
                env_name: env,
                started: std::time::Instant::now(),
                found: Box::default(),
            });
            return;
        }
        if self.discovery_running(&env) {
            return;
        }
        let started = std::time::Instant::now();
        self.app_discovering.insert(key.clone(), started);
        // The Detail view's own env: by name alone, a same-named env in
        // another region of a fan-out could name another application.
        let app_name = self
            .detail
            .as_ref()
            .filter(|d| d.env_name == env)
            .map(|d| d.env_snapshot.application.clone())
            .or_else(|| {
                self.environments
                    .iter()
                    .find(|e| e.name == env)
                    .map(|e| e.application.clone())
            })
            .unwrap_or_default();
        let fleet: Vec<String> = self.environments.iter().map(|e| e.name.clone()).collect();
        let dimension_names = crate::app_discover::env_dimension_names(
            &self.cfg.alarm_dimensions,
            self.cfg.app_tabs.envs.get(&env),
        );
        let client = self.discovery_client(&key);
        let region = key.0;
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            let found = match client.resolve().await {
                Ok(aws) => {
                    crate::aws::discover_app(&aws, &app_name, &env, fleet, dimension_names).await
                }
                Err(e) => Discovered {
                    failed: vec![("AWS client".into(), flatten_err("cached_client", e))],
                    ..Default::default()
                },
            };
            let _ = tx.send(AppMsg::AppTabDiscovered {
                gen,
                region,
                env_name: env,
                started,
                found: Box::new(found),
            });
        });
    }

    /// Whether `env`'s discovery is running, and not so long that its
    /// result must have been lost.
    pub(crate) fn discovery_running(&self, env: &str) -> bool {
        self.app_discovering
            .get(&self.discovery_key(env))
            .is_some_and(|t| t.elapsed() < DISCOVERY_STALE)
    }

    /// Where `env`'s App tab comes from: its config, else what discovery
    /// found evidence for (drawn as a suggestion), else nothing. Every
    /// place that asks "is this env set up?" asks this, so the tab, its
    /// key strip, its fetch and its discovery cannot disagree.
    pub(crate) fn app_tab_source(&self, env: &str) -> AppTabSource<'_> {
        match self.cfg.app_tabs.envs.get(env) {
            Some(spec) => AppTabSource::Configured(spec),
            None => match self
                .discovered_for(env)
                .and_then(crate::app_discover::preview_spec)
            {
                Some(spec) => AppTabSource::Preview(Box::new(spec)),
                None => AppTabSource::NotSetUp,
            },
        }
    }

    /// What discovery found for `env` this session, if anything.
    pub(crate) fn discovered_for(&self, env: &str) -> Option<&Discovered> {
        self.app_discovered.get(&self.discovery_key(env))
    }

    /// The discovery caches' key for `env`: (region, env). The Detail
    /// view's own env by its snapshot's region (it may have left the
    /// list), any other by its row's.
    pub(crate) fn discovery_key(&self, env: &str) -> (String, String) {
        let region = self
            .detail
            .as_ref()
            .filter(|d| d.env_name == env)
            .map(|d| self.region_for(&d.env_snapshot))
            .unwrap_or_else(|| self.region_for_name(env));
        (region, env.to_string())
    }

    /// The client discovery for `key` runs on: the key's region, so the
    /// lookups and the cache entry they fill agree.
    pub(crate) fn discovery_client(&self, key: &(String, String)) -> RegionClient {
        self.client_for_region(&key.0)
    }

    pub(super) fn handle_app_tab_discovered(
        &mut self,
        key: (String, String),
        started: std::time::Instant,
        found: Discovered,
    ) {
        if self.app_discovering.get(&key) == Some(&started) {
            self.app_discovering.remove(&key);
        }
        // Another region's same-named env is not the one on screen: its
        // answer is cached for it, and fills nothing here.
        let current = key == self.discovery_key(&key.1);
        let env_name = key.1.clone();
        if !self.demo_mode {
            self.app_discovered.insert(key, found.clone());
        }
        if !current {
            return;
        }
        self.fill_app_tab_form(&env_name, &found);
        // An unconfigured env's App tab on screen: preview what was found.
        let on_tab = self
            .detail
            .as_ref()
            .is_some_and(|d| d.env_name == env_name && d.tab() == DetailTab::App);
        if on_tab && !self.app_tab_source(&env_name).is_configured() && self.form.is_none() {
            self.spawn_detail_app(env_name);
        }
    }

    /// Merge `found` into the open editor for `env_name`, if there is one.
    fn fill_app_tab_form(&mut self, env_name: &str, found: &Discovered) {
        let Some(form) = self.form.as_mut() else {
            return;
        };
        if form.env_name != env_name || !matches!(form.submit, crate::form::FormSubmit::AppTab) {
            return;
        }
        apply_discovery(&mut form.fields, found);
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
        let written = match crate::config::save_app_lines(&self.config_file, &env, &lines) {
            Ok(written) => written,
            Err(e) => {
                form.banner = format!("not saved: {e}");
                return;
            }
        };
        // Applied from what was written, not a read-back that can fail.
        // The refresh below re-reads the file and records it as seen.
        self.set_app_tabs(crate::config::parse(&written).app_tabs);
        self.form = None;
        self.mode = Mode::Detail;
        self.pin_status(format!("App tab saved for {env}"));
        // A metrics fetch in flight does not make this skip when the
        // save changed the env's spec: set_app_tabs reset it. When it
        // did not, that fetch is for this same config.
        self.detail_refresh_active_tab();
    }
}

/// See [`App::app_tab_source`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AppTabSource<'a> {
    /// The env's `app.ENV.*` lines.
    Configured(&'a AppTabSpec),
    /// No config; the resources discovery has evidence for, unsaved.
    Preview(Box<AppTabSpec>),
    /// No config, and nothing to suggest (yet).
    NotSetUp,
}

impl AppTabSource<'_> {
    pub(crate) fn is_configured(&self) -> bool {
        matches!(self, AppTabSource::Configured(_))
    }

    /// The spec to fetch for: the config's, or the preview's.
    pub(crate) fn spec(&self) -> Option<AppTabSpec> {
        match self {
            AppTabSource::Configured(s) => Some((*s).clone()),
            AppTabSource::Preview(s) => Some((**s).clone()),
            AppTabSource::NotSetUp => None,
        }
    }
}

/// Past this, a discovery with no answer is treated as lost: well beyond
/// the discovery lookups' own bound (`aws::discover`), which bounds every lookup it makes.
pub(crate) const DISCOVERY_STALE: std::time::Duration = std::time::Duration::from_secs(60);

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

    /// Every field the parser accepts survives the editor. A field
    /// added to the parser (and so to `FIELDS`) but not to the form is
    /// erased from disk the first time the form is saved, because a save
    /// replaces all of the env's lines: so this test needs a sample for
    /// every `FIELDS` entry, and fails on one it does not have.
    #[test]
    fn every_field_the_parser_accepts_survives_the_editor() {
        let sample = |field: &str| -> Option<Vec<&str>> {
            Some(match field {
                "namespace" => vec!["app.e.namespace = \"Shop\""],
                "dimensions" => vec!["app.e.dimensions = \"Environment=prod,Tier=web\""],
                "instance_dimension" => vec!["app.e.instance_dimension = \"InstanceId\""],
                "metric.LABEL" => vec![
                    "app.e.metric.users = \"usersActive|Maximum|count\"",
                    "app.e.metric.heap = \"heapUsedMb|p99\"",
                ],
                "threshold.LABEL" => vec![
                    "app.e.threshold.heap = \"1500,1800.5\"",
                    "app.e.threshold.users = \"<5,1\"",
                ],
                "rds" => vec!["app.e.rds = \"db-a,db-b\""],
                "elasticache" => vec!["app.e.elasticache = \"c-001\""],
                "alarm_prefix" => vec!["app.e.alarm_prefix = \"e-\""],
                "log_group" => vec!["app.e.log_group = \"var/log/app.log\""],
                "error_fingerprint" => vec![r#"app.e.error_fingerprint = "msg="(?<fp>[^"]+)"""#],
                _ => return None,
            })
        };
        let mut text = String::new();
        for field in crate::app_tab::FIELDS {
            let lines = sample(field).unwrap_or_else(|| {
                panic!("no sample for {field}: add one, and the field to the form")
            });
            for l in lines {
                text.push_str(l);
                text.push('\n');
            }
        }
        let tabs = crate::config::parse(&text).app_tabs;
        assert!(tabs.errors.is_empty(), "{:?}", tabs.errors);
        let spec = &tabs.envs["e"];
        let lines = lines_from_fields("e", &fields(Some(spec))).expect("valid");
        let back: String = lines.iter().map(|l| format!("{l}\n")).collect();
        assert_eq!(
            &crate::config::parse(&back).app_tabs.envs["e"],
            spec,
            "{back}"
        );
    }

    #[test]
    fn a_save_keeps_the_envs_lines_that_did_not_apply() {
        let tabs = crate::config::parse(
            "app.e.namespace = \"N\"\n\
             app.e.threshold.heap = \"1800,1500\"\n\
             app.e.future_field = \"x\"\n\
             app.e.alarm_prefix = \"\"\n\
             app.e.threshold. lag = \"2,1\"\n\
             app.other.future_field = \"y\"\n",
        )
        .app_tabs;
        let lines = with_rejected_lines(
            vec![
                "app.e.namespace = \"N\"".to_string(),
                // The form now writes alarm_prefix: the empty, rejected one goes.
                "app.e.alarm_prefix = \"e-\"".to_string(),
                // And this threshold, whose rejected line had a stray space.
                "app.e.threshold.lag = \"1,2\"".to_string(),
            ],
            tabs.rejected_lines("e"),
        );
        assert_eq!(
            lines,
            [
                "app.e.namespace = \"N\"",
                "app.e.alarm_prefix = \"e-\"",
                "app.e.threshold.lag = \"1,2\"",
                "app.e.threshold.heap = \"1800,1500\"",
                "app.e.future_field = \"x\"",
            ],
            "another env's lines are not this save's"
        );
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
