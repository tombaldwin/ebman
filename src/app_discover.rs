//! Suggestions for an environment's App tab config, from the
//! environment's own evidence: its log groups, the endpoints its
//! environment variables name, alarms named for it, and metrics
//! dimensioned by its name.
//!
//! Pure: the fetches happen elsewhere and hand their results in, so
//! every rule here is testable without AWS. Nothing is configured from
//! a suggestion until the operator saves the App tab's form.
//!
//! Environment variable VALUES are read only for an RDS or ElastiCache
//! endpoint hostname: [`endpoint_hosts`] keeps the hostname and the
//! variable's name and drops the rest, so no other value (a password
//! sitting next to a host) leaves the scan.

/// What discovery was given.
#[derive(Debug, Clone, Default)]
pub(crate) struct Inputs {
    /// Full log group names under `/aws/elasticbeanstalk/<env>/`.
    pub log_groups: Vec<String>,
    /// `(variable name, endpoint hostname)`, from [`endpoint_hosts`].
    pub endpoint_hosts: Vec<(String, String)>,
    /// DB instance identifiers CloudWatch has recent metrics for.
    pub rds_ids: Vec<String>,
    /// Cache node ids (`CacheClusterId`) CloudWatch has recent metrics for.
    pub cache_ids: Vec<String>,
    /// Alarm names starting with the env's name and a dash.
    pub prefixed_alarms: Vec<String>,
    /// Every environment name, so a prefix can give way to a longer one.
    pub fleet: Vec<String>,
    /// Recently active metrics outside the `AWS/` namespaces.
    pub custom_metrics: Vec<CustomMetric>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CustomMetric {
    pub namespace: String,
    pub name: String,
    pub dimensions: Vec<(String, String)>,
}

/// What the evidence suggests. Each suggestion says why, for the form
/// to show beside it.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Discovery {
    /// Log group suffixes under the env, every one: the form's options.
    pub log_groups: Vec<String>,
    /// The one group that is not Elastic Beanstalk's own, if exactly one.
    pub app_log: Option<String>,
    /// `(prefix, how many alarms it names)`.
    pub alarm_prefix: Option<(String, usize)>,
    /// Every DB instance with recent metrics: options, never chosen.
    pub rds_options: Vec<String>,
    /// DB instances an environment variable names: `(id, variable)`.
    pub rds_found: Vec<(String, String)>,
    pub cache_options: Vec<String>,
    /// Cache nodes an environment variable's endpoint leads to.
    pub cache_found: Vec<(String, String)>,
    pub app_metrics: Option<AppMetricsFound>,
}

/// The operator's own metrics, found by a dimension valued the env name.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AppMetricsFound {
    pub namespace: String,
    pub dimension: (String, String),
    pub instance_dimension: Option<String>,
    pub names: Vec<String>,
}

/// Most metric names offered at once; more would bury the form.
const MAX_METRICS: usize = 20;

/// Log files Elastic Beanstalk and its platforms write for themselves.
/// What is left is the application's.
const PLATFORM_LOGS: &[&str] = &[
    "var/log/eb-engine.log",
    "var/log/eb-hooks.log",
    "var/log/eb-activity.log",
    "var/log/eb-cfn-init.log",
    "var/log/eb-cfn-init-call.log",
    "var/log/eb-commandprocessor.log",
    "var/log/eb-publish-logs.log",
    "var/log/eb-tools.log",
    "var/log/cfn-init.log",
    "var/log/cfn-init-cmd.log",
    "var/log/cfn-hup.log",
    "var/log/cloud-init.log",
    "var/log/cloud-init-output.log",
    "var/log/messages",
    "var/log/secure",
    "var/log/yum.log",
    "var/log/docker",
    "var/log/docker-events.log",
];

/// Directories whose logs are a web server's or a platform agent's.
const PLATFORM_LOG_DIRS: &[&str] = &[
    "var/log/httpd/",
    "var/log/nginx/",
    "var/log/healthd/",
    "var/log/aws-sqsd/",
    "var/log/eb-docker/",
    "var/log/xray/",
];

/// The `(variable, hostname)` pairs of RDS and ElastiCache endpoints in
/// an env's variables, and nothing else from them.
pub(crate) fn endpoint_hosts(vars: &[(String, String)]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (name, value) in vars {
        for token in value.split(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-')) {
            let host = token.trim_matches('.').to_ascii_lowercase();
            let endpoint =
                host.ends_with(".rds.amazonaws.com") || host.ends_with(".cache.amazonaws.com");
            if endpoint && !out.iter().any(|(n, h)| n == name && *h == host) {
                out.push((name.clone(), host));
            }
        }
    }
    out
}

/// The DB instance an RDS endpoint names. An Aurora cluster endpoint
/// (`<cluster>.cluster-…`) names a cluster, not an instance, and gives
/// no instance to read, so it suggests nothing.
fn rds_instance_of(host: &str) -> Option<&str> {
    let host = host.strip_suffix(".rds.amazonaws.com")?;
    let mut labels = host.split('.');
    let first = labels.next()?;
    let second = labels.next().unwrap_or("");
    (!second.starts_with("cluster-")).then_some(first)
}

/// The replication group or node an ElastiCache endpoint names:
/// `master.<group>.…`, `replica.<group>.…`, `clustercfg.<group>.…`, or
/// `<group-or-node>.…`. Serverless endpoints are not read (they are
/// dimensioned differently), so they suggest nothing.
fn cache_target_of(host: &str) -> Option<&str> {
    let host = host.strip_suffix(".cache.amazonaws.com")?;
    if host.contains(".serverless.") {
        return None;
    }
    let mut labels = host.split('.');
    let first = labels.next()?;
    match first {
        "master" | "replica" | "clustercfg" => labels.next(),
        _ => Some(first),
    }
}

/// Whether cache node `node` is `target` itself or one of group
/// `target`'s nodes, which ElastiCache names `<group>-<digits>`, or
/// `<group>-<shard>-<node>` (`-0001-001`) with cluster mode on. A bare
/// prefix would take `prod-cache-001` as a node of group `prod`.
fn is_node_of(node: &str, target: &str) -> bool {
    let digits = |s: &str, n: usize| s.len() == n && s.chars().all(|c| c.is_ascii_digit());
    if node == target {
        return true;
    }
    let Some(suffix) = node.strip_prefix(target).and_then(|r| r.strip_prefix('-')) else {
        return false;
    };
    // ElastiCache's own suffixes only: `-001`, or `-0001-001` with
    // cluster mode on. Any digits would take group `app-2`'s `app-2-001`
    // as a node of group `app`.
    match suffix.split_once('-') {
        None => digits(suffix, 3),
        Some((shard, n)) => digits(shard, 4) && digits(n, 3),
    }
}

pub(crate) fn discover(env: &str, inputs: &Inputs) -> Discovery {
    let base = format!("/aws/elasticbeanstalk/{env}/");
    let log_groups: Vec<String> = inputs
        .log_groups
        .iter()
        .filter_map(|g| g.strip_prefix(&base).map(str::to_string))
        .collect();
    let app_logs: Vec<&String> = log_groups
        .iter()
        .filter(|g| !PLATFORM_LOGS.contains(&g.as_str()))
        .filter(|g| !PLATFORM_LOG_DIRS.iter().any(|d| g.starts_with(d)))
        .collect();
    let app_log = match app_logs.as_slice() {
        [only] => Some((*only).clone()),
        _ => None,
    };

    let prefix = format!("{env}-");
    let shadowing: Vec<&String> = inputs
        .fleet
        .iter()
        .filter(|other| *other != env && other.starts_with(&prefix))
        .collect();
    let named = inputs
        .prefixed_alarms
        .iter()
        .filter(|a| a.starts_with(&prefix))
        .filter(|a| {
            !shadowing
                .iter()
                .any(|other| crate::app_tab::alarm_names_env(a, other))
        })
        .count();
    let alarm_prefix = (named > 0).then_some((prefix, named));

    let mut rds_found = Vec::new();
    let mut cache_found = Vec::new();
    for (var, host) in &inputs.endpoint_hosts {
        if let Some(id) = rds_instance_of(host) {
            if inputs.rds_ids.iter().any(|r| r == id) && !rds_found.iter().any(|(r, _)| r == id) {
                rds_found.push((id.to_string(), var.clone()));
            }
        }
        if let Some(target) = cache_target_of(host) {
            for node in inputs.cache_ids.iter().filter(|n| is_node_of(n, target)) {
                if !cache_found.iter().any(|(n, _)| n == node) {
                    cache_found.push((node.clone(), var.clone()));
                }
            }
        }
    }

    Discovery {
        log_groups,
        app_log,
        alarm_prefix,
        rds_options: sorted(&inputs.rds_ids),
        rds_found,
        cache_options: sorted(&inputs.cache_ids),
        cache_found,
        app_metrics: app_metrics(env, &inputs.custom_metrics),
    }
}

fn sorted(ids: &[String]) -> Vec<String> {
    let mut v = ids.to_vec();
    v.sort();
    v.dedup();
    v
}

/// The namespace with the most metrics carrying a dimension valued the
/// env name, that dimension, the extra dimension its per-instance
/// series carry (if any), and its metric names.
fn app_metrics(env: &str, metrics: &[CustomMetric]) -> Option<AppMetricsFound> {
    let ours: Vec<&CustomMetric> = metrics
        .iter()
        .filter(|m| m.dimensions.iter().any(|(_, v)| v == env))
        .collect();
    let mut namespaces: Vec<&str> = ours.iter().map(|m| m.namespace.as_str()).collect();
    namespaces.sort();
    namespaces.dedup();
    let namespace = namespaces.into_iter().max_by_key(|ns| {
        (
            ours.iter().filter(|m| m.namespace == *ns).count(),
            std::cmp::Reverse(*ns),
        )
    })?;
    let in_ns: Vec<&&CustomMetric> = ours.iter().filter(|m| m.namespace == namespace).collect();
    let dimension = in_ns
        .iter()
        .flat_map(|m| m.dimensions.iter())
        .find(|(_, v)| v == env)?
        .clone();
    let mut extra: Vec<&str> = in_ns
        .iter()
        .filter(|m| m.dimensions.len() == 2)
        .filter_map(|m| m.dimensions.iter().find(|(k, _)| *k != dimension.0))
        .map(|(k, _)| k.as_str())
        .collect();
    extra.sort();
    extra.dedup();
    let instance_dimension = match extra.as_slice() {
        [only] => Some((*only).to_string()),
        _ => None,
    };
    let mut names: Vec<String> = in_ns.iter().map(|m| m.name.clone()).collect();
    names.sort();
    names.dedup();
    names.truncate(MAX_METRICS);
    Some(AppMetricsFound {
        namespace: namespace.to_string(),
        dimension,
        instance_dimension,
        names,
    })
}

/// What discovery found, and which of its lookups failed.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Discovered {
    pub discovery: Discovery,
    /// `(what, why)` for each lookup that failed: the form says so,
    /// rather than offering fewer options as if that were all there was.
    pub failed: Vec<(String, String)>,
}

/// The App tab to preview for an env with no config: the resources an
/// env variable names, which is evidence, and nothing else. No log
/// group (that is billed) and no alarm prefix (that would change which
/// alarms other screens call the env's); `None` when there is nothing.
pub(crate) fn preview_spec(found: &Discovered) -> Option<crate::app_tab::AppTabSpec> {
    let d = &found.discovery;
    let spec = crate::app_tab::AppTabSpec {
        rds: d.rds_found.iter().map(|(id, _)| id.clone()).collect(),
        elasticache: d.cache_found.iter().map(|(id, _)| id.clone()).collect(),
        ..Default::default()
    };
    (!spec.is_empty()).then_some(spec)
}

/// Dimension names an env's own metrics might be valued by: what ebman
/// already matches envs by (`alarm_dimensions`), what this env's config
/// names, and the usual spellings. CloudWatch filters by a dimension's
/// name and value, not by value alone, so the names have to be given.
pub(crate) fn env_dimension_names(
    alarm_dimensions: &[String],
    spec: Option<&crate::app_tab::AppTabSpec>,
) -> Vec<String> {
    const USUAL: &[&str] = &[
        "EnvironmentName",
        "Environment",
        "Env",
        "environment",
        "env",
        "EnvName",
    ];
    let mut names: Vec<String> = Vec::new();
    let configured = spec
        .into_iter()
        .flat_map(|s| s.dimensions.iter().map(|(k, _)| k.clone()));
    for n in alarm_dimensions
        .iter()
        .cloned()
        .chain(configured)
        .chain(USUAL.iter().map(|s| s.to_string()))
    {
        if !names.contains(&n) {
            names.push(n);
        }
    }
    names
}

/// A discovery with something in most fields, for tests here and in
/// the editor and the renderer.
#[cfg(test)]
pub(crate) fn sample() -> Discovered {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn discovered() -> Discovered {
        sample()
    }

    #[test]
    fn the_dimension_names_tried_are_ebmans_the_configs_and_the_usual() {
        let spec = crate::config::parse("app.e.dimensions = \"Stage=prod\"\n")
            .app_tabs
            .envs
            .remove("e")
            .unwrap();
        let names =
            super::env_dimension_names(&["EnvironmentName".into(), "Svc".into()], Some(&spec));
        assert_eq!(&names[..3], ["EnvironmentName", "Svc", "Stage"]);
        assert!(names.contains(&"Environment".to_string()));
        assert_eq!(
            names.iter().filter(|n| *n == "EnvironmentName").count(),
            1,
            "no name tried twice"
        );
    }

    #[test]
    fn the_preview_takes_only_what_an_env_variable_names() {
        let mut d = discovered();
        assert_eq!(
            preview_spec(&d),
            Some(crate::app_tab::AppTabSpec {
                elasticache: vec!["sessions-001".into()],
                ..Default::default()
            }),
            "no log group (billed), no alarm prefix, no RDS without evidence"
        );
        d.discovery.rds_found = vec![("shop-db".into(), "DB_HOST".into())];
        assert_eq!(preview_spec(&d).unwrap().rds, ["shop-db"]);
        d.discovery.rds_found.clear();
        d.discovery.cache_found.clear();
        assert_eq!(preview_spec(&d), None, "nothing named, nothing previewed");
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn only_endpoint_hostnames_leave_the_env_var_scan() {
        let vars = vec![
            (
                "REDIS_HOST".to_string(),
                "master.shop-sessions.abc123.usw1.cache.amazonaws.com".to_string(),
            ),
            (
                "JDBC".to_string(),
                "jdbc:mysql://db-a.abc.us-west-1.rds.amazonaws.com:3306/app?pw=hunter2".to_string(),
            ),
            ("DB_PASSWORD".to_string(), "hunter2".to_string()),
            ("API".to_string(), "https://example.com".to_string()),
        ];
        let hosts = endpoint_hosts(&vars);
        assert_eq!(
            hosts,
            [
                (
                    "REDIS_HOST".to_string(),
                    "master.shop-sessions.abc123.usw1.cache.amazonaws.com".to_string()
                ),
                (
                    "JDBC".to_string(),
                    "db-a.abc.us-west-1.rds.amazonaws.com".to_string()
                ),
            ]
        );
        assert!(
            !format!("{hosts:?}").contains("hunter2"),
            "no other value survives"
        );
    }

    #[test]
    fn endpoints_name_their_instance_group_or_nothing() {
        assert_eq!(
            rds_instance_of("db-a.abc.us-west-1.rds.amazonaws.com"),
            Some("db-a")
        );
        assert_eq!(
            rds_instance_of("aur.cluster-abc.us-west-1.rds.amazonaws.com"),
            None,
            "a cluster"
        );
        assert_eq!(
            rds_instance_of("aur.cluster-ro-abc.us-west-1.rds.amazonaws.com"),
            None
        );
        assert_eq!(
            cache_target_of("master.shop-sessions.abc123.usw1.cache.amazonaws.com"),
            Some("shop-sessions")
        );
        assert_eq!(
            cache_target_of("replica.g.abc123.usw1.cache.amazonaws.com"),
            Some("g")
        );
        assert_eq!(
            cache_target_of("clustercfg.g.abc123.usw1.cache.amazonaws.com"),
            Some("g")
        );
        assert_eq!(
            cache_target_of("g-001.abc123.0001.usw1.cache.amazonaws.com"),
            Some("g-001")
        );
        assert_eq!(
            cache_target_of("c-abc.serverless.usw1.cache.amazonaws.com"),
            None
        );
    }

    fn inputs() -> Inputs {
        Inputs {
            log_groups: s(&[
                "/aws/elasticbeanstalk/Shop-prod/var/log/eb-engine.log",
                "/aws/elasticbeanstalk/Shop-prod/var/log/eb-hooks.log",
                "/aws/elasticbeanstalk/Shop-prod/var/log/httpd/access_log",
                "/aws/elasticbeanstalk/Shop-prod/var/log/nginx/error.log",
                "/aws/elasticbeanstalk/Shop-prod/var/log/tomcat9/shop-main.log",
            ]),
            endpoint_hosts: vec![
                (
                    "REDIS".into(),
                    "master.sessions.x.usw1.cache.amazonaws.com".into(),
                ),
                ("DB".into(), "shop-db.x.us-west-1.rds.amazonaws.com".into()),
            ],
            rds_ids: s(&["shop-db", "other-db", "shop-db"]),
            cache_ids: s(&[
                "sessions-001",
                "sessions-0002-001",
                "sessionsx-001",
                "sessions-eu-001",
                "sessions-2-001",
                "sessions-12",
                "other-001",
            ]),
            prefixed_alarms: s(&["Shop-prod-5xx", "Shop-prod-wkld-cpu", "Shop-prod-wk-health"]),
            fleet: s(&["Shop-prod", "Shop-prod-wk"]),
            custom_metrics: vec![],
        }
    }

    #[test]
    fn the_envs_evidence_becomes_suggestions() {
        let d = discover("Shop-prod", &inputs());
        assert_eq!(d.log_groups.len(), 5);
        assert_eq!(d.app_log.as_deref(), Some("var/log/tomcat9/shop-main.log"));
        assert_eq!(
            d.alarm_prefix,
            Some(("Shop-prod-".to_string(), 2)),
            "the worker's alarm is not counted"
        );
        assert_eq!(
            d.rds_options,
            ["other-db", "shop-db"],
            "every instance offered, deduplicated"
        );
        assert_eq!(d.rds_found, [("shop-db".to_string(), "DB".to_string())]);
        assert_eq!(d.cache_options.len(), 7);
        assert_eq!(
            d.cache_found,
            [
                ("sessions-001".to_string(), "REDIS".to_string()),
                ("sessions-0002-001".to_string(), "REDIS".to_string()),
            ],
            "the group's nodes, not sessionsx-001 or another group's sessions-eu-001"
        );
    }

    #[test]
    fn nothing_is_suggested_without_evidence() {
        let mut i = inputs();
        i.endpoint_hosts.clear();
        i.prefixed_alarms = s(&["Shop-prod-wk-health"]);
        i.log_groups
            .push("/aws/elasticbeanstalk/Shop-prod/var/log/web.stdout.log".into());
        let d = discover("Shop-prod", &i);
        assert!(
            d.rds_found.is_empty() && d.cache_found.is_empty(),
            "options only"
        );
        assert!(!d.rds_options.is_empty(), "but still offered");
        assert_eq!(d.alarm_prefix, None, "only the worker's alarms");
        assert_eq!(
            d.app_log, None,
            "two candidate app logs: the operator picks"
        );
        // An instance the variable names but CloudWatch has no metrics for.
        let mut i = inputs();
        i.rds_ids = s(&["other-db"]);
        assert!(discover("Shop-prod", &i).rds_found.is_empty());
    }

    #[test]
    fn metrics_dimensioned_by_the_env_suggest_the_app_namespace() {
        let m = |ns: &str, name: &str, dims: &[(&str, &str)]| CustomMetric {
            namespace: ns.into(),
            name: name.into(),
            dimensions: dims
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        };
        let mut i = inputs();
        i.custom_metrics = vec![
            m("Shop", "usersActive", &[("Environment", "Shop-prod")]),
            m(
                "Shop",
                "usersActive",
                &[("Environment", "Shop-prod"), ("InstanceId", "i-a")],
            ),
            m(
                "Shop",
                "heapUsedMb",
                &[("Environment", "Shop-prod"), ("InstanceId", "i-a")],
            ),
            m("Shop", "usersActive", &[("Environment", "Shop-demo")]),
            m("Other", "x", &[("Env", "Shop-prod")]),
        ];
        let found = discover("Shop-prod", &i).app_metrics.expect("found");
        assert_eq!(found.namespace, "Shop");
        assert_eq!(
            found.dimension,
            ("Environment".to_string(), "Shop-prod".to_string())
        );
        assert_eq!(found.instance_dimension.as_deref(), Some("InstanceId"));
        assert_eq!(found.names, ["heapUsedMb", "usersActive"]);
        assert_eq!(discover("Nope", &i).app_metrics, None);
    }
}
