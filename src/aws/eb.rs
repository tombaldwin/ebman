//! Elastic Beanstalk itself — the domain this tool is built around.
//!
//! Everything here is EB-shaped: environments and their health, the
//! application/version model, option settings, saved-configuration
//! templates, platform upgrades. The other `aws/*` modules are the
//! generic AWS surface any operator TUI would want; this one is the
//! part that makes ebman specifically an Elastic Beanstalk tool.

use super::*;

#[derive(Clone, Debug)]
pub struct Event {
    pub at: Option<DateTime<Utc>>,
    pub env: String,
    pub application: String,
    pub message: String,
    pub severity: String,
    /// Application version label this event relates to, when EB
    /// tags it (deploy events carry it). `None` for events with no
    /// associated version. Drives `:rollback`'s previous-version
    /// detection.
    pub version_label: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Instance {
    pub id: String,
    pub health: String, // Ok / Warning / Degraded / Severe / Info / NoData / Unknown / Pending
    pub color: String,  // Green / Yellow / Red / Grey
    pub causes: Vec<String>,
    pub instance_type: String,
    pub availability_zone: String,
    pub launched_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct Application {
    pub name: String,
    pub description: String,
    /// Surfaced in the `:apps-info` overlay (in operator timezone)
    /// alongside `date_updated`. Was orphaned briefly when the apps
    /// table dropped its CREATED column in 0.3.3.
    pub date_created: Option<DateTime<Utc>>,
    pub date_updated: Option<DateTime<Utc>>,
    pub version_count: usize,
    pub templates: Vec<String>,
    /// Newest application version's label (from `DescribeApplicationVersions`,
    /// sorted by date_created desc). Populated by a follow-up fetch after
    /// `list_applications` — `None` while still loading or if the app has
    /// no versions yet. The EB-console "latest deployed version" matches
    /// this field, not the application-level `date_updated`.
    pub latest_version_label: Option<String>,
    /// `date_created` of the newest application version.
    pub latest_version_created: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct CustomPlatform {
    pub arn: String,
    pub branch: String,
    pub version: String,
    pub status: String,
    pub lifecycle: String,
}

#[derive(Clone, Debug)]
pub struct AppVersion {
    pub label: String,
    pub description: String,
    pub created: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct Environment {
    pub name: String,
    pub application: String,
    pub status: String,
    pub health: String,
    pub platform: String, // family + version, e.g. "Java 17"
    /// Raw solution-stack name as reported by EB, e.g. `64bit Amazon Linux
    /// 2023 v6.1.0 running Node.js 18`. Empty for platform-ARN / custom-
    /// platform envs that don't report a solution stack. Drives the
    /// stale-platform comparison against `ListAvailableSolutionStacks`.
    pub solution_stack: String,
    pub tier: String, // "Web" / "Worker" / "?"
    pub cname: String,
    pub version_label: String,
    pub arn: Option<String>,
    pub updated: Option<DateTime<Utc>>,
    /// Internal EB environment ID (e.g. `e-abcdef1234`). Required by APIs
    /// that snapshot config from a live env (CreateConfigurationTemplate).
    pub id: Option<String>,
    /// Region the env was discovered in, when results were fanned out across
    /// multiple regions. `None` in single-region mode.
    pub region: Option<String>,
}

/// Per-env summary of instance health, as surfaced in the `INST` column
/// of the main env table. `healthy` is the count EB classifies as Green
/// (Ok + Info — both are "passing health checks", Info just means an
/// operation is in progress on an otherwise-healthy instance); `total`
/// is the sum across every health bucket the env reports. `total == 0`
/// is a real signal (env has no instances right now, e.g. mid-launch),
/// rendered as `0/0` in the table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EnvInstanceCounts {
    pub healthy: i32,
    pub total: i32,
}

/// Parsed `DescribeEnvironmentResources` payload. The SDK returns
/// flat lists; we hold them in field-typed buckets so the
/// `:resources` renderer can format them as a hierarchical tree
/// (ASG → instances → LB → queues etc.) without re-traversing
/// the raw API shape.
#[derive(Clone, Debug, Default)]
pub struct EnvResources {
    pub asgs: Vec<String>,
    pub instances: Vec<String>,
    pub launch_configs: Vec<String>,
    pub launch_templates: Vec<String>,
    pub load_balancers: Vec<String>,
    pub triggers: Vec<String>,
    pub queues: Vec<EnvResourceQueue>,
}

#[derive(Clone, Debug)]
pub struct EnvResourceQueue {
    pub name: String,
    pub url: String,
}

/// One settable EB configuration option, as returned by
/// [`AwsClient::fetch_env_configuration_options`]. Covers both the
/// operator's currently-set value and the platform's metadata
/// (default / constraints / change severity). `:options` uses this
/// to render the full config vocabulary in one overlay.
#[derive(Clone, Debug)]
pub struct ConfigOption {
    pub namespace: String,
    pub name: String,
    /// Current value, or `None` when the operator hasn't overridden
    /// the default. EB sometimes returns `Some("")` for unset; the
    /// renderer treats both as "default" and tags accordingly.
    pub value: Option<String>,
    pub default_value: Option<String>,
    /// `"Scalar"` / `"List"` / sometimes blank. Lower-cased on
    /// the wire; we render as-is.
    pub value_type: String,
    /// Constrained value options for enum-shaped settings
    /// (e.g. `["AllAtOnce", "Rolling", "Immutable", ...]` for
    /// `DeploymentPolicy`). Empty Vec when unconstrained.
    pub value_options: Vec<String>,
    /// `"NoInterruption"` / `"RestartEnvironment"` /
    /// `"RestartApplicationServer"` / `"Unknown"`. Warns the
    /// operator that changing this option will roll instances.
    pub change_severity: Option<String>,
    /// EB exposes a "this option is operator-settable" flag —
    /// most options have this true. Currently captured but not
    /// rendered (operator-set vs default distinction is enough
    /// signal); kept on the struct because a future "hide read-only
    /// options" filter would consume it.
    #[allow(dead_code)]
    pub user_defined: Option<bool>,
    pub min_value: Option<i32>,
    pub max_value: Option<i32>,
    pub max_length: Option<i32>,
}

pub(super) fn map_env(e: aws_sdk_elasticbeanstalk::types::EnvironmentDescription) -> Environment {
    let solution_stack = e.solution_stack_name.clone().unwrap_or_default();
    let raw_platform = e
        .solution_stack_name
        .clone()
        .or(e.platform_arn.clone())
        .unwrap_or_default();
    let tier = e
        .tier
        .as_ref()
        .and_then(|t| t.name.as_deref())
        .map(normalize_tier)
        .unwrap_or_else(|| "?".into());
    Environment {
        name: e.environment_name.unwrap_or_default(),
        application: e.application_name.unwrap_or_default(),
        status: e
            .status
            .map(|s| s.as_str().to_string())
            .unwrap_or_else(|| "-".into()),
        health: e
            .health
            .map(|h| h.as_str().to_string())
            .unwrap_or_else(|| "-".into()),
        platform: platform_family(&raw_platform),
        solution_stack,
        tier,
        cname: e.cname.unwrap_or_default(),
        version_label: e.version_label.unwrap_or_default(),
        arn: e.environment_arn,
        updated: e
            .date_updated
            .and_then(|d| DateTime::from_timestamp(d.secs(), d.subsec_nanos())),
        id: e.environment_id,
        region: None,
    }
}

/// Fan-out helper: build a transient `AwsClient` for `region` (sharing the
/// caller's profile) and pull `DescribeEnvironments` from there. Each
/// returned env has `region` stamped so the table can sort / group on it.
/// Best-effort extraction of the EB platform branch name from a solution
/// stack name or platform ARN. The names look like `64bit Amazon Linux 2023
/// v4.5.2 running Tomcat 9 Corretto 17` — we keep the "running …" tail and
/// strip any leading "running " marker. ARNs follow a separate scheme and
/// already carry the branch in their path.
pub(crate) fn platform_branch_from(stack_or_arn: &str) -> String {
    // ARN first — every real platform ARN's name segment itself
    // contains " running " (e.g. ".../platform/Python 3.9 running on
    // 64bit Amazon Linux 2023/4.0.1"), so the solution-stack split
    // below would mangle it into "on 64bit …". The second-to-last
    // path segment IS the full branch name.
    if stack_or_arn.starts_with("arn:") {
        let parts: Vec<&str> = stack_or_arn.split('/').collect();
        if parts.len() >= 2 {
            return parts[parts.len() - 2].to_string();
        }
        return String::new();
    }
    // Solution stack ("64bit Amazon Linux 2023 v4.0.1 running
    // Python 3.9") yields the branch FAMILY ("Python 3.9"). Real
    // branch names are "<family> running on <os>" — which is why the
    // PlatformBranchName filter uses begins_with, not `=` (an exact
    // match against the bare family matched nothing, so `:upgrade`
    // always reported an empty compatible-platform list).
    if let Some(rest) = stack_or_arn.split(" running ").nth(1) {
        return rest.trim().to_string();
    }
    String::new()
}

/// Compare two dotted version strings semver-ish. Numeric tokens compared
/// numerically; non-numeric tails fall back to string comparison. Returns
/// `Ordering` so this can drive `sort_by`.
pub(super) fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    // Split off any pre-release suffix at the first `-`. Solution-stack
    // versions never have one (`stack_family_version` only accepts
    // all-digit dot parts), so this only matters for operator-authored
    // custom platform versions.
    fn split(s: &str) -> (&str, Option<&str>) {
        match s.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (s, None),
        }
    }
    let (a_core, a_pre) = split(a);
    let (b_core, b_pre) = split(b);

    let parse = |s: &str| {
        s.split('.')
            .map(|p| p.parse::<u64>().ok())
            .collect::<Vec<_>>()
    };
    let av = parse(a_core);
    let bv = parse(b_core);
    for i in 0..av.len().max(bv.len()) {
        let aa = av.get(i).and_then(|x| *x);
        let bb = bv.get(i).and_then(|x| *x);
        match (aa, bb) {
            (Some(x), Some(y)) => match x.cmp(&y) {
                Ordering::Equal => continue,
                o => return o,
            },
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => break,
        }
    }
    // Cores tie. Semver: a pre-release ranks BELOW the release it
    // precedes, so `1.0.0-rc1` must not be offered as newer than
    // `1.0.0` in the platform-upgrade picker. The old code fell
    // straight through to `a.cmp(b)` here, and lexicographically
    // "1.0.0-rc1" > "1.0.0" because it's a prefix extension.
    match compare_prerelease(a_pre, b_pre) {
        Ordering::Equal => a.cmp(b),
        o => o,
    }
}

/// Semver pre-release precedence, for two versions whose cores are equal.
///
/// Absent beats present (a release outranks its own pre-release), then
/// dot-separated identifiers compare left to right: numeric ones
/// numerically, numeric below alphanumeric, alphanumeric ASCII-wise, and
/// a shorter identifier list below a longer one when all else ties.
fn compare_prerelease(a: Option<&str>, b: Option<&str>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (a, b) = match (a, b) {
        (None, None) => return Ordering::Equal,
        (None, Some(_)) => return Ordering::Greater,
        (Some(_), None) => return Ordering::Less,
        (Some(a), Some(b)) => (a, b),
    };
    let mut ai = a.split('.');
    let mut bi = b.split('.');
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return Ordering::Equal,
            // Fewer identifiers ranks below more, all else equal.
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let o = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(nx), Ok(ny)) => nx.cmp(&ny),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    (Err(_), Err(_)) => x.cmp(y),
                };
                if o != Ordering::Equal {
                    return o;
                }
            }
        }
    }
}

/// Pure: roll up EB's per-bucket `InstanceHealthSummary` into the
/// `(healthy, total)` shape the INST column wants. `healthy` is `ok +
/// info` (both Green per EB's docs — Info just means an operation is in
/// progress on an otherwise-healthy instance, not a problem signal).
/// `total` is the sum across every bucket including Grey buckets like
/// `no_data` / `unknown` / `pending` so an env that's mid-launch
/// reports `0/N` rather than `0/0`. Missing input (`None`) and
/// all-None buckets render as `EnvInstanceCounts::default()` (0/0).
pub fn summarise_instance_health(
    summary: Option<&aws_sdk_elasticbeanstalk::types::InstanceHealthSummary>,
) -> EnvInstanceCounts {
    let Some(s) = summary else {
        return EnvInstanceCounts::default();
    };
    let g = |v: Option<i32>| v.unwrap_or(0);
    let ok = g(s.ok);
    let info = g(s.info);
    let healthy = ok + info;
    let total = g(s.no_data)
        + g(s.unknown)
        + g(s.pending)
        + ok
        + info
        + g(s.warning)
        + g(s.degraded)
        + g(s.severe);
    EnvInstanceCounts { healthy, total }
}

/// Split a solution-stack name into `(family_key, version)`. The family key
/// is the stack name with its `vX.Y.Z` token removed and surrounding
/// whitespace collapsed, so two stacks that differ only in version share a
/// key (e.g. `64bit Amazon Linux 2023 v6.1.0 running Node.js 18` →
/// `("64bit Amazon Linux 2023 running Node.js 18", "6.1.0")`). Returns
/// `None` when no `vN.N…` token is present — platform-ARN / custom-platform
/// envs have no solution stack and so can't be version-compared.
pub fn stack_family_version(stack: &str) -> Option<(String, String)> {
    let version_token = stack.split_whitespace().find(|tok| {
        tok.strip_prefix('v')
            .map(|rest| {
                !rest.is_empty()
                    && rest
                        .split('.')
                        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
            })
            .unwrap_or(false)
    })?;
    let version = version_token.trim_start_matches('v').to_string();
    let key = stack
        .split_whitespace()
        .filter(|tok| *tok != version_token)
        .collect::<Vec<_>>()
        .join(" ");
    Some((key, version))
}

/// Build a `family_key → newest version` map from a flat
/// `ListAvailableSolutionStacks` listing. Stacks with no version token are
/// skipped.
pub fn latest_stack_versions(stacks: &[String]) -> std::collections::HashMap<String, String> {
    let mut out: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for s in stacks {
        if let Some((key, ver)) = stack_family_version(s) {
            match out.get(&key) {
                Some(cur) if compare_versions(&ver, cur) != std::cmp::Ordering::Greater => {}
                _ => {
                    out.insert(key, ver);
                }
            }
        }
    }
    out
}

/// If a strictly-newer version of `env_stack`'s platform family exists in
/// `latest`, return that version. `None` when the env is already current,
/// has no parseable stack, or its family isn't in the listing.
pub fn newer_stack_version(
    env_stack: &str,
    latest: &std::collections::HashMap<String, String>,
) -> Option<String> {
    let (key, ver) = stack_family_version(env_stack)?;
    let newest = latest.get(&key)?;
    if compare_versions(newest, &ver) == std::cmp::Ordering::Greater {
        Some(newest.clone())
    } else {
        None
    }
}

pub async fn list_environments_in_region(
    profile: Option<String>,
    region: String,
) -> Result<Vec<Environment>> {
    let client = AwsClient::with(profile, Some(region.clone())).await?;
    let mut envs = client.list_environments().await?;
    for e in &mut envs {
        e.region = Some(region.clone());
    }
    Ok(envs)
}

/// Sibling of `list_environments_in_region` for the AssumeRole path:
/// assumes into the named role, then lists envs. `region` overrides the
/// AccountSpec's region when supplied; otherwise the spec's own region
/// wins (or env default). Used by the multi-account fan-out in
/// `:org-health` / `:find-env`.
pub async fn list_environments_for_account(
    name: &str,
    spec: &crate::config::AccountSpec,
    region: Option<String>,
) -> Result<Vec<Environment>> {
    let mut spec = spec.clone();
    if region.is_some() {
        spec.region = region.clone();
    }
    let client = AwsClient::assume_role(name, &spec).await?;
    let resolved_region = client.context.region.clone();
    let mut envs = client.list_environments().await?;
    for e in &mut envs {
        e.region = Some(resolved_region.clone());
    }
    Ok(envs)
}

/// Pulls the family + version out of either a solution_stack_name like
/// "64bit Amazon Linux 2 v3.7.0 running Tomcat 9 Corretto 17"  → "Tomcat 9 Corretto 17"
/// or a platform_arn like
/// "arn:aws:elasticbeanstalk:us-east-1::platform/Java 17 running on 64bit Amazon Linux 2/3.5.0"
///   → "Java 17"
pub(crate) fn platform_family(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    // Platform ARN form: "...platform/Family X running on 64bit Amazon Linux/3.5.0"
    // The interesting segment lives between '/' separators and contains " running on ".
    if raw.contains(" running on ") {
        for seg in raw.split('/') {
            if let Some((family, _)) = seg.split_once(" running on ") {
                return family.trim().to_string();
            }
        }
    }
    // Solution-stack form: "...64bit Amazon Linux 2 v3.5.0 running Family X"
    if let Some((_, after)) = raw.rsplit_once(" running ") {
        return after.trim().to_string();
    }
    raw.to_string()
}

pub(crate) fn normalize_tier(name: &str) -> String {
    match name {
        "WebServer" => "Web".into(),
        "Worker" => "Worker".into(),
        other => other.to_string(),
    }
}

#[derive(Clone, Debug, Default)]
pub struct WorkerQueues {
    pub main_url: Option<String>,
    pub dlq_url: Option<String>,
    pub main_stats: Option<QueueStats>,
    pub dlq_stats: Option<QueueStats>,
}

/// Result of `fetch_env_vpc_context` — the env's VPC plus the option-
/// settings selections the `:subnets` / `:elb-subnets` / `:security-groups`
/// pickers need for their pre-fill. Each field is `None` / empty when the
/// env doesn't override that option (EB uses its account-default in that
/// case).
#[derive(Clone, Debug, Default)]
pub struct EnvVpcContext {
    pub vpc_id: Option<String>,
    pub subnets: Vec<String>,
    /// ELB subnets (`aws:ec2:vpc.ELBSubnets`). Web-tier envs typically
    /// attach the ELB to a separate subnet set than the instance subnets;
    /// worker envs leave this empty.
    pub elb_subnets: Vec<String>,
    pub security_groups: Vec<String>,
}

impl AwsClient {
    pub async fn list_events(&self, max: i32) -> Result<Vec<Event>> {
        self.list_events_inner(None, None, max).await
    }

    pub async fn list_events_for_env(&self, env_name: &str, max: i32) -> Result<Vec<Event>> {
        self.list_events_inner(Some(env_name.to_string()), None, max)
            .await
    }

    /// Fleet-wide events newer than `since_ms` (epoch millis) — the
    /// `:event-tail` polling primitive. `start_time` keeps each poll's
    /// batch small so a busy fleet doesn't re-ship its whole history
    /// every cycle.
    pub async fn list_events_since(&self, since_ms: i64, max: i32) -> Result<Vec<Event>> {
        self.list_events_inner(None, Some(since_ms), max).await
    }

    async fn list_events_inner(
        &self,
        env_name: Option<String>,
        since_ms: Option<i64>,
        max: i32,
    ) -> Result<Vec<Event>> {
        let mut req = self.client.describe_events().max_records(max);
        if let Some(n) = env_name {
            req = req.environment_name(n);
        }
        if let Some(ms) = since_ms {
            req = req.start_time(aws_sdk_elasticbeanstalk::primitives::DateTime::from_millis(
                ms,
            ));
        }
        let resp = req.send().await?;
        let events = resp
            .events
            .unwrap_or_default()
            .into_iter()
            .map(|e| Event {
                at: e
                    .event_date
                    .and_then(|d| DateTime::from_timestamp(d.secs(), d.subsec_nanos())),
                env: e.environment_name.unwrap_or_default(),
                application: e.application_name.unwrap_or_default(),
                message: e.message.unwrap_or_default(),
                severity: e
                    .severity
                    .map(|s| s.as_str().to_string())
                    .unwrap_or_else(|| "INFO".to_string()),
                version_label: e.version_label.filter(|v| !v.is_empty()),
            })
            .collect();
        Ok(events)
    }

    /// Full `DescribeEnvironmentResources` dump for an env, formatted as a
    /// human-readable string suitable for an overlay. Covers ASGs,
    /// instances, launch configurations, launch templates, load balancers,
    /// trigger names, and SQS queues — i.e. every infra resource EB
    /// manages for the env. Useful for "what's actually under this env?".
    /// Fetch the env's underlying AWS resources (ASGs, instances,
    /// launch config/template, load balancers, triggers, queues).
    /// Returns the parsed shape so the renderer can format as a
    /// hierarchical tree rather than a flat dump.
    pub async fn describe_env_resources(&self, env_name: &str) -> Result<EnvResources> {
        let resp = self
            .client
            .describe_environment_resources()
            .environment_name(env_name)
            .send()
            .await
            .wrap_err("DescribeEnvironmentResources failed")?;
        let res = resp
            .environment_resources
            .ok_or_else(|| eyre!("no environment_resources in response"))?;
        Ok(EnvResources {
            asgs: res
                .auto_scaling_groups
                .unwrap_or_default()
                .into_iter()
                .filter_map(|a| a.name)
                .collect(),
            instances: res
                .instances
                .unwrap_or_default()
                .into_iter()
                .filter_map(|i| i.id)
                .collect(),
            launch_configs: res
                .launch_configurations
                .unwrap_or_default()
                .into_iter()
                .filter_map(|l| l.name)
                .collect(),
            launch_templates: res
                .launch_templates
                .unwrap_or_default()
                .into_iter()
                .filter_map(|l| l.id)
                .collect(),
            load_balancers: res
                .load_balancers
                .unwrap_or_default()
                .into_iter()
                .filter_map(|l| l.name)
                .collect(),
            triggers: res
                .triggers
                .unwrap_or_default()
                .into_iter()
                .filter_map(|t| t.name)
                .collect(),
            queues: res
                .queues
                .unwrap_or_default()
                .into_iter()
                .filter_map(|q| {
                    let name = q.name?;
                    Some(EnvResourceQueue {
                        name,
                        url: q.url.unwrap_or_default(),
                    })
                })
                .collect(),
        })
    }

    /// Resolve the worker queue URL (and DLQ URL) for an env. EB autocreates
    /// queues when the user doesn't override `WorkerQueueURL`, and in that
    /// (common) case the option value comes back empty — so we ask
    /// `DescribeEnvironmentResources` first, which exposes the actual queue
    /// URLs under named entries (`WorkerQueue`, `WorkerDeadLetterQueue`).
    /// Falls back to the option-settings path for users who override the
    /// URL explicitly.
    pub async fn describe_worker_queues(
        &self,
        application_name: &str,
        env_name: &str,
    ) -> Result<WorkerQueues> {
        let mut main_url: Option<String> = None;
        let mut dlq_url: Option<String> = None;
        // Errors must stay distinguishable from "this env has no
        // queues": the pre-0.27 shape swallowed every failure into
        // an empty result, so an AccessDenied rendered as "no worker
        // queues" and silently blinded DLQ red-alerting.
        let mut discovery_err: Option<String> = None;

        // Primary path: ask EB for the env's resources. Includes the URLs of
        // the queues EB created automatically when WorkerQueueURL is empty.
        match self
            .client
            .describe_environment_resources()
            .environment_name(env_name)
            .send()
            .await
        {
            Ok(resp) => {
                if let Some(res) = resp.environment_resources {
                    for q in res.queues.unwrap_or_default() {
                        let name = q.name.unwrap_or_default();
                        let url = q.url.unwrap_or_default();
                        if url.is_empty() {
                            continue;
                        }
                        match name.as_str() {
                            "WorkerQueue" => main_url = Some(url),
                            "WorkerDeadLetterQueue" => dlq_url = Some(url),
                            _ => {}
                        }
                    }
                }
            }
            Err(e) => discovery_err = Some(format!("DescribeEnvironmentResources: {e}")),
        }

        // Fallback / override: look at user-supplied option settings in case
        // the env explicitly points at a queue the user manages outside EB.
        if main_url.is_none() || dlq_url.is_none() {
            match self
                .client
                .describe_configuration_settings()
                .application_name(application_name)
                .environment_name(env_name)
                .send()
                .await
            {
                Err(e) => {
                    // Record the fallback failure too — resolution
                    // below decides whether it matters.
                    let msg = format!("DescribeConfigurationSettings: {e}");
                    discovery_err = Some(match discovery_err.take() {
                        Some(prior) => format!("{prior} + {msg}"),
                        None => msg,
                    });
                }
                Ok(resp) => {
                    for setting in resp.configuration_settings.unwrap_or_default() {
                        for opt in setting.option_settings.unwrap_or_default() {
                            let ns = opt.namespace.unwrap_or_default();
                            let name = opt.option_name.unwrap_or_default();
                            if ns != "aws:elasticbeanstalk:sqsd" {
                                continue;
                            }
                            match name.as_str() {
                                "WorkerQueueURL" => {
                                    let v = opt.value.unwrap_or_default();
                                    if !v.is_empty() && main_url.is_none() {
                                        main_url = Some(v);
                                    }
                                }
                                "DeadLetterQueueURL" => {
                                    let v = opt.value.unwrap_or_default();
                                    if !v.is_empty() && dlq_url.is_none() {
                                        dlq_url = Some(v);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }

        // A discovery error with nothing found must surface as an
        // error: "no queues" is only trustworthy when at least one
        // discovery call succeeded AND we found nothing — a failed
        // primary may have hidden real EB-created queues (0.27
        // re-review: the first cut only errored when BOTH calls
        // failed, so AccessDenied-on-primary + empty-fallback — the
        // common autocreated-queue case — still read as "no queues"
        // and silently cleared DLQ alerting).
        if main_url.is_none() {
            if let Some(err) = discovery_err {
                return Err(eyre!(err));
            }
        }

        // If we still have a main queue but no DLQ URL, derive one by SQS naming convention.
        if let (Some(main), None) = (&main_url, &dlq_url) {
            dlq_url = derive_dlq_url(main);
        }

        // Stats failures must stay distinguishable from "queue empty"
        // / "no DLQ": SQS permissions are separate from EB's, and an
        // AccessDenied here previously produced dlq_stats=None → the
        // depth cache treated it as "no DLQ" and cleared the alert.
        // NonExistentQueue on the DERIVED DLQ url is the one genuine
        // "no DLQ" error (the naming-convention guess missed).
        let main_stats = match &main_url {
            Some(u) => match self.queue_stats(u).await {
                Ok(st) => Some(st),
                Err(e) => {
                    let text = format!("{e:#}");
                    if text.contains("NonExistentQueue") {
                        None
                    } else {
                        return Err(eyre!("main queue stats: {text}"));
                    }
                }
            },
            None => None,
        };
        let dlq_stats = match &dlq_url {
            Some(u) => match self.queue_stats(u).await {
                Ok(st) => Some(st),
                Err(e) => {
                    let text = format!("{e:#}");
                    if text.contains("NonExistentQueue") {
                        None
                    } else {
                        return Err(eyre!("dlq stats: {text}"));
                    }
                }
            },
            None => None,
        };

        Ok(WorkerQueues {
            main_url,
            dlq_url,
            main_stats,
            dlq_stats,
        })
    }

    /// Fetch the current env vars for an environment from
    /// `DescribeConfigurationSettings` filtered to the
    /// `aws:elasticbeanstalk:application:environment` namespace. Returns
    /// sorted `(KEY, VALUE)` pairs.
    /// Fetch every option setting for a live env. Used by the modal-form
    /// pre-fill: callers filter the result down to the `(namespace, option_name)`
    /// pairs their form cares about. Returns `(namespace, option_name, value)`
    /// triples.
    pub async fn fetch_env_option_settings(
        &self,
        application_name: &str,
        env_name: &str,
    ) -> Result<Vec<(String, String, String)>> {
        let resp = self
            .client
            .describe_configuration_settings()
            .application_name(application_name)
            .environment_name(env_name)
            .send()
            .await
            .wrap_err("DescribeConfigurationSettings(env) failed")?;
        let out = resp
            .configuration_settings
            .unwrap_or_default()
            .into_iter()
            .flat_map(|c| c.option_settings.unwrap_or_default())
            .map(|o| {
                (
                    o.namespace.unwrap_or_default(),
                    o.option_name.unwrap_or_default(),
                    o.value.unwrap_or_default(),
                )
            })
            .collect();
        Ok(out)
    }

    /// Pull the env's VPC id plus the currently-selected subnet and
    /// security-group IDs from EB option settings in a single round-trip.
    /// `:subnets` and `:security-groups` both call this — VPC id drives
    /// the subsequent EC2 list call, the existing selections drive the
    /// MultiSelect pre-fill.
    pub async fn fetch_env_vpc_context(
        &self,
        application_name: &str,
        env_name: &str,
    ) -> Result<EnvVpcContext> {
        let resp = self
            .client
            .describe_configuration_settings()
            .application_name(application_name)
            .environment_name(env_name)
            .send()
            .await
            .wrap_err("DescribeConfigurationSettings(env) failed")?;
        let mut ctx = EnvVpcContext::default();
        for setting in resp.configuration_settings.unwrap_or_default() {
            for opt in setting.option_settings.unwrap_or_default() {
                let ns = opt.namespace.unwrap_or_default();
                let name = opt.option_name.unwrap_or_default();
                let value = opt.value.unwrap_or_default();
                match (ns.as_str(), name.as_str()) {
                    ("aws:ec2:vpc", "VPCId") if !value.is_empty() => {
                        ctx.vpc_id = Some(value);
                    }
                    ("aws:ec2:vpc", "Subnets") if !value.is_empty() => {
                        ctx.subnets = split_csv(&value);
                    }
                    ("aws:ec2:vpc", "ELBSubnets") if !value.is_empty() => {
                        ctx.elb_subnets = split_csv(&value);
                    }
                    ("aws:autoscaling:launchconfiguration", "SecurityGroups")
                        if !value.is_empty() =>
                    {
                        ctx.security_groups = split_csv(&value);
                    }
                    _ => {}
                }
            }
        }
        Ok(ctx)
    }

    /// Fetch RDS dbinstance option settings for an env. EB envs
    /// optionally have an attached RDS instance (via
    /// `aws:rds:dbinstance.*` option settings + auto-managed
    /// security group); this returns the configured settings as
    /// `(option_name, value)` pairs sorted alphabetically.
    ///
    /// Empty result = no RDS attached. Caller should distinguish
    /// "no RDS" from "fetch failed" via the Result type.
    pub async fn fetch_env_rds_config(
        &self,
        application_name: &str,
        env_name: &str,
    ) -> Result<Vec<(String, String)>> {
        let resp = self
            .client
            .describe_configuration_settings()
            .application_name(application_name)
            .environment_name(env_name)
            .send()
            .await
            .wrap_err("DescribeConfigurationSettings(rds) failed")?;
        let mut out: Vec<(String, String)> = resp
            .configuration_settings
            .unwrap_or_default()
            .into_iter()
            .flat_map(|c| c.option_settings.unwrap_or_default())
            .filter_map(|o| {
                let ns = o.namespace?;
                if ns != "aws:rds:dbinstance" {
                    return None;
                }
                let opt = o.option_name?;
                let value = o.value.unwrap_or_default();
                if value.is_empty() {
                    return None;
                }
                Some((opt, value))
            })
            .collect();
        out.sort();
        Ok(out)
    }

    /// Fetch every settable EB option for an env — namespace, name,
    /// current value (when set), default, type, constraints.
    ///
    /// Two SDK calls correlated by (namespace, name):
    ///
    ///   - `describe_configuration_options` is the canonical
    ///     "what's the full config vocabulary for this env's
    ///     platform?" API. Returns ~hundreds of option metadata
    ///     rows (default value, value type, change severity,
    ///     constraints) — but no current values.
    ///   - `describe_configuration_settings` returns the current
    ///     values for *every* option, including ones still at
    ///     their default.
    ///
    /// Merged on namespace+name so each row carries both the
    /// metadata and the live value. This is what closes the
    /// operator's "how do I know what I can set?" question.
    /// Caller should treat as on-demand (run via `:options`), not
    /// part of the background refresh — both calls are slow for
    /// platforms with deep option trees.
    pub async fn fetch_env_configuration_options(
        &self,
        application_name: &str,
        env_name: &str,
    ) -> Result<Vec<ConfigOption>> {
        // Parallel fetch of both shapes. The vocabulary call is
        // the slower of the two, so kicking them off together
        // shaves a round-trip off the total latency.
        let vocab_fut = self
            .client
            .describe_configuration_options()
            .environment_name(env_name)
            .send();
        let settings_fut = self
            .client
            .describe_configuration_settings()
            .application_name(application_name)
            .environment_name(env_name)
            .send();
        let (vocab_resp, settings_resp) = tokio::try_join!(
            async {
                vocab_fut
                    .await
                    .wrap_err("DescribeConfigurationOptions failed")
            },
            async {
                settings_fut
                    .await
                    .wrap_err("DescribeConfigurationSettings(options) failed")
            },
        )?;

        // Index current values by (namespace, name).
        let mut current: std::collections::HashMap<(String, String), String> =
            std::collections::HashMap::new();
        for c in settings_resp.configuration_settings.unwrap_or_default() {
            for o in c.option_settings.unwrap_or_default() {
                if let (Some(ns), Some(name)) = (o.namespace, o.option_name) {
                    if let Some(v) = o.value {
                        if !v.is_empty() {
                            current.insert((ns, name), v);
                        }
                    }
                }
            }
        }

        let mut out: Vec<ConfigOption> = vocab_resp
            .options
            .unwrap_or_default()
            .into_iter()
            .filter_map(|o| {
                let namespace = o.namespace?;
                let name = o.name?;
                let value = current.get(&(namespace.clone(), name.clone())).cloned();
                Some(ConfigOption {
                    namespace,
                    name,
                    value,
                    default_value: o.default_value,
                    value_type: o
                        .value_type
                        .map(|v| v.as_str().to_string())
                        .unwrap_or_default(),
                    value_options: o.value_options.unwrap_or_default(),
                    change_severity: o.change_severity,
                    user_defined: o.user_defined,
                    min_value: o.min_value,
                    max_value: o.max_value,
                    max_length: o.max_length,
                })
            })
            .collect();
        // Sort: namespace asc, user-set first within each namespace,
        // then alpha by name. Puts the operator's mutations at the
        // top of each group where they catch the eye.
        out.sort_by(|a, b| {
            let a_set = a.value.is_some();
            let b_set = b.value.is_some();
            a.namespace
                .cmp(&b.namespace)
                .then_with(|| b_set.cmp(&a_set))
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(out)
    }

    /// Fetch ALB listener option settings for an env. EB stores
    /// listener config in `aws:elbv2:listener:<PORT>` namespaces (one
    /// per listener; `default` is the port-80 HTTP listener, `443`
    /// is the typical HTTPS one). Returns a Vec of
    /// `(port_or_default, option_name, value)` rows so the renderer
    /// can group by port.
    ///
    /// Result is empty when the env doesn't use an ALB (Classic LB
    /// or worker tier) — caller should distinguish from "no config"
    /// by checking the env's tier first.
    pub async fn fetch_env_listeners(
        &self,
        application_name: &str,
        env_name: &str,
    ) -> Result<Vec<(String, String, String)>> {
        let resp = self
            .client
            .describe_configuration_settings()
            .application_name(application_name)
            .environment_name(env_name)
            .send()
            .await
            .wrap_err("DescribeConfigurationSettings(listeners) failed")?;
        let mut out: Vec<(String, String, String)> = resp
            .configuration_settings
            .unwrap_or_default()
            .into_iter()
            .flat_map(|c| c.option_settings.unwrap_or_default())
            .filter_map(|o| {
                let ns = o.namespace?;
                // Listener namespaces look like
                // `aws:elbv2:listener:default` / `aws:elbv2:listener:443`.
                // Strip the prefix to get the port (or "default").
                let port = ns.strip_prefix("aws:elbv2:listener:")?.to_string();
                let opt = o.option_name?;
                let value = o.value.unwrap_or_default();
                // Skip empty values — EB returns every settable key
                // even when unset, and an empty cert ARN / rule
                // list isn't worth showing.
                if value.is_empty() {
                    return None;
                }
                Some((port, opt, value))
            })
            .collect();
        // Sort: 'default' (port 80) first, then numeric ports asc,
        // then alpha by option name within each listener.
        out.sort_by(|a, b| {
            let rank_a = u8::from(a.0 != "default");
            let rank_b = u8::from(b.0 != "default");
            let port_a = a.0.parse::<u32>().unwrap_or(0);
            let port_b = b.0.parse::<u32>().unwrap_or(0);
            (rank_a, port_a, &a.1).cmp(&(rank_b, port_b, &b.1))
        });
        Ok(out)
    }

    pub async fn fetch_env_vars(
        &self,
        application_name: &str,
        env_name: &str,
    ) -> Result<Vec<(String, String)>> {
        let resp = self
            .client
            .describe_configuration_settings()
            .application_name(application_name)
            .environment_name(env_name)
            .send()
            .await
            .wrap_err("DescribeConfigurationSettings(env) failed")?;
        let mut out: Vec<(String, String)> = resp
            .configuration_settings
            .unwrap_or_default()
            .into_iter()
            .flat_map(|c| c.option_settings.unwrap_or_default())
            .filter(|o| {
                o.namespace.as_deref() == Some("aws:elasticbeanstalk:application:environment")
            })
            .map(|o| {
                (
                    o.option_name.unwrap_or_default(),
                    o.value.unwrap_or_default(),
                )
            })
            .collect();
        out.sort();
        Ok(out)
    }

    /// Update an env's option settings — `to_set` is `(namespace, option_name,
    /// value)` triples to add or overwrite; `to_remove` is `(namespace,
    /// option_name)` pairs to clear back to defaults. EB applies the change
    /// as a rolling update (or instantly for non-disruptive options).
    pub async fn update_env_option_settings(
        &self,
        env_name: &str,
        to_set: &[(String, String, String)],
        to_remove: &[(String, String)],
    ) -> Result<()> {
        use aws_sdk_elasticbeanstalk::types::{ConfigurationOptionSetting, OptionSpecification};
        if to_set.is_empty() && to_remove.is_empty() {
            return Err(eyre!("update_env_option_settings: nothing to do"));
        }
        let mut req = self.client.update_environment().environment_name(env_name);
        for (ns, name, value) in to_set {
            req = req.option_settings(
                ConfigurationOptionSetting::builder()
                    .namespace(ns)
                    .option_name(name)
                    .value(value)
                    .build(),
            );
        }
        for (ns, name) in to_remove {
            req = req.options_to_remove(
                OptionSpecification::builder()
                    .namespace(ns)
                    .option_name(name)
                    .build(),
            );
        }
        req.send()
            .await
            .wrap_err("UpdateEnvironment(option_settings) failed")?;
        Ok(())
    }

    pub async fn list_tags(&self, resource_arn: &str) -> Result<Vec<(String, String)>> {
        let resp = self
            .client
            .list_tags_for_resource()
            .resource_arn(resource_arn)
            .send()
            .await?;
        let tags = resp
            .resource_tags
            .unwrap_or_default()
            .into_iter()
            .filter_map(|t| match (t.key, t.value) {
                (Some(k), Some(v)) => Some((k, v)),
                _ => None,
            })
            .collect();
        Ok(tags)
    }

    /// UpdateTagsForResource — add/update tags listed in `to_add` and remove
    /// keys listed in `to_remove`. Empty lists are allowed but at least one
    /// side must be non-empty (the API rejects no-op calls).
    pub async fn update_tags(
        &self,
        resource_arn: &str,
        to_add: &[(String, String)],
        to_remove: &[String],
    ) -> Result<()> {
        use aws_sdk_elasticbeanstalk::types::Tag;
        let mut req = self
            .client
            .update_tags_for_resource()
            .resource_arn(resource_arn);
        for (k, v) in to_add {
            req = req.tags_to_add(Tag::builder().key(k).value(v).build());
        }
        for k in to_remove {
            req = req.tags_to_remove(k);
        }
        req.send().await?;
        Ok(())
    }

    pub async fn rebuild_env(&self, env_name: &str) -> Result<()> {
        self.client
            .rebuild_environment()
            .environment_name(env_name)
            .send()
            .await?;
        Ok(())
    }

    pub async fn restart_app_server(&self, env_name: &str) -> Result<()> {
        self.client
            .restart_app_server()
            .environment_name(env_name)
            .send()
            .await?;
        Ok(())
    }

    pub async fn swap_cnames(&self, source: &str, dest: &str) -> Result<()> {
        self.client
            .swap_environment_cnames()
            .source_environment_name(source)
            .destination_environment_name(dest)
            .send()
            .await?;
        Ok(())
    }

    /// Snapshot an env's current configuration as a named template under the
    /// same application. Idempotent for the user — if a template with the
    /// same name already exists, the API returns an error which we surface.
    pub async fn create_config_template(
        &self,
        application_name: &str,
        template_name: &str,
        source_env_name: &str,
    ) -> Result<()> {
        self.client
            .create_configuration_template()
            .application_name(application_name)
            .template_name(template_name)
            .environment_id(source_env_name)
            .send()
            .await
            .wrap_err("CreateConfigurationTemplate failed")?;
        Ok(())
    }

    /// Delete a configuration template by name. AWS will refuse if the
    /// template is currently in use; we pass the error back unchanged.
    pub async fn delete_config_template(
        &self,
        application_name: &str,
        template_name: &str,
    ) -> Result<()> {
        self.client
            .delete_configuration_template()
            .application_name(application_name)
            .template_name(template_name)
            .send()
            .await
            .wrap_err("DeleteConfigurationTemplate failed")?;
        Ok(())
    }

    /// List the newer platform versions in the same branch family as the
    /// env's current platform. Filtered server-side to `Ready` platforms;
    /// branch matching is best-effort using the current ARN's branch suffix
    /// (e.g. `Tomcat 9 with Corretto 17`). Sorted newest version first.
    pub async fn list_compatible_platforms(&self, env_name: &str) -> Result<Vec<CustomPlatform>> {
        use aws_sdk_elasticbeanstalk::types::{PlatformFilter, PlatformStatus};
        // Read the env's current platform ARN.
        let desc = self
            .client
            .describe_environments()
            .environment_names(env_name)
            .send()
            .await
            .wrap_err("DescribeEnvironments failed")?;
        let env = desc
            .environments
            .unwrap_or_default()
            .into_iter()
            .next()
            .ok_or_else(|| eyre!("env '{env_name}' not found"))?;
        let current_arn = env.platform_arn.clone().unwrap_or_default();
        let stack_or_arn = env
            .solution_stack_name
            .clone()
            .unwrap_or_else(|| current_arn.clone());
        let branch = platform_branch_from(&stack_or_arn);
        let owner_filter = PlatformFilter::builder()
            .r#type("PlatformStatus")
            .operator("=")
            .values(PlatformStatus::Ready.as_str())
            .build();
        let mut filters = vec![owner_filter];
        if !branch.is_empty() {
            filters.push(
                PlatformFilter::builder()
                    .r#type("PlatformBranchName")
                    // begins_with: `branch` is the bare family when
                    // derived from a solution-stack name, the full
                    // branch when derived from an ARN — both prefix
                    // the real PlatformBranchName.
                    .operator("begins_with")
                    .values(branch.clone())
                    .build(),
            );
        }
        let mut next_token: Option<String> = None;
        let mut out: Vec<CustomPlatform> = Vec::new();
        loop {
            let mut req = self.client.list_platform_versions();
            for f in &filters {
                req = req.filters(f.clone());
            }
            if let Some(t) = next_token.clone() {
                req = req.next_token(t);
            }
            let resp = req.send().await.wrap_err("ListPlatformVersions failed")?;
            for p in resp.platform_summary_list.unwrap_or_default() {
                out.push(CustomPlatform {
                    arn: p.platform_arn.unwrap_or_default(),
                    branch: p.platform_branch_name.unwrap_or_default(),
                    version: p.platform_version.unwrap_or_default(),
                    status: p
                        .platform_status
                        .map(|s| s.as_str().to_string())
                        .unwrap_or_default(),
                    lifecycle: p.platform_lifecycle_state.unwrap_or_default(),
                });
            }
            match resp.next_token {
                Some(t) if !t.is_empty() => next_token = Some(t),
                _ => break,
            }
        }
        // Sort newest-first by semver-ish version.
        out.sort_by(|a, b| compare_versions(&b.version, &a.version));
        Ok(out)
    }

    /// Migrate the env to a new platform ARN via UpdateEnvironment. EB
    /// performs this as a rolling update; the API returns immediately and
    /// the event log carries progress.
    pub async fn upgrade_platform(&self, env_name: &str, platform_arn: &str) -> Result<()> {
        self.client
            .update_environment()
            .environment_name(env_name)
            .platform_arn(platform_arn)
            .send()
            .await
            .wrap_err("UpdateEnvironment(platform_arn) failed")?;
        Ok(())
    }

    /// Clone an env: snapshot the source's settings into a transient
    /// configuration template, spin up a new env from it, then clean the
    /// template up. The new env starts the usual EB launch process — the
    /// caller can monitor via DescribeEvents.
    pub async fn clone_env(&self, source_env_name: &str, target_env_name: &str) -> Result<()> {
        // Snapshot the source env's application + ID.
        let desc = self
            .client
            .describe_environments()
            .environment_names(source_env_name)
            .send()
            .await
            .wrap_err("DescribeEnvironments failed")?;
        let env = desc
            .environments
            .unwrap_or_default()
            .into_iter()
            .next()
            .ok_or_else(|| eyre!("source env '{source_env_name}' not found"))?;
        let application = env
            .application_name
            .ok_or_else(|| eyre!("source env has no application_name"))?;
        let env_id = env
            .environment_id
            .ok_or_else(|| eyre!("source env has no environment_id"))?;
        // Use a transient template name so we can clean it up even if the
        // create fails partway.
        let template = format!(
            "__ebman-clone-{}-{}",
            target_env_name,
            chrono::Utc::now().timestamp()
        );
        self.client
            .create_configuration_template()
            .application_name(&application)
            .template_name(&template)
            .environment_id(&env_id)
            .send()
            .await
            .wrap_err("CreateConfigurationTemplate failed")?;
        // Best-effort cleanup even if create_environment fails — we don't
        // want to leave debris.
        let create_result = self
            .client
            .create_environment()
            .application_name(&application)
            .environment_name(target_env_name)
            .template_name(&template)
            .send()
            .await;
        let _ = self
            .client
            .delete_configuration_template()
            .application_name(&application)
            .template_name(&template)
            .send()
            .await;
        create_result.wrap_err("CreateEnvironment failed")?;
        Ok(())
    }

    /// Set the env's `aws:autoscaling:asg:{MinSize,MaxSize}` so the ASG
    /// reaches `count` instances. Passing `Some(0)` is the "stop" pattern
    /// (no instances, env keeps its config). The API returns immediately;
    /// EB performs the scale as a rolling change.
    pub async fn scale_env(&self, env_name: &str, min: i32, max: i32) -> Result<()> {
        use aws_sdk_elasticbeanstalk::types::ConfigurationOptionSetting;
        let opts = vec![
            ConfigurationOptionSetting::builder()
                .namespace("aws:autoscaling:asg")
                .option_name("MinSize")
                .value(min.to_string())
                .build(),
            ConfigurationOptionSetting::builder()
                .namespace("aws:autoscaling:asg")
                .option_name("MaxSize")
                .value(max.to_string())
                .build(),
        ];
        self.client
            .update_environment()
            .environment_name(env_name)
            .set_option_settings(Some(opts))
            .send()
            .await
            .wrap_err("UpdateEnvironment(asg) failed")?;
        Ok(())
    }

    /// Stop an in-flight environment update. Useful to bail out of a hung
    /// deploy. No-op if EB sees no operation in progress.
    pub async fn abort_environment_update(&self, env_name: &str) -> Result<()> {
        self.client
            .abort_environment_update()
            .environment_name(env_name)
            .send()
            .await
            .wrap_err("AbortEnvironmentUpdate failed")?;
        Ok(())
    }

    /// List custom EB platforms in this account. Filters server-side via
    /// `PlatformOwner=self` so we only show platforms the caller built, not
    /// the AWS-managed ones. Returns the ARN, platform branch name, and
    /// lifecycle state per entry.
    pub async fn list_custom_platforms(&self) -> Result<Vec<CustomPlatform>> {
        use aws_sdk_elasticbeanstalk::types::PlatformFilter;
        let filter = PlatformFilter::builder()
            .r#type("PlatformOwner")
            .operator("=")
            .values("self")
            .build();
        let mut next_token: Option<String> = None;
        let mut out: Vec<CustomPlatform> = Vec::new();
        loop {
            let mut req = self.client.list_platform_versions().filters(filter.clone());
            if let Some(t) = next_token.clone() {
                req = req.next_token(t);
            }
            let resp = req.send().await.wrap_err("ListPlatformVersions failed")?;
            for p in resp.platform_summary_list.unwrap_or_default() {
                out.push(CustomPlatform {
                    arn: p.platform_arn.unwrap_or_default(),
                    branch: p.platform_branch_name.unwrap_or_default(),
                    version: p.platform_version.unwrap_or_default(),
                    status: p
                        .platform_status
                        .map(|s| s.as_str().to_string())
                        .unwrap_or_default(),
                    lifecycle: p.platform_lifecycle_state.unwrap_or_default(),
                });
            }
            match resp.next_token {
                Some(t) if !t.is_empty() => next_token = Some(t),
                _ => break,
            }
        }
        Ok(out)
    }

    /// The newest version-publish date across a custom platform's
    /// version ARNs, via per-version `DescribePlatformVersion` (the
    /// only API that carries dates — `ListPlatformVersions` doesn't).
    /// `None` when no version reported a date. Feeds EBL015.
    pub async fn latest_platform_version_date(
        &self,
        version_arns: &[String],
    ) -> Result<Option<DateTime<Utc>>> {
        let mut latest: Option<DateTime<Utc>> = None;
        for arn in version_arns {
            let resp = self
                .client
                .describe_platform_version()
                .platform_arn(arn)
                .send()
                .await
                .wrap_err("DescribePlatformVersion failed")?;
            let date = resp
                .platform_description
                .and_then(|d| d.date_created)
                .and_then(|t| DateTime::<Utc>::from_timestamp(t.secs(), t.subsec_nanos()));
            if let Some(d) = date {
                if latest.is_none_or(|l| d > l) {
                    latest = Some(d);
                }
            }
        }
        Ok(latest)
    }

    /// Delete a custom platform by ARN. EB returns success immediately even
    /// though the underlying AMI / EBS cleanup runs async. Will fail if any
    /// envs are still using the platform.
    pub async fn delete_custom_platform(&self, platform_arn: &str) -> Result<()> {
        self.client
            .delete_platform_version()
            .platform_arn(platform_arn)
            .send()
            .await
            .wrap_err("DeletePlatformVersion failed")?;
        Ok(())
    }

    /// List application versions for `application_name`, sorted newest-first
    /// by `date_created`. Each entry carries the version label and the
    /// optional description text shown in the EB console. Pages through
    /// `next_token` so orgs with hundreds of historical versions see
    /// everything in `:versions` and `:rollback` can find labels that
    /// fall past the first page.
    pub async fn list_application_versions(
        &self,
        application_name: &str,
    ) -> Result<Vec<AppVersion>> {
        let mut out: Vec<AppVersion> = Vec::new();
        let mut next_token: Option<String> = None;
        loop {
            let mut req = self
                .client
                .describe_application_versions()
                .application_name(application_name);
            if let Some(t) = next_token.take() {
                req = req.next_token(t);
            }
            let resp = req
                .send()
                .await
                .wrap_err("DescribeApplicationVersions failed")?;
            for v in resp.application_versions.unwrap_or_default() {
                out.push(AppVersion {
                    label: v.version_label.unwrap_or_default(),
                    description: v.description.unwrap_or_default(),
                    created: v
                        .date_created
                        .and_then(|d| DateTime::from_timestamp(d.secs(), d.subsec_nanos())),
                });
            }
            match resp.next_token {
                Some(t) if !t.is_empty() => next_token = Some(t),
                _ => break,
            }
        }
        out.sort_by_key(|v| std::cmp::Reverse(v.created));
        Ok(out)
    }

    /// Delete an application version by label. `delete_source_bundle = true`
    /// also removes the underlying `.zip` from S3 so the storage cost goes
    /// away. EB rejects the call if the version is currently deployed to any
    /// env — surfaced as `SourceBundleDeletionException` /
    /// `OperationInProgressException` in the error chain.
    pub async fn delete_application_version(
        &self,
        application_name: &str,
        version_label: &str,
        delete_source_bundle: bool,
    ) -> Result<()> {
        self.client
            .delete_application_version()
            .application_name(application_name)
            .version_label(version_label)
            .delete_source_bundle(delete_source_bundle)
            .send()
            .await
            .wrap_err("DeleteApplicationVersion failed")?;
        Ok(())
    }

    /// Deploy a specific application-version label to an existing env via
    /// Ask EB for its managed S3 bucket — same bucket EB uses for its own
    /// uploads. We push application bundles into a known prefix here so
    /// `CreateApplicationVersion` can reference an `S3Location`. EB
    /// auto-creates the bucket on first call; subsequent calls return the
    /// same name.
    pub async fn create_storage_location(&self) -> Result<String> {
        let resp = self
            .client
            .create_storage_location()
            .send()
            .await
            .wrap_err("CreateStorageLocation failed")?;
        resp.s3_bucket
            .ok_or_else(|| eyre!("CreateStorageLocation returned no S3Bucket"))
    }

    /// Register a new application version pointing at an S3 source bundle.
    /// `auto_create_app` is `false` because we only create versions for
    /// existing applications; the env's application is the source of truth.
    pub async fn create_app_version(
        &self,
        application_name: &str,
        version_label: &str,
        description: Option<&str>,
        s3_bucket: &str,
        s3_key: &str,
    ) -> Result<()> {
        use aws_sdk_elasticbeanstalk::types::S3Location;
        let source = S3Location::builder()
            .s3_bucket(s3_bucket)
            .s3_key(s3_key)
            .build();
        let mut req = self
            .client
            .create_application_version()
            .application_name(application_name)
            .version_label(version_label)
            .source_bundle(source)
            .auto_create_application(false);
        if let Some(d) = description {
            req = req.description(d);
        }
        req.send()
            .await
            .wrap_err("CreateApplicationVersion failed")?;
        Ok(())
    }

    /// `UpdateEnvironment(version_label)`. Returns immediately — the env
    /// will mutate in the background.
    pub async fn deploy_version(&self, env_name: &str, version_label: &str) -> Result<()> {
        self.client
            .update_environment()
            .environment_name(env_name)
            .version_label(version_label)
            .send()
            .await
            .wrap_err("UpdateEnvironment(version_label) failed")?;
        Ok(())
    }

    /// Fetch the option settings stored in a saved configuration template.
    /// Returns a sorted `(namespace, option_name, value)` vector — sort makes
    /// the overlay output stable and diffable across runs. Empty values are
    /// preserved (operators sometimes care that a setting is explicitly
    /// empty vs. unset; the call only returns settings the template actually
    /// defines, so "missing" already means "use platform default").
    pub async fn describe_template_settings(
        &self,
        application_name: &str,
        template_name: &str,
    ) -> Result<Vec<(String, String, String)>> {
        let resp = self
            .client
            .describe_configuration_settings()
            .application_name(application_name)
            .template_name(template_name)
            .send()
            .await
            .wrap_err("DescribeConfigurationSettings(template) failed")?;
        let mut out: Vec<(String, String, String)> = resp
            .configuration_settings
            .unwrap_or_default()
            .into_iter()
            .flat_map(|c| c.option_settings.unwrap_or_default())
            .map(|o| {
                (
                    o.namespace.unwrap_or_default(),
                    o.option_name.unwrap_or_default(),
                    o.value.unwrap_or_default(),
                )
            })
            .collect();
        out.sort();
        Ok(out)
    }

    /// Apply a saved configuration template to an existing env via
    /// `UpdateEnvironment(template_name)`. The env will start mutating in
    /// the background; surface the launch via the events panel.
    pub async fn apply_config_template(&self, env_name: &str, template_name: &str) -> Result<()> {
        self.client
            .update_environment()
            .environment_name(env_name)
            .template_name(template_name)
            .send()
            .await
            .wrap_err("UpdateEnvironment(template_name) failed")?;
        Ok(())
    }

    pub async fn terminate_env(&self, env_name: &str) -> Result<()> {
        self.client
            .terminate_environment()
            .environment_name(env_name)
            .send()
            .await?;
        Ok(())
    }

    /// Ask EB to start collecting the tail log for an env. Per-instance log
    /// snapshots become available via `retrieve_env_info` once each instance
    /// has uploaded its sample to S3 (usually 5-15 seconds).
    pub async fn request_env_info_tail(&self, env_name: &str) -> Result<()> {
        use aws_sdk_elasticbeanstalk::types::EnvironmentInfoType;
        self.client
            .request_environment_info()
            .environment_name(env_name)
            .info_type(EnvironmentInfoType::Tail)
            .send()
            .await
            .wrap_err("RequestEnvironmentInfo failed")?;
        Ok(())
    }

    /// Read whatever tail-log samples EB has on file for the env, mapped to
    /// pre-signed S3 URLs. Empty vec means no samples have been uploaded yet —
    /// poll again. Each entry is `(ec2_instance_id, pre_signed_url)`.
    pub async fn retrieve_env_info_tail(&self, env_name: &str) -> Result<Vec<(String, String)>> {
        use aws_sdk_elasticbeanstalk::types::EnvironmentInfoType;
        let resp = self
            .client
            .retrieve_environment_info()
            .environment_name(env_name)
            .info_type(EnvironmentInfoType::Tail)
            .send()
            .await
            .wrap_err("RetrieveEnvironmentInfo failed")?;
        let mut out = Vec::new();
        for info in resp.environment_info.unwrap_or_default() {
            if let (Some(id), Some(url)) = (info.ec2_instance_id, info.message) {
                out.push((id, url));
            }
        }
        Ok(out)
    }

    /// `DescribeEnvironmentHealth` summarised down to a `(healthy, total)`
    /// pair for the INST column on the main table. Lightweight compared to
    /// `DescribeInstancesHealth` (one call returns aggregated counts; no
    /// per-instance attributes). Fanned across every env on each refresh
    /// tick — typical accounts have ≤ 50 envs which is well under the
    /// EB API's per-second budget.
    pub async fn fetch_env_instance_counts(&self, env_name: &str) -> Result<EnvInstanceCounts> {
        let resp = self
            .client
            .describe_environment_health()
            .environment_name(env_name)
            .attribute_names(
                aws_sdk_elasticbeanstalk::types::EnvironmentHealthAttribute::InstancesHealth,
            )
            .send()
            .await
            .wrap_err("DescribeEnvironmentHealth failed")?;
        Ok(summarise_instance_health(resp.instances_health.as_ref()))
    }

    pub async fn list_instances(&self, env_name: &str) -> Result<Vec<Instance>> {
        let resp = self
            .client
            .describe_instances_health()
            .environment_name(env_name)
            .attribute_names(aws_sdk_elasticbeanstalk::types::InstancesHealthAttribute::All)
            .send()
            .await?;
        let instances = resp
            .instance_health_list
            .unwrap_or_default()
            .into_iter()
            .map(|i| Instance {
                id: i.instance_id.unwrap_or_default(),
                health: i.health_status.unwrap_or_default(),
                color: i.color.unwrap_or_default(),
                causes: i.causes.unwrap_or_default(),
                instance_type: i.instance_type.unwrap_or_default(),
                availability_zone: i.availability_zone.unwrap_or_default(),
                launched_at: i
                    .launched_at
                    .and_then(|d| DateTime::from_timestamp(d.secs(), d.subsec_nanos())),
            })
            .collect();
        Ok(instances)
    }

    pub async fn list_applications(&self) -> Result<Vec<Application>> {
        let resp = self.client.describe_applications().send().await?;
        let apps = resp
            .applications
            .unwrap_or_default()
            .into_iter()
            .map(|a| Application {
                name: a.application_name.unwrap_or_default(),
                description: a.description.unwrap_or_default(),
                date_created: a
                    .date_created
                    .and_then(|d| DateTime::from_timestamp(d.secs(), d.subsec_nanos())),
                date_updated: a
                    .date_updated
                    .and_then(|d| DateTime::from_timestamp(d.secs(), d.subsec_nanos())),
                version_count: a.versions.map(|v| v.len()).unwrap_or(0),
                templates: a.configuration_templates.unwrap_or_default(),
                // Filled in by a follow-up `list_application_versions` fan-out.
                latest_version_label: None,
                latest_version_created: None,
            })
            .collect();
        Ok(apps)
    }

    pub async fn list_environments(&self) -> Result<Vec<Environment>> {
        let mut all = Vec::new();
        let mut next_token: Option<String> = None;
        loop {
            let mut req = self.client.describe_environments().include_deleted(false);
            if let Some(t) = next_token.take() {
                req = req.next_token(t);
            }
            let resp = req.send().await.wrap_err("DescribeEnvironments failed")?;
            if let Some(envs) = resp.environments {
                all.extend(envs.into_iter().map(map_env));
            }
            match resp.next_token {
                Some(t) if !t.is_empty() => next_token = Some(t),
                _ => break,
            }
        }
        Ok(all)
    }

    /// Flat list of every solution-stack name available in this region
    /// (`ListAvailableSolutionStacks`). Drives the stale-platform check:
    /// an env whose stack has a lower version than the newest stack in
    /// the same family is flagged in the table.
    pub async fn list_solution_stacks(&self) -> Result<Vec<String>> {
        let resp = self
            .client
            .list_available_solution_stacks()
            .send()
            .await
            .wrap_err("ListAvailableSolutionStacks failed")?;
        Ok(resp.solution_stacks.unwrap_or_default())
    }
}
