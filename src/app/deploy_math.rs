//! Deploy arithmetic: rolling-batch sizing, unavailability estimates,
//! and classifying what an in-flight environment update actually is.
//!
//! Pure functions over AWS-shaped inputs so the numbers shown in the
//! confirm modal can be tested without a live environment.

use super::*;

/// Compact age formatter — "3s", "12s", "2m", "1h", "4d". Used for the
/// pending-actions overlay so ages stay short and uniform.
/// Pure: the version label deployed *before* `current`, found by
/// scanning `events` (newest-first, as `DescribeEvents` returns)
/// for the first `version_label` that differs from `current`. EB
/// tags each event with the version current at the time, so walking
/// back, the first label ≠ `current` is the one the env ran before
/// this deploy. `None` when no prior version appears in the window.
pub(crate) fn previous_version_label(events: &[EbEvent], current: &str) -> Option<String> {
    events
        .iter()
        .filter_map(|e| e.version_label.as_deref())
        .filter(|v| !v.is_empty())
        .find(|v| *v != current)
        .map(|v| v.to_string())
}

/// Pure: whether an event message looks like a deploy or a
/// configuration change — the rows the `:changes` timeline keeps,
/// filtering out routine health / scaling / launch noise.
pub(crate) fn is_config_event(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("version label")
        || m.contains("deploying")
        || m.contains("configuration")
        || m.contains("config setting")
}

/// Pure: how many instances will be simultaneously unavailable
/// during a deploy with the given EB deployment policy + batch
/// settings + ASG max-size. Returns the worst-case planning
/// number — what the operator sees on the EB dashboard during
/// the rollout. `asg_max` clamps at 1 to avoid divide-by-zero
/// nonsense on misconfigured envs.
///
/// Numbers per policy (EB docs):
/// - `AllAtOnce` — every instance restarts simultaneously
/// - `Rolling` — one batch at a time, no extra capacity
/// - `RollingWithAdditionalBatch` — extra batch launched first
/// - `Immutable` — new instances alongside (no current-fleet impact)
/// - `TrafficSplitting` — new ASG receives % traffic (no impact)
pub(crate) fn compute_unavailability_count(
    policy: &str,
    batch_size: i32,
    batch_size_type: &str,
    asg_max: i32,
) -> i32 {
    let asg_max = asg_max.max(1);
    match policy {
        p if p.eq_ignore_ascii_case("AllAtOnce") => asg_max,
        p if p.eq_ignore_ascii_case("Rolling") => {
            compute_batch_count(batch_size, batch_size_type, asg_max)
        }
        // The "additional batch" launches before rotating, so no
        // capacity dip from the perspective of in-service requests.
        p if p.eq_ignore_ascii_case("RollingWithAdditionalBatch") => 0,
        p if p.eq_ignore_ascii_case("Immutable") => 0,
        p if p.eq_ignore_ascii_case("TrafficSplitting") => 0,
        // Unknown policy — be honest about the lack of signal.
        // Return the worst case rather than 0 so an operator with
        // a custom policy isn't lulled by a false-zero.
        _ => asg_max,
    }
}

/// Pure helper: resolve `BatchSize` + `BatchSizeType` into a
/// concrete instance count, clamped to [1, asg_max]. `Percentage`
/// rounds UP (a 33% batch on a 4-instance ASG is 2 instances; EB
/// rounds up internally too, per the docs).
pub(crate) fn compute_batch_count(batch_size: i32, batch_size_type: &str, asg_max: i32) -> i32 {
    if batch_size_type.eq_ignore_ascii_case("Percentage") {
        let pct = batch_size.clamp(1, 100);
        // Manual ceiling-divide: `i32::div_ceil` is still unstable
        // on this MSRV (1.91). Both operands are positive after the
        // clamps above, so `(a + b - 1) / b` is safe.
        let count = (asg_max * pct + 99) / 100;
        count.max(1).min(asg_max)
    } else {
        // Fixed — `BatchSizeType=Fixed` or any non-Percentage value.
        batch_size.max(1).min(asg_max)
    }
}

/// Pure: render the modal's unavailability line. Returns the
/// human-readable text plus a severity flag for colouring (true
/// = caution, false = green/no impact).
pub(crate) fn format_unavailability_line(
    policy: &str,
    unavailable: i32,
    asg_max: i32,
) -> (String, bool) {
    let asg_max = asg_max.max(1);
    let caution = unavailable > 0;
    let plural = if unavailable == 1 {
        "instance"
    } else {
        "instances"
    };
    let body = if unavailable == 0 {
        format!("deploy plan: {policy} → no in-service unavailability")
    } else {
        format!("deploy plan: {policy} → max {unavailable}/{asg_max} {plural} unavailable")
    };
    (body, caution)
}

/// Pure: extract the four option-settings the unavailability
/// estimate needs from the flat `(namespace, name, value)` shape
/// `fetch_env_option_settings` returns. Defaults match EB's own
/// defaults so the math degrades gracefully on partial reads.
pub(crate) fn extract_unavailability_inputs(
    opts: &[(String, String, String)],
) -> (String, i32, String, i32) {
    let get = |ns: &str, name: &str| -> Option<&String> {
        opts.iter()
            .find(|(n, k, _)| n == ns && k == name)
            .map(|(_, _, v)| v)
    };
    let policy = get("aws:elasticbeanstalk:command", "DeploymentPolicy")
        .cloned()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "AllAtOnce".to_string());
    let batch_size = get("aws:elasticbeanstalk:command", "BatchSize")
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(1);
    let batch_size_type = get("aws:elasticbeanstalk:command", "BatchSizeType")
        .cloned()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Fixed".to_string());
    let asg_max = get("aws:autoscaling:asg", "MaxSize")
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(1);
    (policy, batch_size, batch_size_type, asg_max)
}

/// Pure: "deploy has fully succeeded for this env" predicate.
/// Both conditions matter — `UpdateEnvironment` momentarily
/// leaves `health=Green` while `status` flips to `Updating`,
/// so a watcher that only checks health would false-positive
/// during that window and disarm a rollback (or report
/// success) before the deploy has actually settled. Single
/// source of truth shared by the rollback-watchdog pass, the
/// wait-for-green pass, and the non-interactive CLI's
/// `decide_poll`.
///
/// Truthy when:
/// - `status` is `Ready` (case-insensitive, EB's settled state)
/// - `health` is `Green` or `Ok` (case-insensitive, EB's two
///   "all-clear" terms across enhanced vs legacy reporting)
pub(crate) fn deploy_settled_green(status: &str, health: &str) -> bool {
    status.eq_ignore_ascii_case("Ready")
        && (health.eq_ignore_ascii_case("Green") || health.eq_ignore_ascii_case("Ok"))
}

/// Pure: copy `latest_version_label` / `latest_version_created` from a
/// previous `applications` snapshot onto the new one (matched by name) so
/// the apps-view LATEST column doesn't flicker to "—" while the follow-up
/// `DescribeApplicationVersions` fan-out is in flight after each refresh.
///
/// Only fills slots that are currently `None`. Today `list_applications`
/// never populates those fields itself so the conditional is a no-op
/// safety net — but it means a future caller that *does* pre-populate
/// won't get silently overwritten with stale data.
pub(crate) fn merge_app_latest_versions(prev: &[Application], next: &mut [Application]) {
    let by_name: std::collections::HashMap<
        &str,
        (&Option<String>, &Option<chrono::DateTime<chrono::Utc>>),
    > = prev
        .iter()
        .map(|a| {
            (
                a.name.as_str(),
                (&a.latest_version_label, &a.latest_version_created),
            )
        })
        .collect();
    for app in next.iter_mut() {
        let Some((label, created)) = by_name.get(app.name.as_str()) else {
            continue;
        };
        if app.latest_version_label.is_none() {
            app.latest_version_label = (*label).clone();
        }
        if app.latest_version_created.is_none() {
            app.latest_version_created = **created;
        }
    }
}

/// Inferred kind of an `Updating` env's in-flight operation. EB's
/// `status` field is generic ("Updating") regardless of cause, but the
/// recent events expose what's actually happening. The Health tab uses
/// this to render `Updating: deploying build-142` (or similar) instead
/// of just the generic pill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UpdateKind {
    /// `UpdateEnvironment(version_label)` — a version deploy in flight.
    /// `version_label` is extracted from the event message when present.
    Deploy { version_label: Option<String> },
    /// `UpdateEnvironment(option_settings)` — configuration change in flight.
    Config,
    /// Auto-scaling activity — instances being added or removed.
    Scale,
    /// `UpdateEnvironment(platform_arn)` / managed platform update.
    Platform,
    /// Status is Updating but no recent event matches a known pattern.
    /// Falls back to a generic "updating" label.
    Generic,
}

/// Pure: classify an `Updating` env's in-flight op by looking at the
/// most recent event whose message matches a known pattern. Events are
/// expected newest-first (as the EB API returns them); returns the kind
/// from the first matching event. Returns `Generic` when nothing
/// matches.
pub(crate) fn classify_update_kind(events: &[crate::aws::Event]) -> UpdateKind {
    for e in events {
        let lower = e.message.to_lowercase();
        // Deploy comes first — "version label" is the unambiguous signal.
        // The "version label" check catches both the dispatch event
        // (`Updating environment to use version label 'X'`) and the
        // completion event (`Environment update completed successfully
        // … version 'X'`).
        if lower.contains("version label") {
            return UpdateKind::Deploy {
                version_label: extract_quoted_after(&e.message, "version label"),
            };
        }
        if lower.contains("deploying") && lower.contains("version") {
            return UpdateKind::Deploy {
                version_label: extract_quoted_after(&e.message, "version"),
            };
        }
        // Platform updates have a distinctive "platform" + "updat" stem.
        if lower.contains("platform") && (lower.contains("updat") || lower.contains("upgrad")) {
            return UpdateKind::Platform;
        }
        // Config changes — option settings.
        if lower.contains("configuration") && lower.contains("updat") {
            return UpdateKind::Config;
        }
        // Auto-scaling — instances coming or going.
        if (lower.contains("adding") || lower.contains("removing")) && lower.contains("instance") {
            return UpdateKind::Scale;
        }
    }
    UpdateKind::Generic
}
