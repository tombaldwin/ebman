//! Hand-crafted demo data for `ebman --demo`. Builds a believable
//! six-env fleet across two applications so VHS / asciinema captures
//! and screenshots can be reproduced without touching a real AWS
//! account.
//!
//! Story: a fictional `ledgerly` company runs:
//!   * `ledgerly-prod-{api,worker}`
//!   * `ledgerly-staging-{api,worker}`
//!   * `ledgerly-canary-api`
//!   * `ledgerly-dev-api`
//!
//! Plus a `ledgerly-batch` Worker env with a DLQ backlog, sitting on
//! the same application as the API envs. Health distribution covers
//! Green / Yellow / Red / Updating so the table renders every health
//! tier; one Worker env has DLQ messages so the row-level red-tint
//! kicks in even though EB calls it Yellow.
//!
//! Pure data — no random number generator, no system time outside of
//! a single fixed reference timestamp. `install(&mut App)` overwrites
//! `App.environments` + companion fields. The caller is responsible
//! for setting `App.demo_mode = true` and skipping refresh ticks
//! (otherwise the next refresh re-fetches from the stub AWS client
//! and the fixture vanishes).

use std::collections::HashMap;

use chrono::{TimeZone, Utc};

use crate::app::App;
use crate::aws::{
    AppVersion, CwAlarm, EnvInstanceCounts, Environment, Event as EbEvent, Instance, QueueStats,
    WorkerQueues,
};

/// Anchor timestamp the fixture is computed against. Stable across
/// runs so VHS recordings show the same "Last refresh" / event
/// timestamps every time. Picked as a recent-but-stable wall-clock
/// instant so age fields read sensibly.
fn fixture_now() -> chrono::DateTime<chrono::Utc> {
    Utc.with_ymd_and_hms(2026, 5, 24, 14, 30, 0).unwrap()
}

/// Build the six-env fleet. Order matters — it's the on-screen sort
/// order before App's own sort kicks in.
fn envs() -> Vec<Environment> {
    let now = fixture_now();
    let mk =
        |name: &str, tier: &str, status: &str, health: &str, version: &str, minutes_ago: i64| {
            Environment {
                name: name.into(),
                application: "ledgerly".into(),
                status: status.into(),
                health: health.into(),
                platform: "Node.js 20 running on 64bit Amazon Linux 2023".into(),
                solution_stack: "64bit Amazon Linux 2023 v6.1.0 running Node.js 20".into(),
                tier: tier.into(),
                cname: format!("{name}.us-east-1.elasticbeanstalk.com"),
                version_label: version.into(),
                arn: Some(format!(
                    "arn:aws:elasticbeanstalk:us-east-1:123456789012:environment/ledgerly/{name}"
                )),
                updated: Some(now - chrono::Duration::minutes(minutes_ago)),
                id: Some(format!(
                    "e-{:08x}",
                    name.bytes()
                        .fold(0u32, |a, b| a.wrapping_add(b as u32) ^ a.rotate_left(5))
                )),
                region: Some("us-east-1".into()),
            }
        };
    vec![
        mk(
            "ledgerly-prod-api",
            "Web",
            "Ready",
            "Green",
            "build-823",
            47,
        ),
        mk(
            "ledgerly-prod-worker",
            "Worker",
            "Ready",
            "Green",
            "build-823",
            47,
        ),
        // Yellow + DLQ scenario.
        mk(
            "ledgerly-batch",
            "Worker",
            "Ready",
            "Yellow",
            "build-820",
            192,
        ),
        // Currently deploying — shows the Updating status tint.
        mk(
            "ledgerly-canary-api",
            "Web",
            "Updating",
            "Green",
            "build-825",
            2,
        ),
        // Red transition.
        mk(
            "ledgerly-staging-api",
            "Web",
            "Ready",
            "Red",
            "build-825",
            12,
        ),
        mk(
            "ledgerly-staging-worker",
            "Worker",
            "Ready",
            "Green",
            "build-823",
            47,
        ),
        mk(
            "ledgerly-dev-api",
            "Web",
            "Ready",
            "Grey",
            "build-825",
            1440,
        ),
    ]
}

/// Per-env filtered slice of the fleet's events. Detail/Events
/// consumes this in demo mode so it renders without a live
/// `list_events_for_env` round-trip. Once `:why` spawn-gating
/// lands, that overlay will reuse the same accessor.
pub fn events_for_env(env_name: &str) -> Vec<EbEvent> {
    events().into_iter().filter(|e| e.env == env_name).collect()
}

/// Canned ssm-session content for `--demo` mode's fake shell pane.
/// Written as a single `\r\n`-terminated string so the resulting
/// vt100::Parser screen reads like a real SSM session: the AWS CLI's
/// session-id banner, then a few short interactive commands an
/// operator would actually run during a Red-env triage (`uptime`,
/// `tail` on the EB engine log, etc.). The trailing prompt is left
/// blinking so the pane looks live; F12 detaches as usual.
pub fn canned_ssm_session(instance_id: &str) -> String {
    // Short session-id matching what `aws ssm start-session` prints.
    let session_id = format!("tom-demo-{}", &instance_id[2..10.min(instance_id.len())]);
    let mut lines = Vec::<String>::new();
    let push = |lines: &mut Vec<String>, s: &str| {
        lines.push(s.to_string());
    };
    push(
        &mut lines,
        &format!("Starting session with SessionId: {session_id}"),
    );
    push(&mut lines, "");
    push(&mut lines, "sh-4.2$ uptime");
    push(
        &mut lines,
        " 14:30:15 up 3 days,  4:22,  1 user,  load average: 0.42, 0.38, 0.31",
    );
    push(&mut lines, "sh-4.2$ tail -3 /var/log/eb-engine/health.log");
    push(&mut lines, "2026-05-24T14:27:42Z [WARN]  5xx rate 12.7%");
    push(
        &mut lines,
        "2026-05-24T14:28:55Z [ERROR] health Yellow → Red",
    );
    push(&mut lines, "sh-4.2$ ");
    // vt100 wants \r\n line endings (it's emulating an xterm).
    lines.join("\r\n")
}

/// Per-env filtered slice of the fleet's alarms. Matches by alarm-
/// name prefix against the env name since the fixture-side alarms
/// encode their owning env that way. Used by `spawn_detail_alarms`
/// and `spawn_why_red_alarms` in demo mode.
pub fn alarms_for_env(env_name: &str) -> Vec<CwAlarm> {
    alarms()
        .into_iter()
        .filter(|a| a.name.starts_with(env_name))
        .collect()
}

/// Per-application recent-deploys history. Used by
/// `spawn_detail_recent_versions` and `spawn_why_red_deploys` in
/// demo mode. Newest first, matching the live API's sort order; the
/// labels line up with the fleet's `version_label` values so an
/// operator scanning `:why` sees "what shipped last, on which env".
pub fn deploys_for_app(app_name: &str) -> Vec<AppVersion> {
    if app_name != "ledgerly" {
        return Vec::new();
    }
    let now = fixture_now();
    let mk = |label: &str, desc: &str, ago_min: i64| AppVersion {
        label: label.into(),
        description: desc.into(),
        created: Some(now - chrono::Duration::minutes(ago_min)),
    };
    vec![
        mk("build-825", "feat: new dashboard widget", 12),
        mk("build-823", "fix: backoff on 429 from upstream", 47),
        mk("build-820", "chore: bump otel-collector", 192),
        mk("build-817", "perf: cache HEAD on hot path", 1500),
        mk("build-814", "feat: add reconciler retry policy", 4320),
    ]
}

/// Per-env synthetic worker-queue stats. Worker envs return both
/// a main + DLQ stat block; non-Worker envs return defaults (no
/// URLs, no stats). Used by `spawn_detail_queues` and
/// `spawn_why_red_queues` in demo mode.
pub fn worker_queues_for_env(env_name: &str) -> WorkerQueues {
    match env_name {
        "ledgerly-batch" => WorkerQueues {
            main_url: Some(
                "https://sqs.us-east-1.amazonaws.com/123456789012/ledgerly-batch".into(),
            ),
            dlq_url: Some(
                "https://sqs.us-east-1.amazonaws.com/123456789012/ledgerly-batch-dlq".into(),
            ),
            main_stats: Some(QueueStats {
                visible: 7,
                in_flight: 3,
                delayed: 0,
            }),
            dlq_stats: Some(QueueStats {
                visible: 12,
                in_flight: 0,
                delayed: 0,
            }),
        },
        "ledgerly-prod-worker" => WorkerQueues {
            main_url: Some(
                "https://sqs.us-east-1.amazonaws.com/123456789012/ledgerly-prod-worker".into(),
            ),
            dlq_url: Some(
                "https://sqs.us-east-1.amazonaws.com/123456789012/ledgerly-prod-worker-dlq".into(),
            ),
            main_stats: Some(QueueStats {
                visible: 4,
                in_flight: 2,
                delayed: 0,
            }),
            dlq_stats: Some(QueueStats::default()),
        },
        "ledgerly-staging-worker" => WorkerQueues {
            main_url: Some(
                "https://sqs.us-east-1.amazonaws.com/123456789012/ledgerly-staging-worker".into(),
            ),
            dlq_url: Some(
                "https://sqs.us-east-1.amazonaws.com/123456789012/ledgerly-staging-worker-dlq"
                    .into(),
            ),
            main_stats: Some(QueueStats::default()),
            dlq_stats: Some(QueueStats::default()),
        },
        _ => WorkerQueues::default(),
    }
}

/// Per-env synthetic instance list, used by `spawn_detail_instances`
/// when in demo mode so Detail/Instances renders without firing an
/// AWS call. EC2-ID format (`i-` + 17 hex) matches the post-2017
/// long-form IDs operators see in production. Envs not listed here
/// return an empty Vec (Grey envs / envs with no instances yet).
pub fn instances_for(env_name: &str) -> Vec<Instance> {
    let now = fixture_now();
    let mk = |id: &str, health: &str, color: &str, az: &str, ago_min: i64| Instance {
        id: id.into(),
        health: health.into(),
        color: color.into(),
        causes: Vec::new(),
        instance_type: "t3.medium".into(),
        availability_zone: az.into(),
        launched_at: Some(now - chrono::Duration::minutes(ago_min)),
    };
    match env_name {
        "ledgerly-staging-api" => vec![
            mk("i-0abc123def456789a", "Severe", "Red", "us-east-1a", 84),
            mk("i-0bcd234ef567890ab", "Ok", "Green", "us-east-1b", 84),
        ],
        "ledgerly-batch" => vec![
            mk(
                "i-0cde345f6789012bc",
                "Warning",
                "Yellow",
                "us-east-1a",
                240,
            ),
            mk("i-0def4567890123cde", "Ok", "Green", "us-east-1b", 240),
        ],
        "ledgerly-prod-api" => vec![
            mk("i-0ef56789012345def", "Ok", "Green", "us-east-1a", 1500),
            mk("i-0f6789012345678ef", "Ok", "Green", "us-east-1b", 1500),
            mk("i-01234567890abcdef", "Ok", "Green", "us-east-1c", 1500),
        ],
        "ledgerly-prod-worker" => vec![
            mk("i-0234567890abcdef0", "Ok", "Green", "us-east-1a", 1500),
            mk("i-03456789012abcdef", "Ok", "Green", "us-east-1b", 1500),
        ],
        "ledgerly-staging-worker" => vec![
            mk("i-04567890abcdef012", "Ok", "Green", "us-east-1a", 84),
            mk("i-05678901abcdef234", "Ok", "Green", "us-east-1b", 84),
        ],
        "ledgerly-canary-api" => vec![
            // Canary mid-deploy — one of two is being replaced.
            mk("i-067890abcdef34567", "Pending", "Grey", "us-east-1a", 2),
        ],
        _ => Vec::new(),
    }
}

/// Recent events the events panel + `:why` overlay surface. Picked so
/// the staging-api Red transition has a story (deploy → health
/// degraded → no recovery) the operator can read in 10 seconds.
fn events() -> Vec<EbEvent> {
    let now = fixture_now();
    let mk = |env: &str, msg: &str, sev: &str, version: Option<&str>, ago: i64| EbEvent {
        at: Some(now - chrono::Duration::minutes(ago)),
        env: env.into(),
        application: "ledgerly".into(),
        message: msg.into(),
        severity: sev.into(),
        version_label: version.map(String::from),
    };
    vec![
        // Newest first — App display order.
        mk(
            "ledgerly-staging-api",
            "Environment health has transitioned from Yellow to Red. Application running 95% of the time. 50% of the requests are erroring with HTTP 4xx.",
            "ERROR",
            None,
            8,
        ),
        mk(
            "ledgerly-staging-api",
            "Environment health has transitioned from Green to Yellow. Application running 99% of the time.",
            "WARN",
            None,
            10,
        ),
        mk(
            "ledgerly-canary-api",
            "Environment update is starting. Deploying new version to instance(s).",
            "INFO",
            Some("build-825"),
            2,
        ),
        mk(
            "ledgerly-staging-api",
            "New application version deployed.",
            "INFO",
            Some("build-825"),
            12,
        ),
        mk(
            "ledgerly-staging-api",
            "Environment update completed successfully.",
            "INFO",
            Some("build-823"),
            240,
        ),
        mk(
            "ledgerly-prod-api",
            "Environment update completed successfully.",
            "INFO",
            Some("build-823"),
            48,
        ),
        mk(
            "ledgerly-batch",
            "Environment update completed successfully.",
            "INFO",
            Some("build-820"),
            193,
        ),
    ]
}

/// Fleet-wide alarms — consumed via `alarms_for_env` by the spawn-
/// site gates in `spawn_detail_alarms` / `spawn_why_red_alarms` so
/// the live fetches don't hit the stub AwsClient in demo mode.
fn alarms() -> Vec<CwAlarm> {
    vec![
        CwAlarm {
            name: "ledgerly-staging-api-4xx-elevated".into(),
            state: "ALARM".into(),
            state_reason:
                "Threshold Crossed: 1 datapoint [120.0 (24/05/26 14:25:00)] was greater than the threshold (50.0)."
                    .into(),
            metric_name: "ApplicationRequests4xx".into(),
            namespace: "AWS/ElasticBeanstalk".into(),
        },
        CwAlarm {
            name: "ledgerly-batch-dlq-depth".into(),
            state: "ALARM".into(),
            state_reason: "Threshold Crossed: 1 out of the last 1 datapoints [12.0] was greater than the threshold (5.0).".into(),
            metric_name: "ApproximateNumberOfMessagesVisible".into(),
            namespace: "AWS/SQS".into(),
        },
        CwAlarm {
            name: "ledgerly-prod-api-p99-latency".into(),
            state: "OK".into(),
            state_reason: "Threshold Crossed: 1 out of the last 1 datapoints [0.42] was not greater than the threshold (1.0).".into(),
            metric_name: "ApplicationLatencyP99".into(),
            namespace: "AWS/ElasticBeanstalk".into(),
        },
    ]
}

fn instance_counts() -> HashMap<String, EnvInstanceCounts> {
    let mut out = HashMap::new();
    let put =
        |out: &mut HashMap<String, EnvInstanceCounts>, name: &str, healthy: i32, total: i32| {
            out.insert(name.to_string(), EnvInstanceCounts { healthy, total });
        };
    put(&mut out, "ledgerly-prod-api", 3, 3);
    put(&mut out, "ledgerly-prod-worker", 2, 2);
    put(&mut out, "ledgerly-batch", 1, 2);
    put(&mut out, "ledgerly-canary-api", 0, 1);
    put(&mut out, "ledgerly-staging-api", 1, 2);
    put(&mut out, "ledgerly-staging-worker", 2, 2);
    put(&mut out, "ledgerly-dev-api", 0, 0);
    out
}

/// Worker DLQ depths — populates the row-level red-tint for
/// `ledgerly-batch` and gives `:why` something to render under the
/// Worker section.
fn worker_dlq_depths() -> HashMap<String, i64> {
    let mut out = HashMap::new();
    out.insert("ledgerly-batch".to_string(), 12);
    out
}

/// Per-env cost figures so `:cost on` (and the corresponding `:why`
/// row) has numbers. Buckets: green < $50, muted $50–$500, red >= $500.
fn costs() -> HashMap<String, f64> {
    let mut out = HashMap::new();
    out.insert("ledgerly-prod-api".into(), 612.0);
    out.insert("ledgerly-prod-worker".into(), 184.0);
    out.insert("ledgerly-batch".into(), 96.0);
    out.insert("ledgerly-canary-api".into(), 42.0);
    out.insert("ledgerly-staging-api".into(), 38.0);
    out.insert("ledgerly-staging-worker".into(), 28.0);
    out.insert("ledgerly-dev-api".into(), 11.0);
    out
}

/// Install the demo fixture onto `app`. Caller is responsible for
/// setting `app.demo_mode = true` (see `App::new_demo`) and skipping
/// the refresh ticker — otherwise the stub AwsClient's empty
/// responses will overwrite this data.
///
/// **Scope**: only the fleet-wide caches populated by every refresh
/// tick are filled in — `environments`, the events panel, DLQ
/// depths, instance counts, cost data. The spawn-and-overlay caches
/// (per-env alarms / instances / queues that `:why` / Detail tabs
/// fetch on demand) are *not* pre-populated — those endpoints would
/// fire against the stub `AwsClient` and either error or return
/// empty. v1 demo coverage: main table + Detail/Health + the
/// breadcrumb. Drill-into-other-tabs is best-effort and may show
/// stub errors; closing that gap needs spawn-site gating in demo
/// mode, which is a separate piece of work.
pub fn install(app: &mut App) {
    app.environments = envs();
    app.event_panel.events = events();
    app.worker_dlq_depths = worker_dlq_depths();
    app.env_instance_counts = instance_counts();
    app.cost_enabled = true;
    app.costs = costs();
    app.costs_fetched_at = Some(fixture_now());
    app.context.account_id = Some("123456789012".into());
    app.context.profile = Some("ledgerly-demo".into());
    app.context.caller_arn = Some("arn:aws:iam::123456789012:user/demo-operator".into());
    app.last_refresh = Some(fixture_now());
    app.rebuild_view();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_contains_one_of_every_health_tier() {
        let envs = envs();
        let healths: std::collections::BTreeSet<&str> =
            envs.iter().map(|e| e.health.as_str()).collect();
        // The four headline tiers a VHS demo wants to show off.
        assert!(healths.contains("Green"), "missing Green");
        assert!(healths.contains("Yellow"), "missing Yellow");
        assert!(healths.contains("Red"), "missing Red");
        // Updating is a status, not a health — make sure at least
        // one env is in Updating so that tint also renders.
        assert!(
            envs.iter().any(|e| e.status == "Updating"),
            "no Updating env"
        );
    }

    #[test]
    fn fixture_dlq_env_has_messages() {
        // The `:why` Worker section's DLQ peek is one of the
        // demo's headline moments. The fixture must keep it non-empty.
        let dlqs = worker_dlq_depths();
        assert!(
            dlqs.get("ledgerly-batch").copied().unwrap_or(0) > 0,
            "ledgerly-batch DLQ should be non-empty"
        );
    }

    #[test]
    fn fixture_red_env_has_an_alarm_and_a_health_event() {
        // Demo flow: operator presses `!` on ledgerly-staging-api,
        // sees both an ERROR-level health event and an ALARM. Both
        // need to be present for the overlay not to look thin.
        assert!(events()
            .iter()
            .any(|e| e.env == "ledgerly-staging-api" && e.severity == "ERROR"));
        assert!(alarms()
            .iter()
            .any(|a| a.name.starts_with("ledgerly-staging-api") && a.state == "ALARM"));
    }

    #[test]
    fn fixture_cost_buckets_span_green_muted_red() {
        // `:cost on` colours by bucket — the demo's COST column should
        // show all three so a single screenshot covers each tier.
        let c = costs();
        assert!(c.values().any(|v| *v < 50.0), "missing green-bucket cost");
        assert!(
            c.values().any(|v| *v >= 50.0 && *v < 500.0),
            "missing muted-bucket cost"
        );
        assert!(c.values().any(|v| *v >= 500.0), "missing red-bucket cost");
    }
}
