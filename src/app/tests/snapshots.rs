//! Whole rendered screens, as snapshots (`insta`), at three widths and
//! in both icon styles.
//!
//! The layout bugs this repo keeps finding (a marker clipped by a
//! column's width, a column dropped too early on a narrow terminal, a
//! glyph ascii mode missed) were each pinned by a hand-written
//! assertion someone thought to add after the fact. A frame diff shows
//! them in review instead. `cargo insta review` accepts a change; in CI
//! a mismatch fails.
//!
//! The fleet is built rather than taken from the demo fixture: the
//! table renders ages against the real clock, and these frames must not
//! drift with it. Every env has `updated: None`.
//!
//! One invariant runs over every frame as well: in ascii mode nothing
//! decorative is drawn (`support::decorative`).

use super::super::*;
use super::support::*;

/// A fleet with every per-row signal on screen: firing alarms, an
/// unknown alarm count, a DLQ backlog, stale instance counts, a platform
/// version with a newer one available, an updating env.
fn fleet(icons: &str) -> App {
    let cfg = crate::config::Config {
        theme: "dark".into(),
        icons: icons.into(),
        ..crate::config::Config::default()
    };
    let mut app = App::for_tests(crate::aws::AwsClient::stub(), cfg);
    let env = |name: &str, tier: &str, health: &str, status: &str| {
        let mut e = mk_env(name, "shop", tier, health);
        e.status = status.into();
        e.platform = "Node.js 20".into();
        e.platform_version = "6.1.0".into();
        e.solution_stack = "64bit Amazon Linux 2023 v6.1.0 running Node.js 20".into();
        e
    };
    app.environments = vec![
        env("shop-prod", "Web", "Green", "Ready"),
        env("shop-prod-wk", "Worker", "Green", "Ready"),
        env("shop-staging", "Web", "Red", "Ready"),
        env("shop-batch", "Worker", "Yellow", "Updating"),
    ];
    app.caches.latest_stacks = crate::aws::latest_stack_versions(&[
        "64bit Amazon Linux 2023 v6.4.0 running Node.js 20".to_string(),
    ]);
    let counts = |healthy, total| crate::aws::EnvInstanceCounts { healthy, total };
    app.caches
        .firing_alarms
        .set(home_key("shop-prod"), vec!["a".into(), "b".into()]);
    app.caches
        .firing_alarms
        .reconcile(home_key("shop-staging"), Err(()));
    app.caches.worker_dlq.set(home_key("shop-prod-wk"), 3);
    app.caches
        .env_instance_counts
        .set(home_key("shop-prod"), counts(3, 3));
    app.caches
        .env_instance_counts
        .set(home_key("shop-prod-wk"), counts(2, 2));
    app.caches
        .env_instance_counts
        .set(home_key("shop-staging"), counts(0, 2));
    app.caches
        .env_instance_counts
        .set(home_key("shop-batch"), counts(1, 2));
    app.caches
        .env_instance_counts
        .reconcile(home_key("shop-batch"), Err(()));
    app.caches.applications = vec![mk_application("shop")];
    app.view.invalidate();
    app.rebuild_view();
    app.table_state.select(Some(0));
    app
}

/// Render, check the ascii invariant, and return the frame.
fn frame(app: &mut App, icons: &str, w: u16, h: u16) -> String {
    let out = render(app, w, h);
    if icons == "ascii" {
        let found: Vec<char> = out.chars().filter(|c| decorative(*c)).collect();
        assert!(found.is_empty(), "ascii mode drew {found:?}:\n{out}");
    }
    out
}

/// The header carries the build's version and release date, and the
/// border after it is as long as they leave room for: masked, so a
/// release does not rewrite every snapshot.
macro_rules! snapshot {
    ($name:expr, $frame:expr) => {
        insta::with_settings!({filters => vec![
            (r"ebman \d+\.\d+\.\d+\S*( · \d{4}-\d{2}-\d{2})?( ◆)? \][─-]*", "ebman [version] ]"),
        ]}, {
            insta::assert_snapshot!($name, $frame);
        });
    };
}

#[tokio::test]
async fn the_fleet_table() {
    for icons in ["unicode", "ascii"] {
        for width in [80u16, 120, 200] {
            let mut app = fleet(icons);
            let out = frame(&mut app, icons, width, 14);
            snapshot!(format!("fleet_{icons}_{width}"), out);
        }
    }
}

#[tokio::test]
async fn the_fleet_table_across_regions() {
    for icons in ["unicode", "ascii"] {
        let mut app = fleet(icons);
        app.multi_regions = vec!["us-east-1".into(), "eu-west-2".into()];
        let mut away = app.environments[0].clone();
        away.region = Some("eu-west-2".into());
        app.environments.push(away);
        app.view.invalidate();
        app.rebuild_view();
        let out = frame(&mut app, icons, 160, 14);
        snapshot!(format!("fleet_regions_{icons}"), out);
    }
}

#[tokio::test]
async fn the_applications_view() {
    for icons in ["unicode", "ascii"] {
        let mut app = fleet(icons);
        app.set_scope(crate::app::Scope::Apps);
        let out = frame(&mut app, icons, 120, 12);
        snapshot!(format!("apps_{icons}"), out);
    }
}
