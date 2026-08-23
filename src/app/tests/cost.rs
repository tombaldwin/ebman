//! Cost Explorer: fetch, cache, truncation, the fleet rollup.
//!
//! Split out of the 9,515-line `app/tests.rs`. Bodies moved
//! unchanged apart from one rewrite: `super::` meant `crate::app` in
//! the flat file and would mean `crate::app::tests` here, so every
//! explicit `super::` path was re-anchored (rustfmt reflowed some
//! lines as a result, since the new path is longer).

use super::super::*;
#[allow(unused_imports)]
use super::support::*;

#[test]
fn render_fleet_cost_breaks_down_by_app_tier_health() {
    let envs = vec![
        mk_env("api-prod", "api", "Web", "Green"),
        mk_env("api-staging", "api", "Web", "Yellow"),
        mk_env("worker-prod", "api", "Worker", "Green"),
        mk_env("billing-prod", "billing", "Web", "Red"),
    ];
    let mut costs = std::collections::HashMap::new();
    costs.insert("api-prod".to_string(), 100.0);
    costs.insert("api-staging".to_string(), 25.5);
    costs.insert("worker-prod".to_string(), 40.0);
    costs.insert("billing-prod".to_string(), 60.25);
    let now = chrono::Utc::now();
    let body = crate::app::render_fleet_cost(&envs, &costs, Some(now), now);
    assert!(body.contains("Total: $225.75/mo"), "total: {body}");
    assert!(body.contains("4 env(s) covered"), "covered count: {body}");
    // Per-app cost: api = 100 + 25.5 + 40 = 165.5
    assert!(body.contains("$    165.50/mo  api"), "by app: {body}");
    assert!(body.contains("$     60.25/mo  billing"), "by app: {body}");
    // Per-tier: Web = 100 + 25.5 + 60.25 = 185.75
    assert!(body.contains("$    185.75/mo  Web"), "by tier: {body}");
    assert!(body.contains("$     40.00/mo  Worker"), "by tier: {body}");
    // Per-health: Green = 100 + 40 = 140
    assert!(body.contains("$    140.00/mo  Green"), "by health: {body}");
}

#[test]
fn render_fleet_cost_flags_uncovered_envs() {
    let envs = vec![
        mk_env("api-prod", "api", "Web", "Green"),
        mk_env("api-uncached", "api", "Web", "Green"),
    ];
    let mut costs = std::collections::HashMap::new();
    costs.insert("api-prod".to_string(), 50.0);
    let now = chrono::Utc::now();
    let body = crate::app::render_fleet_cost(&envs, &costs, Some(now), now);
    assert!(
        body.contains("1 env(s) covered, 1 without cost data"),
        "missing count: {body}"
    );
}

#[test]
fn render_fleet_cost_flags_stale_cache() {
    let envs = vec![mk_env("a", "x", "Web", "Green")];
    let mut costs = std::collections::HashMap::new();
    costs.insert("a".to_string(), 10.0);
    let now = chrono::Utc::now();
    let stale = now - chrono::Duration::hours(36);
    let body = crate::app::render_fleet_cost(&envs, &costs, Some(stale), now);
    assert!(body.contains("stale"), "stale marker: {body}");
}

#[test]
fn render_fleet_cost_no_freshness_line_when_unset() {
    let envs = vec![mk_env("a", "x", "Web", "Green")];
    let mut costs = std::collections::HashMap::new();
    costs.insert("a".to_string(), 10.0);
    let now = chrono::Utc::now();
    let body = crate::app::render_fleet_cost(&envs, &costs, None, now);
    assert!(!body.contains("Cached:"), "no cached line: {body}");
    assert!(body.contains("Total: $10.00/mo"));
}

#[test]
fn instance_hourly_usd_known_types() {
    assert!(instance_hourly_usd("t3.micro").unwrap() > 0.0);
    assert!(instance_hourly_usd("m5.large").unwrap() > 0.0);
    assert_eq!(instance_hourly_usd("not-a-real-type"), None);
}

#[test]
fn estimate_cost_handles_mixed() {
    let mk = |t: &str, az: &str| Instance {
        id: "i-1".into(),
        health: "Ok".into(),
        color: "Green".into(),
        causes: vec![],
        instance_type: t.into(),
        availability_zone: az.into(),
        launched_at: None,
    };
    let instances = vec![
        mk("t3.micro", "us-east-1a"),
        mk("t3.micro", "us-east-1b"),
        mk("unknown-type-xyz", "us-east-1c"),
    ];
    let (hourly, missing) = estimate_cost(&instances);
    assert_eq!(missing, 1);
    // Two t3.micro at $0.0104/hr each.
    assert!((hourly - 0.0208).abs() < 1e-9);
}

#[tokio::test]
async fn render_cost_column_red_tints_expensive_envs() {
    // `:cost on`: a >= $500/mo env paints its COST cell health_red.
    // poly-prod-api ($612, Green health) is the only red in its row
    // (its health dot is green), while the cheaper green-bucket
    // poly-staging-worker ($28, Green) has no red at all.
    let mut app = test_app();
    crate::demo_fixture::install(&mut app);
    app.table_state.select(None);
    let theme = app.theme.clone();
    let buf = render_buf(&mut app, 160, 30);
    let pricey = find_row(&buf, "poly-prod-api").expect("prod-api row");
    let cheap = find_row(&buf, "poly-staging-worker").expect("staging-worker row");
    assert!(
        row_has_fg(&buf, pricey, theme.health_red),
        "the $612 env should paint a health_red COST cell"
    );
    assert!(
        !row_has_fg(&buf, cheap, theme.health_red),
        "a cheap green-health env row should have no red cell"
    );
}

// --- cost refresh truncation ------------------------------------------

#[tokio::test]
async fn a_truncated_cost_refresh_keeps_the_previous_map() {
    // The truncation flag protected the 24-hour disk cache but the
    // handler still cleared and replaced the live map first — so 25 of
    // 40 envs would flip from real numbers to `—`, which renders
    // identically to "untagged", while `:fleet-cost` under-reported.
    let mut app = test_app();
    app.costs.insert("api-prod".into(), 100.0);
    app.costs.insert("web-prod".into(), 200.0);
    app.costs.insert("worker-prod".into(), 50.0);
    let before = app.costs.clone();

    app.handle_msg(AppMsg::CostsFetched {
        gen: app.generation,
        account: Some("123456789012".into()),
        region: "us-east-1".into(),
        result: Ok(crate::aws::EnvCosts {
            rows: vec![crate::aws::EnvCost {
                env_name: "api-prod".into(),
                cost_usd: 111.0,
            }],
            truncated: true,
        }),
    });

    assert_eq!(
        app.costs, before,
        "a partial walk must not replace a good map"
    );
    let msg = app.error_message.as_deref().expect("must say so");
    assert!(msg.contains("INCOMPLETE"), "{msg}");
    assert_no_run_on_spaces(msg);
}

#[tokio::test]
async fn a_truncated_cost_refresh_with_nothing_cached_shows_what_it_has() {
    // With no previous map there is nothing to preserve, so partial
    // beats blank — but it must be labelled and must not stamp a fetch
    // time that would suppress the retry.
    let mut app = test_app();
    assert!(app.costs.is_empty());

    app.handle_msg(AppMsg::CostsFetched {
        gen: app.generation,
        account: None,
        region: "us-east-1".into(),
        result: Ok(crate::aws::EnvCosts {
            rows: vec![crate::aws::EnvCost {
                env_name: "api-prod".into(),
                cost_usd: 111.0,
            }],
            truncated: true,
        }),
    });

    assert_eq!(app.costs.len(), 1, "partial data still renders");
    assert!(
        app.costs_fetched_at.is_none(),
        "an incomplete walk must not stamp a fetch time"
    );
    let msg = app.error_message.as_deref().expect("must say so");
    assert!(msg.contains("INCOMPLETE"), "{msg}");
    assert_no_run_on_spaces(msg);
}

#[tokio::test]
async fn a_complete_cost_refresh_replaces_the_map() {
    let mut app = test_app();
    app.costs.insert("stale".into(), 999.0);

    app.handle_msg(AppMsg::CostsFetched {
        gen: app.generation,
        account: None,
        region: "us-east-1".into(),
        result: Ok(crate::aws::EnvCosts {
            rows: vec![crate::aws::EnvCost {
                env_name: "api-prod".into(),
                cost_usd: 111.0,
            }],
            truncated: false,
        }),
    });

    assert_eq!(app.costs.len(), 1);
    assert!(app.costs.contains_key("api-prod"));
    assert!(!app.costs.contains_key("stale"), "a complete walk replaces");
    assert!(app.costs_fetched_at.is_some());
}

#[tokio::test]
async fn a_partial_cost_map_does_not_become_permanent() {
    // The "do we already have costs?" test made partial data sticky:
    // the first truncated walk populated the map, and every later one
    // then saw a non-empty map and kept it — so the first failure's
    // data survived the session while each retry paid for twenty
    // metered Cost Explorer pages and threw them away.
    let mut app = test_app();
    assert!(app.costs.is_empty());

    let truncated = |env: &str, usd: f64| crate::aws::EnvCosts {
        rows: vec![crate::aws::EnvCost {
            env_name: env.into(),
            cost_usd: usd,
        }],
        truncated: true,
    };

    // First truncated walk: nothing to preserve, take it, mark partial.
    app.handle_msg(AppMsg::CostsFetched {
        gen: app.generation,
        account: None,
        region: "us-east-1".into(),
        result: Ok(truncated("api-prod", 100.0)),
    });
    assert!(!app.costs_complete, "a truncated walk is not complete");
    assert_eq!(app.costs.get("api-prod"), Some(&100.0));

    // Second truncated walk: what we hold is itself partial, so the
    // fresher partial data must replace it rather than be discarded.
    app.handle_msg(AppMsg::CostsFetched {
        gen: app.generation,
        account: None,
        region: "us-east-1".into(),
        result: Ok(truncated("web-prod", 55.0)),
    });
    assert!(!app.costs_complete);
    assert_eq!(app.costs.get("web-prod"), Some(&55.0));
    assert!(
        !app.costs.contains_key("api-prod"),
        "the stale partial map must not accumulate"
    );

    // A complete walk clears the partial flag and wins outright.
    app.handle_msg(AppMsg::CostsFetched {
        gen: app.generation,
        account: None,
        region: "us-east-1".into(),
        result: Ok(crate::aws::EnvCosts {
            rows: vec![crate::aws::EnvCost {
                env_name: "full".into(),
                cost_usd: 1.0,
            }],
            truncated: false,
        }),
    });
    assert!(app.costs_complete);
    assert_eq!(app.costs.len(), 1);

    // And now a truncated walk must NOT replace the complete map.
    app.handle_msg(AppMsg::CostsFetched {
        gen: app.generation,
        account: None,
        region: "us-east-1".into(),
        result: Ok(truncated("partial", 9.0)),
    });
    assert!(
        app.costs_complete,
        "a complete map survives a truncated walk"
    );
    assert_eq!(app.costs.get("full"), Some(&1.0));
}

#[tokio::test]
async fn cost_status_and_fleet_cost_say_when_the_data_is_partial() {
    // A truncated walk deliberately leaves `costs_fetched_at` unset so
    // a retry isn't suppressed — which meant `:cost status` reported
    // "no data yet" while dollar figures were on screen, and
    // `:fleet-cost` rendered an under-reporting total with no marker.
    let mut app = test_app();
    app.cost_enabled = true;
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
    app.rebuild_view();
    app.handle_msg(AppMsg::CostsFetched {
        gen: app.generation,
        account: None,
        region: "us-east-1".into(),
        result: Ok(crate::aws::EnvCosts {
            rows: vec![crate::aws::EnvCost {
                env_name: "api-prod".into(),
                cost_usd: 100.0,
            }],
            truncated: true,
        }),
    });
    assert!(!app.costs_complete);

    app.execute_command("cost status");
    let status = app.status_message.as_deref().unwrap_or_default();
    assert!(
        status.contains("INCOMPLETE"),
        ":cost status must not present partial data as settled: {status:?}"
    );

    app.error_message = None;
    app.execute_command("fleet-cost");
    let err = app.error_message.as_deref().unwrap_or_default();
    assert!(
        err.contains("under-reports"),
        ":fleet-cost must mark a partial total: {err:?}"
    );
}

#[tokio::test]
async fn switching_context_resets_the_cost_completeness_verdict() {
    // The flag belonged to the previous account; leaving it set meant
    // a fresh context inherited a stale "partial" verdict.
    let mut app = test_app();
    app.cost_enabled = true; // `:cost off` early-returns when already off
    app.costs.insert("old".into(), 1.0);
    app.costs_complete = false;
    app.execute_command("cost off");
    assert!(app.costs.is_empty());
    assert!(app.costs_complete, "a torn-down map carries no verdict");
}

#[tokio::test]
async fn cost_on_retries_an_incomplete_walk_instead_of_saying_already_on() {
    // `spawn_cost_fetch` has exactly one caller — the `:cost on`
    // transition — so there is no periodic refetch. Answering "already
    // on" made a truncated walk terminal for the session: the partial
    // map stayed, the INCOMPLETE toast was cleared by the next refresh
    // tick, and every env past the cap showed `—`, indistinguishable
    // from untagged.
    let mut app = test_app();
    app.cost_enabled = true;
    app.costs.insert("api-prod".into(), 1.0);
    app.costs_complete = false;

    app.execute_command("cost on");
    let msg = app.status_message.as_deref().unwrap_or_default();
    assert!(
        msg.contains("retrying"),
        ":cost on must retry an incomplete walk, not report 'already on': {msg:?}"
    );

    // With complete data it still short-circuits — no metered refetch
    // for an operator who typed it twice.
    app.costs_complete = true;
    app.execute_command("cost on");
    let msg = app.status_message.as_deref().unwrap_or_default();
    assert!(msg.contains("already on"), "{msg:?}");
}

#[tokio::test]
async fn cost_status_does_not_call_a_partial_result_cached() {
    // Reachable when a truncated walk lands over a non-empty map: the
    // previous timestamp stays, so the arm that formats it said
    // "cached" for data the handler had explicitly refused to cache.
    let mut app = test_app();
    app.cost_enabled = true;
    app.costs.insert("api-prod".into(), 1.0);
    app.costs_fetched_at = Some(chrono::Utc::now() - chrono::Duration::hours(3));
    app.costs_complete = false;

    app.execute_command("cost status");
    let msg = app.status_message.as_deref().unwrap_or_default();
    assert!(msg.contains("INCOMPLETE"), "{msg:?}");
    assert!(
        !msg.contains("env(s) cached"),
        "a partial result was never cached: {msg:?}"
    );
}
