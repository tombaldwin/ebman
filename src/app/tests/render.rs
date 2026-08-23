//! Table, header, footer, columns, pills, glyphs, themes.
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
fn action_ssm_run_label_and_glyph() {
    use crate::theme::IconStyle;
    assert_eq!(Action::SsmRun.label(), "Run SSM shell command");
    // Glyph entry exists for all three icon styles (no `_` fall-
    // through panic from the per-icon match).
    assert!(!Action::SsmRun.glyph(IconStyle::Powerline).is_empty());
    assert!(!Action::SsmRun.glyph(IconStyle::Unicode).is_empty());
    assert!(!Action::SsmRun.glyph(IconStyle::Ascii).is_empty());
}

#[test]
fn view_mode_labels() {
    assert_eq!(ViewMode::Default.label(), "default");
    assert_eq!(ViewMode::Compact.label(), "compact");
    assert_eq!(ViewMode::Spacious.label(), "spacious");
}

#[test]
fn render_promotions_orders_newest_first_and_includes_version() {
    let now = chrono::Utc::now();
    let records = vec![
        crate::app::PromotionRecord {
            source: "staging".into(),
            target: "uat".into(),
            version_label: "v1.4.2".into(),
            at: now - chrono::Duration::hours(2),
        },
        crate::app::PromotionRecord {
            source: "uat".into(),
            target: "prod".into(),
            version_label: "v1.4.2".into(),
            at: now - chrono::Duration::minutes(5),
        },
    ];
    let body = crate::app::render_promotions(&records, now);
    let prod_pos = body.find("uat → prod").expect("prod row");
    let staging_pos = body.find("staging → uat").expect("staging row");
    assert!(
        prod_pos < staging_pos,
        "newest (uat → prod, 5m ago) should sort above older (staging → uat, 2h ago)"
    );
    assert!(body.contains("version=v1.4.2"), "version label: {body}");
}

#[test]
fn render_env_resources_tree_shows_asg_with_nested_instances() {
    let mut res = empty_resources();
    res.asgs = vec!["awseb-AWSEBAutoScalingGroup-XYZ".into()];
    res.instances = vec!["i-0abc".into(), "i-0def".into(), "i-0ghi".into()];
    let body = crate::app::render_env_resources_tree(&res, "prod-api", "Web");
    // Section header for ASG group.
    assert!(body.contains("Auto-scaling groups (1)"));
    // ASG node under it (└─ since only one ASG).
    assert!(body.contains("└─ awseb-AWSEBAutoScalingGroup-XYZ"));
    // Instances nested below the ASG with proper tree glyphs.
    assert!(body.contains("├─ i-0abc"));
    assert!(body.contains("├─ i-0def"));
    assert!(body.contains("└─ i-0ghi"));
}

#[test]
fn render_env_resources_tree_skips_empty_sections() {
    let mut res = empty_resources();
    res.asgs = vec!["asg-1".into()];
    // Everything else empty.
    let body = crate::app::render_env_resources_tree(&res, "small-env", "Web");
    assert!(body.contains("Auto-scaling groups (1)"));
    // No load-balancer / launch-config / queue headers when
    // the lists are empty.
    assert!(!body.contains("Load balancers"));
    assert!(!body.contains("Launch configurations"));
    assert!(!body.contains("Queues"));
}

#[test]
fn render_env_resources_tree_handles_zero_resources() {
    let res = empty_resources();
    let body = crate::app::render_env_resources_tree(&res, "fresh-env", "Web");
    assert!(body.contains("(no resources reported"));
}

#[test]
fn short_fingerprint_is_stable_and_diffs() {
    let a = crate::app::short_fingerprint("hunter2");
    let b = crate::app::short_fingerprint("hunter2");
    let c = crate::app::short_fingerprint("hunter3");
    assert_eq!(a, b);
    assert_ne!(a, c);
    assert_eq!(a.len(), 8);
}

#[test]
fn assign_app_colors_stable_first_appearance() {
    use ratatui::style::Color;
    let palette = vec![Color::Red, Color::Green, Color::Blue];
    let names = ["app-a", "app-b", "app-a", "app-c", "app-b"];
    let m = assign_app_colors(names.iter().copied(), &palette);
    assert_eq!(m.get("app-a").copied(), Some(Color::Red));
    assert_eq!(m.get("app-b").copied(), Some(Color::Green));
    assert_eq!(m.get("app-c").copied(), Some(Color::Blue));
    assert_eq!(m.len(), 3);
}

#[test]
fn bucket_delta_only_envs_in_both() {
    let mut prev = HashMap::new();
    prev.insert("a".into(), "Green".into());
    prev.insert("b".into(), "Red".into());
    prev.insert("c".into(), "Green".into()); // c disappears in next, so dropped from delta
    let next = vec![
        fake_env("a", "Ready", "Yellow", "v1"), // Green → Yellow: −1 Green, +1 Yellow
        fake_env("b", "Ready", "Red", "v1"),    // Red → Red: no change
        fake_env("d", "Ready", "Green", "v1"),  // new env: ignored (no prev state)
    ];
    let delta = bucket_delta(&prev, &next, |e| e.health.clone());
    let map: BTreeMap<String, i32> = delta.into_iter().collect();
    // Only env `a` transitions: −1 Green, +1 Yellow. b unchanged; c disappeared (ignored); d is new (ignored).
    assert_eq!(map.get("Green").copied(), Some(-1));
    assert_eq!(map.get("Yellow").copied(), Some(1));
    assert_eq!(map.get("Red").copied(), None);
}

#[test]
fn bucket_delta_empty_prev_yields_no_deltas() {
    // Regression: when prev_health is cleared (e.g. on context switch),
    // the delta against the new env list should produce nothing. Otherwise
    // every env shows up as a transition.
    let prev = HashMap::new();
    let next = vec![
        fake_env("a", "Ready", "Green", "v1"),
        fake_env("b", "Ready", "Red", "v1"),
    ];
    let delta = bucket_delta(&prev, &next, |e| e.health.clone());
    assert!(
        delta.is_empty(),
        "expected no deltas with empty prev, got {delta:?}"
    );
}

#[test]
fn diff_envs_marks_differing_fields() {
    let a = fake_env("prod", "Ready", "Green", "v1");
    let b = fake_env("staging", "Updating", "Yellow", "v2");
    let out = diff_envs(&a, &b, false, &[]);
    // Differing fields prefixed by ≠
    assert!(out.contains("≠ Status"));
    assert!(out.contains("≠ Health"));
    assert!(out.contains("≠ Version"));
    assert!(out.contains("≠ Name"));
    assert!(out.contains("≠ CNAME"));
    // Identical fields prefixed by space
    assert!(out.contains("  Application"));
    assert!(out.contains("  Tier"));
    assert!(out.contains("  Platform"));
}

#[test]
fn diff_envs_redacts_cname() {
    let a = fake_env("prod", "Ready", "Green", "v1");
    let b = fake_env("staging", "Updating", "Yellow", "v2");
    let out = diff_envs(&a, &b, true, &[]);
    // CNAMEs become blocks; the canonical envname-portion shouldn't survive.
    assert!(!out.contains("prod.elb.amazonaws.com"));
    assert!(out.contains("▓"));
}

#[test]
fn diff_envs_drops_ignored_rows() {
    let a = fake_env("prod", "Ready", "Green", "v1");
    let b = fake_env("staging", "Updating", "Yellow", "v2");
    let keys = parse_ignore_keys(Some("version,cname,updated"));
    let out = diff_envs(&a, &b, false, &keys);
    // Ignored rows vanish entirely (no row at all, differing or not).
    assert!(!out.contains("Version"), "Version row should be ignored");
    assert!(!out.contains("CNAME"), "CNAME row should be ignored");
    assert!(!out.contains("Updated"), "Updated row should be ignored");
    // Untouched rows still render.
    assert!(out.contains("≠ Status"));
    assert!(out.contains("  Application"));
}

#[tokio::test]
async fn cycle_saved_view_wraps_forward_through_saved_views() {
    // Three saved views: cycling forward from "dev" → "prod" →
    // "staging" → back to "dev". Cycle order follows BTreeMap
    // iteration (alphabetical), matching the chip-bar render.
    let mut app = test_app();
    app.saved_views.insert(
        "dev".into(),
        crate::app::encode_filter_only_view("tag:env=dev"),
    );
    app.saved_views.insert(
        "prod".into(),
        crate::app::encode_filter_only_view("tag:env=prod"),
    );
    app.saved_views.insert(
        "staging".into(),
        crate::app::encode_filter_only_view("tag:env=staging"),
    );
    // Start on "dev".
    app.view.set_filter("tag:env=dev");
    app.cycle_saved_view(1);
    assert_eq!(app.view.filter().text(), "tag:env=prod");
    app.cycle_saved_view(1);
    assert_eq!(app.view.filter().text(), "tag:env=staging");
    // Wraps back to first.
    app.cycle_saved_view(1);
    assert_eq!(app.view.filter().text(), "tag:env=dev");
}

#[tokio::test]
async fn cycle_saved_view_wraps_backward_and_handles_no_active() {
    // Backward from "dev" wraps to "staging" (last in sort).
    let mut app = test_app();
    app.saved_views.insert(
        "dev".into(),
        crate::app::encode_filter_only_view("tag:env=dev"),
    );
    app.saved_views.insert(
        "staging".into(),
        crate::app::encode_filter_only_view("tag:env=staging"),
    );
    app.view.set_filter("tag:env=dev");
    app.cycle_saved_view(-1);
    assert_eq!(app.view.filter().text(), "tag:env=staging");
    // No active filter (freeform or empty) → forward goes to first,
    // backward goes to last.
    app.view.set_filter("some-random-text");
    app.cycle_saved_view(1);
    assert_eq!(
        app.view.filter().text(),
        "tag:env=dev",
        "forward-with-no-active → first"
    );
    app.view.set_filter("some-random-text");
    app.cycle_saved_view(-1);
    assert_eq!(
        app.view.filter().text(),
        "tag:env=staging",
        "backward-with-no-active → last"
    );
}

#[tokio::test]
async fn cycle_saved_view_noop_with_empty_views() {
    // Cycling when there are no saved views shouldn't crash or
    // mutate state. The keybind guard already short-circuits, but
    // the method itself is the actual safety net.
    let mut app = test_app();
    app.view.set_filter("keep-me");
    app.cycle_saved_view(1);
    assert_eq!(app.view.filter().text(), "keep-me");
}

#[tokio::test]
async fn cycle_saved_view_with_full_view_applies_sort_and_group_too() {
    // The point of unifying named_filters into saved_views: a
    // full view's encoded payload changes sort + group + scope
    // alongside the filter. This is the gh-dash-style "tabs"
    // behavior the BACKLOG had been promising since 2026-05-24.
    let mut app = test_app();
    // Filter-only view (from :save).
    app.saved_views.insert(
        "dev".into(),
        crate::app::encode_filter_only_view("tag:env=dev"),
    );
    // Full view (from :save-view) — flips sort to App + groups.
    app.saved_views.insert(
        "by-app".into(),
        "filter=tag:env=prod;sort=app:asc;grouped=true;scope=envs".into(),
    );
    app.view.set_filter("tag:env=dev");
    app.view.set_grouped(false);
    app.cycle_saved_view(1); // dev → by-app
    assert_eq!(app.view.filter().text(), "tag:env=prod");
    assert!(
        app.view.grouped(),
        "full view must apply its grouped=true alongside the filter"
    );
}

#[test]
fn view_mode_cycle_includes_spacious() {
    assert_eq!(ViewMode::Default.next(), ViewMode::Compact);
    assert_eq!(ViewMode::Compact.next(), ViewMode::Spacious);
    assert_eq!(ViewMode::Spacious.next(), ViewMode::Default);
    assert_eq!(ViewMode::Spacious.label(), "spacious");
}

#[tokio::test]
async fn render_main_table_includes_seeded_env_name() {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod-canary", "uflexi", "Web", "Green")];
    app.rebuild_view();
    let frame = render(&mut app, 160, 24);
    assert!(
        frame.contains("api-prod-canary"),
        "rendered frame should show seeded env name; got:\n{frame}"
    );
}

#[tokio::test]
async fn render_main_table_includes_inst_column_header_and_data() {
    // INST column should appear in the main table header by default
    // (not in hidden_cols) and render the per-env counts when the
    // env_instance_counts cache has data, em-dash when it doesn't.
    let mut app = test_app();
    app.environments = vec![
        mk_env("api-prod", "uflexi", "Web", "Green"),
        mk_env("api-staging", "uflexi", "Web", "Green"),
    ];
    // Seed counts: prod has 3/3 healthy, staging unknown (no entry).
    app.env_instance_counts.insert(
        "api-prod".into(),
        crate::aws::EnvInstanceCounts {
            healthy: 3,
            total: 3,
        },
    );
    app.rebuild_view();
    let frame = render(&mut app, 160, 24);
    assert!(
        frame.contains("INST"),
        "expected INST column header in rendered frame; got:\n{frame}"
    );
    assert!(
        frame.contains("3/3"),
        "expected '3/3' for env with seeded counts; got:\n{frame}"
    );
    // Staging has no counts entry → em-dash placeholder.
    assert!(
        frame.contains("—"),
        "expected em-dash placeholder for env with no counts; got:\n{frame}"
    );
}

#[tokio::test]
async fn render_colours_health_dots_by_tier() {
    // Styled-harness demo: assert the env table paints each row's
    // health indicator in the tier colour, not just that the row
    // exists. The Green row must carry no Red cell (and vice versa),
    // which a text-only assertion can't catch.
    let mut app = test_app();
    app.environments = vec![
        mk_env("svc-red", "uflexi", "Web", "Red"),
        mk_env("svc-green", "uflexi", "Web", "Green"),
    ];
    app.rebuild_view();
    // Clear the cursor so neither asserted row gets the REVERSED
    // selection highlight (which swaps fg/bg and would mask the dot).
    app.table_state.select(None);
    let buf = render_buf(&mut app, 120, 30);
    let theme = app.theme.clone();
    let red_row = find_row(&buf, "svc-red").expect("red env row rendered");
    let green_row = find_row(&buf, "svc-green").expect("green env row rendered");
    assert!(
        row_has_fg(&buf, red_row, theme.health_red),
        "Red env row should paint a health_red cell"
    );
    assert!(
        row_has_fg(&buf, green_row, theme.health_green),
        "Green env row should paint a health_green cell"
    );
    assert!(
        !row_has_fg(&buf, green_row, theme.health_red),
        "Green env row must not paint any health_red cell"
    );
}

#[tokio::test]
async fn render_demo_ironwood_row_shows_muted_dashes() {
    // The IRONWOOD demo tell: the `ironwood` env is absent from the
    // cost + instance-count maps, so its INST and COST cells render a
    // muted `—` ("Beanstalk can't account for it"). Drives the real
    // demo fixture so this is the on-screen artifact, not a synthetic.
    let mut app = test_app();
    crate::demo_fixture::install(&mut app);
    app.table_state.select(None); // avoid the REVERSED selection mask
    let theme = app.theme.clone();
    let buf = render_buf(&mut app, 160, 30);
    let y = find_row(&buf, "ironwood").expect("ironwood row rendered");
    // INST + COST both muted em-dashes → at least two such cells.
    assert!(
        count_symbol_fg(&buf, y, "—", theme.muted) >= 2,
        "ironwood row should show muted — for INST and COST"
    );
}

#[tokio::test]
async fn render_redact_masks_the_cname() {
    // `:redact` (Ctrl-X) blanks sensitive columns — the CNAME renders
    // as ▓ blocks, and the real hostname no longer appears.
    let mut app = test_app();
    app.environments = vec![mk_env("svc", "uflexi", "Web", "Green")];
    app.rebuild_view();
    app.table_state.select(None);
    app.view.redact = false;
    let shown = render(&mut app, 160, 30);
    assert!(shown.contains("svc.example.com"), "cname visible when off");
    app.view.redact = true;
    let hidden = render(&mut app, 160, 30);
    assert!(
        !hidden.contains("svc.example.com"),
        "cname must be masked when redact is on"
    );
    assert!(hidden.contains('▓'), "redacted cells render as ▓ blocks");
}

#[tokio::test]
async fn ascii_icon_mode_renders_no_unicode_arrows() {
    // The end-to-end half of `every_status_glyph_has_an_ascii_form`:
    // the pure helpers can be right while a call site still hardcodes
    // the glyph. Five did — the header delta arrows, the sort marker,
    // and the Metrics anomaly badge, which baked `▲` into its message
    // string where a glyph-helper grep wouldn't find it.
    let cfg = crate::config::Config {
        icons: "ascii".into(),
        ..crate::config::Config::default()
    };
    let mut app = App::for_tests(crate::aws::AwsClient::stub(), cfg);
    app.environments = vec![
        mk_env("api-prod", "uflexi", "Web", "Red"),
        mk_env("api-staging", "uflexi", "Web", "Green"),
    ];
    app.rebuild_view();
    app.table_state.select(Some(0));
    // Header deltas render only when a bucket moved.
    app.health_delta = vec![("Red".to_string(), 1)];
    app.status_delta = vec![("Ready".to_string(), -1)];

    let out = render(&mut app, 160, 44);
    for g in ['▲', '▼'] {
        assert!(!out.contains(g), "ascii mode rendered {g}:\n{out}");
    }
    // The information the glyphs carry is still there.
    assert!(
        out.contains('^') || out.contains('v'),
        "the ascii forms replaced them:\n{out}"
    );
}

#[tokio::test]
async fn the_anomaly_badge_is_ascii_at_its_call_site_too() {
    // `series_anomaly_label` takes an `IconStyle` and its unit test
    // pins that. What that test can't see is whether the CALL SITE
    // passes `theme.icons` or hardcodes `Unicode` — and call sites are
    // where every regression in this release cycle actually lived.
    // Verified by mutation: hardcoding the glyph inside the function
    // leaves the fleet-view ascii test green, because that frame never
    // renders the Metrics tab.
    let cfg = crate::config::Config {
        icons: "ascii".into(),
        ..crate::config::Config::default()
    };
    let mut app = App::for_tests(crate::aws::AwsClient::stub(), cfg);
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Red")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();

    let detail = app.detail.as_mut().expect("detail opened");
    detail.tab_idx = detail
        .tabs
        .iter()
        .position(|t| *t == DetailTab::Metrics)
        .expect("Metrics tab present");
    detail.loading_metrics = false;
    // A flat baseline then a spike — the shape `series_anomaly_label`
    // fires on for a 5xx series.
    let now = chrono::Utc::now();
    detail.metrics = vec![crate::aws::MetricSeries {
        id: "req5xx".into(),
        label: "5xx".into(),
        points: (0..6)
            .map(|i| {
                let v = if i == 5 { 99.0 } else { 1.0 };
                (now - chrono::Duration::minutes(6 - i as i64), v)
            })
            .collect(),
    }];

    let out = render(&mut app, 160, 44);
    assert!(
        out.contains("anomaly"),
        "the badge has to be on screen for this test to mean anything:\n{out}"
    );
    assert!(
        !out.contains('▲'),
        "ascii mode rendered ▲ in the anomaly badge:\n{out}"
    );
}

#[tokio::test]
async fn a_red_env_gets_a_red_status_pill_in_the_table() {
    // `status_alert()` has unit tests; nothing checked that the TABLE
    // uses its result. Forcing `StatusAlert::None` at the call site —
    // which strips the alert colour from every Red env's STATUS pill,
    // the thing that says "this one" at a glance during triage —
    // passed all 1,097 tests.
    //
    // HEALTH and TREND are hidden because they colour a Red row red on
    // their own: the FIRST version of this test asserted on the row
    // and passed under the very mutation it was written to catch,
    // because the health dot satisfied it. With them hidden, the
    // status pill is the only thing that can make this row red.
    let mut app = test_app();
    app.view.hidden_cols.insert("HEALTH".into());
    app.view.hidden_cols.insert("TREND".into());

    let mut calm = mk_env("api-calm", "uflexi", "Web", "Green");
    calm.status = "Ready".into();
    let mut red = mk_env("api-red", "uflexi", "Web", "Red");
    red.status = "Ready".into();
    app.environments = vec![calm, red];
    app.rebuild_view();

    let buf = render_buf(&mut app, 170, 20);
    let calm_row = find_row(&buf, "api-calm").expect("calm row rendered");
    let red_row = find_row(&buf, "api-red").expect("red row rendered");

    assert!(
        row_has_fg(&buf, red_row, app.theme.health_red),
        "a Red env's STATUS pill must carry the alert colour"
    );
    assert!(
        !row_has_fg(&buf, calm_row, app.theme.health_red),
        "and a healthy env's must not — otherwise the assertion above \
         proves nothing about the alert"
    );
}

#[tokio::test]
async fn the_group_separator_row_renders_its_app_and_summary() {
    // `:group on` draws a separator between applications carrying the
    // next app's name and a "N envs · M red" summary. Extracting that
    // 137-line match arm into `separator_row` was a pure refactor — but
    // the arm turned out to have NO coverage at all (stub it to an empty
    // row and all 1,135 tests still passed), so the refactor was
    // unverified. This is the test that makes it verifiable.
    let mut app = test_app();
    app.environments = vec![
        mk_env("alpha-prod", "ALPHAAPP", "Web", "Green"),
        mk_env("beta-prod", "BETAAPP", "Web", "Red"),
        mk_env("beta-staging", "BETAAPP", "Web", "Green"),
    ];
    app.view.invalidate();
    app.rebuild_view();
    app.execute_command("group on");
    assert!(app.view.grouped(), "grouping is on");

    let out = render(&mut app, 190, 30);
    assert!(
        out.contains("BETAAPP"),
        "the separator names the group it introduces:\n{out}"
    );
    assert!(
        out.contains("2 envs") && out.contains("1 red"),
        "and summarises it — this is the per-app health an operator \
         reads without expanding anything:\n{out}"
    );
}

#[tokio::test]
async fn a_stale_view_cache_drops_rows_instead_of_panicking() {
    // `ViewState`'s rows hold indices into `environments`, which is one
    // of the four inputs it does NOT own — so a mutation that forgets
    // `view.invalidate()` leaves indices pointing past the end.
    //
    // `assert_fresh` is deliberately softened in release on the
    // reasoning that "one wrong frame is better than a panic in the alt
    // screen". Unchecked indexing made the wrong frame BE the panic,
    // which defeated the softening entirely. Simulate the stale state
    // by shrinking `environments` behind the cache's back — exactly
    // what a missed `invalidate()` produces.
    let mut app = test_app();
    app.environments = vec![
        mk_env("alpha", "uflexi", "Web", "Green"),
        mk_env("beta", "uflexi", "Web", "Green"),
        mk_env("gamma", "uflexi", "Web", "Green"),
    ];
    app.view.invalidate();
    app.rebuild_view();
    assert_eq!(app.view.display().len(), 3, "cache holds three rows");

    // The mutation an author forgets to follow with `invalidate()`.
    app.environments.truncate(1);

    // Must not panic, and must still draw the row that does exist.
    let out = render(&mut app, 150, 20);
    assert!(
        out.contains("alpha"),
        "the surviving row still renders:\n{out}"
    );
    assert!(
        !out.contains("gamma"),
        "and the dropped one is simply absent:\n{out}"
    );

    // The exports read the same cached indices.
    app.export_json();
    app.export_markdown();
}
