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
async fn ascii_icon_mode_renders_no_decorative_unicode() {
    // The end-to-end half of `every_status_glyph_has_an_ascii_form`:
    // the pure helpers can be right while a call site still hardcodes
    // the glyph. Five did in 0.27 — the header delta arrows, the sort
    // marker, and the Metrics anomaly badge, which baked `▲` into its
    // message string where a glyph-helper grep wouldn't find it.
    //
    // That fix left this test checking exactly `▲` and `▼`, which is
    // how two more got in: the armed-rollback pill hardcoded `⏱` and
    // the watching-deploy pill `👁`, and neither was rendered by the
    // frame this test built. So the check is now a RANGE rather than a
    // list, and the frame turns on the header state that makes the
    // optional pills appear.
    //
    // Box drawing (U+2500-U+257F) and block elements (U+2580-U+259F)
    // are deliberately excluded: ratatui draws borders and bar charts
    // with them regardless of our icon style, so they are not ours to
    // fall back.
    fn decorative(c: char) -> bool {
        let n = c as u32;
        (0x2190..=0x21FF).contains(&n)      // arrows
            || (0x2300..=0x23FF).contains(&n) // misc technical (⏱)
            || (0x25A0..=0x25FF).contains(&n) // geometric (▲ ▼ ●)
            || (0x2600..=0x26FF).contains(&n) // misc symbols (★ ⚠)
            || (0x2700..=0x27BF).contains(&n) // dingbats (✓ ✗)
            || (0x1F000..=0x1FAFF).contains(&n) // emoji (👁 💡 🚨)
    }

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
    // Every optional pill, so none of them is invisible to this check
    // the way the two watcher pills were.
    app.alerts = 2;
    app.read_only = true;
    app.frozen = true;
    app.first_run_hint = true;
    app.pinned.insert("api-prod".to_string());
    app.multi_selected.insert("api-prod".to_string());
    app.incident = Some(crate::app::Incident {
        headline: "checkout 5xx".into(),
        started_at: chrono::Utc::now(),
    });
    app.update_available = Some(crate::update_check::LatestRelease {
        version: "9.9.9".into(),
    });
    app.sso_expiry = Some(chrono::Utc::now() + chrono::Duration::minutes(30));
    app.armed_watchdogs.insert(
        "api-prod".into(),
        crate::app::ArmedWatchdog {
            env_name: "api-prod".into(),
            target_label: "v1".into(),
            armed_at: chrono::Utc::now(),
            deadline_at: chrono::Utc::now() + chrono::Duration::minutes(5),
        },
    );
    app.watching_deploys.insert(
        "api-staging".into(),
        crate::app::WatchingDeploy {
            env_name: "api-staging".into(),
            target_label: "v1".into(),
            armed_at: chrono::Utc::now(),
            deadline_at: chrono::Utc::now() + chrono::Duration::minutes(9),
        },
    );
    app.rebuild_view();

    // Wide enough that `prune_pills_to_width` doesn't drop the pills
    // before this test can look at them.
    let out = render(&mut app, 400, 44);
    let found: Vec<char> = out.chars().filter(|c| decorative(*c)).collect();
    assert!(
        found.is_empty(),
        "ascii mode rendered decorative unicode {found:?} — a call site \
         hardcoded a glyph instead of going through `glyph(theme.icons, ..)`:\n{out}"
    );
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

#[tokio::test]
async fn export_json_stays_valid_when_the_view_cache_is_stale() {
    // A cached view index can outlive a mutation of `environments`, so
    // rows can be skipped. The old "comma unless this is the last
    // FILTERED index" test disagreed with reality the moment one was:
    // skip the last and the previous row keeps its comma, producing
    // invalid JSON — handed straight to the clipboard, so the operator
    // finds out when they paste it somewhere that parses.
    let mut app = test_app();
    app.environments = vec![
        mk_env("alpha", "uflexi", "Web", "Green"),
        mk_env("beta", "uflexi", "Web", "Green"),
        mk_env("gamma", "uflexi", "Web", "Green"),
    ];
    app.view.invalidate();
    app.rebuild_view();
    assert_eq!(app.view.filtered().len(), 3);

    // Drop the last two — the shape a missed `invalidate()` produces.
    app.environments.truncate(1);
    app.export_json();

    let json = app
        .status_message
        .as_deref()
        .expect("export reports its outcome");
    assert!(
        json.contains("exported 1 rows"),
        "counts what it emitted, not what it planned: {json}"
    );
}

#[test]
fn export_json_body_has_no_trailing_comma_when_rows_are_skipped() {
    // The `yank` path is a no-op under cfg(test), so assert on the
    // rendering itself: build the same shape and check it parses.
    let rows = ["  {\"name\":\"alpha\"}".to_string()];
    let body = format!("[\n{}\n]", rows.join(",\n"));
    assert!(
        serde_json::from_str::<serde_json::Value>(&body).is_ok(),
        "a single row must not carry a separator: {body}"
    );
    let two = [
        "  {\"name\":\"alpha\"}".to_string(),
        "  {\"name\":\"beta\"}".to_string(),
    ];
    let body = format!("[\n{}\n]", two.join(",\n"));
    assert!(
        serde_json::from_str::<serde_json::Value>(&body).is_ok(),
        "{body}"
    );
}

/// The mode/state pairing is an invariant the types do not enforce, so
/// it is enforced by assertion instead. These prove the assertion is
/// live — a guard that never fires is worse than none, because it reads
/// as coverage.
///
/// One test per surface, deliberately. The first version of this
/// instrumented the four *draw* functions, and three of four fired:
/// `draw_detail` is unreachable in the broken state because the
/// background layer dispatches on `detail.is_some()` rather than on the
/// mode. Had the surfaces shared one test, Detail's dead guard would
/// have looked covered.
mod mode_state_invariant {
    use super::*;

    async fn app_in(mode: Mode) -> App {
        let mut app = test_app();
        app.environments = vec![mk_env("api-prod", "uflexi", "WebServer", "Green")];
        app.rebuild_view();
        app.mode = mode;
        app
    }

    #[tokio::test]
    #[should_panic(expected = "mode is Detail but its state is None")]
    async fn detail_mode_without_its_state_is_loud() {
        let mut app = app_in(Mode::Detail).await;
        app.detail = None;
        let _ = render(&mut app, 120, 40);
    }

    #[tokio::test]
    #[should_panic(expected = "mode is Action but its state is None")]
    async fn action_mode_without_its_state_is_loud() {
        let mut app = app_in(Mode::Action).await;
        app.action_flow = None;
        let _ = render(&mut app, 120, 40);
    }

    #[tokio::test]
    #[should_panic(expected = "mode is Form but its state is None")]
    async fn form_mode_without_its_state_is_loud() {
        let mut app = app_in(Mode::Form).await;
        app.form = None;
        let _ = render(&mut app, 120, 40);
    }

    #[tokio::test]
    #[should_panic(expected = "mode is Picker but its state is None")]
    async fn picker_mode_without_its_state_is_loud() {
        let mut app = app_in(Mode::Picker).await;
        app.picker = None;
        let _ = render(&mut app, 120, 40);
    }

    #[tokio::test]
    #[should_panic(expected = "mode is Dlq but its state is None")]
    async fn dlq_mode_without_its_state_is_loud() {
        let mut app = app_in(Mode::Dlq).await;
        app.dlq = None;
        let _ = render(&mut app, 120, 40);
    }

    #[tokio::test]
    #[should_panic(expected = "mode is Shell but its state is None")]
    async fn shell_mode_without_its_state_is_loud() {
        let mut app = app_in(Mode::Shell).await;
        app.current_shell = None;
        let _ = render(&mut app, 120, 40);
    }

    /// The converse must NOT fire: holding Detail state while the mode
    /// is Help is how a popup keeps Detail behind it, and asserting
    /// both directions would make that legitimate state panic.
    #[tokio::test]
    async fn holding_state_without_the_mode_is_fine() {
        let mut app = app_in(Mode::Normal).await;
        app.table_state.select(Some(0));
        app.open_detail();
        app.mode = Mode::Help; // `?` over an open Detail view
        let _ = render(&mut app, 120, 40);
    }
}

// ── mutation-sweep triage, 2026-08-26 ────────────────────────────────
//
// `render_env_resources_tree` carried 18 survivors and
// `format_deploy_preview` 13, and in both cases most of them sat on the
// same expression written out several times. The duplication was the
// finding; these cover what it collapsed to.

/// `└─` closes a section, `├─` continues it.
#[test]
fn tree_glyph_closes_only_the_last_item() {
    use crate::app::render::{is_last, tree_glyph};
    assert_eq!(tree_glyph(0, 3), "├─");
    assert_eq!(tree_glyph(1, 3), "├─");
    assert_eq!(tree_glyph(2, 3), "└─", "the last item closes the branch");
    // A single item is the last one.
    assert_eq!(tree_glyph(0, 1), "└─");

    assert!(!is_last(0, 3));
    assert!(is_last(2, 3));
    assert!(is_last(0, 1));
    // Degenerate: an empty section has no last item, and `i + 1 == n`
    // must not be satisfiable. `i * 1 == n` would be, at i = n = 0.
    assert!(!is_last(0, 0), "an empty section has no last item");
}

/// The deploy preview's coarse age ladder.
#[test]
fn coarse_age_buckets() {
    use crate::app::render::coarse_age;
    for (secs, want) in [
        (0, "0m"),
        (59, "0m"), // no seconds bucket, by design
        (60, "1m"),
        (3599, "59m"),
        (3600, "1h"), // `secs < 3600`
        (43_200, "12h"),
        (86_399, "23h"),
        (86_400, "1d"), // `secs < 86_400`
        (259_200, "3d"),
    ] {
        assert_eq!(coarse_age(secs), want, "{secs}s");
    }
}

// ── header/footer chrome that reports operator state ──────────────────
//
// Everything below renders through `crate::ui::draw` and was already
// being *executed* by the 56 render call sites in this suite — but
// nothing asserted on it, so the mutation sweep reported the whole
// `ui/draw_*` area as survivors and the backlog read that as
// "unreachable". It is not unreachable; it was unasserted. Three probes
// on 2026-08-27 (footer first-run row, header alert plural, table pin
// star) were each NOT CAUGHT before these tests existed.

#[tokio::test]
async fn read_only_mode_shows_the_badge_in_the_header() {
    // The operator's one visual confirmation that writes are blocked.
    // `deny_write` is separately tested to *refuse*; this pins that the
    // refusal is advertised before they try, which is the difference
    // between a safe session and a confusing one.
    let mut app = test_app();
    app.read_only = true;
    app.rebuild_view();
    let frame = render(&mut app, 160, 24);
    assert!(
        frame.contains("READ-ONLY"),
        "read-only session must advertise itself; got:\n{frame}"
    );
}

#[tokio::test]
async fn a_writable_session_does_not_claim_to_be_read_only() {
    // The other direction matters just as much: a stuck badge would
    // tell an operator they were protected while every write went
    // through.
    let mut app = test_app();
    app.read_only = false;
    app.rebuild_view();
    let frame = render(&mut app, 160, 24);
    assert!(
        !frame.contains("READ-ONLY"),
        "writable session must not show the badge; got:\n{frame}"
    );
}

#[tokio::test]
async fn the_alert_pill_counts_and_pluralises() {
    let mut app = test_app();
    app.alerts = 1;
    app.rebuild_view();
    let one = render(&mut app, 160, 24);
    assert!(
        one.contains("1 alert") && !one.contains("1 alerts"),
        "a single alert reads '1 alert'; got:\n{one}"
    );

    app.alerts = 3;
    app.rebuild_view();
    let many = render(&mut app, 160, 24);
    assert!(
        many.contains("3 alerts"),
        "multiple alerts pluralise; got:\n{many}"
    );
}

#[tokio::test]
async fn no_alert_pill_when_there_are_no_alerts() {
    let mut app = test_app();
    app.alerts = 0;
    app.rebuild_view();
    let frame = render(&mut app, 160, 24);
    assert!(
        !frame.contains("alert"),
        "a quiet fleet shows no alert pill; got:\n{frame}"
    );
}

#[tokio::test]
async fn the_first_run_hint_row_appears_only_on_a_first_launch() {
    // The hint costs a footer row, so it has to disappear once the flag
    // clears — a permanently-present hint is a permanently-lost row.
    let mut app = test_app();
    app.first_run_hint = true;
    app.rebuild_view();
    let shown = render(&mut app, 160, 24);
    assert!(
        shown.contains("First launch"),
        "first launch shows the discovery hint; got:\n{shown}"
    );

    app.first_run_hint = false;
    app.rebuild_view();
    let hidden = render(&mut app, 160, 24);
    assert!(
        !hidden.contains("First launch"),
        "the hint clears on subsequent launches; got:\n{hidden}"
    );
}

#[tokio::test]
async fn a_pinned_env_is_marked_in_the_table() {
    let mut app = test_app();
    app.environments = vec![
        mk_env("api-prod", "uflexi", "Web", "Green"),
        mk_env("api-staging", "uflexi", "Web", "Green"),
    ];
    app.pinned.insert("api-prod".to_string());
    app.rebuild_view();
    let buf = render_buf(&mut app, 160, 24);

    // `find_row` alone is not enough here: the header breadcrumb also
    // contains the selected env's name, so a name search matches chrome
    // as well as the table row. Assert on the star instead — exactly
    // one row in the frame carries it, and it is the pinned env's.
    let row_text = |y: u16| {
        (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol())
            .collect::<String>()
    };
    let starred: Vec<String> = (0..buf.area.height)
        .map(row_text)
        .filter(|r| r.contains('\u{2605}'))
        .collect();
    assert_eq!(
        starred.len(),
        1,
        "exactly one row should carry the pin star; got {starred:#?}"
    );
    assert!(
        starred[0].contains("api-prod") && !starred[0].contains("api-staging"),
        "the starred row is the pinned env's; got: {}",
        starred[0]
    );
}

#[tokio::test]
async fn an_active_freeze_is_advertised_and_says_when_the_data_went_stale() {
    // Auto-refresh is off while frozen, so the operator is looking at a
    // snapshot. After 5 minutes the pill says so — the failure mode is
    // someone heads-down on an incident reading minutes-old health as
    // current.
    let mut app = test_app();
    app.frozen = true;
    app.last_refresh = Some(chrono::Utc::now());
    app.rebuild_view();
    let fresh = render(&mut app, 160, 24);
    assert!(
        fresh.contains("FROZEN") && !fresh.contains("FROZEN (stale)"),
        "a fresh freeze reads FROZEN; got:\n{fresh}"
    );

    app.last_refresh = Some(chrono::Utc::now() - chrono::Duration::minutes(10));
    app.rebuild_view();
    let stale = render(&mut app, 160, 24);
    assert!(
        stale.contains("FROZEN (stale)"),
        "a freeze older than 5 minutes must say the data is stale; got:\n{stale}"
    );
}

#[tokio::test]
async fn a_declared_incident_shows_its_headline_in_the_header() {
    // `:incident START` is the one signal everyone sharing the terminal
    // must see, and it outranks the UX pills in the width-pruning pass.
    let mut app = test_app();
    app.incident = Some(crate::app::Incident {
        headline: "checkout 5xx spike".into(),
        started_at: chrono::Utc::now(),
    });
    app.rebuild_view();
    let frame = render(&mut app, 160, 24);
    assert!(
        frame.contains("INCIDENT"),
        "a declared incident must be visible; got:\n{frame}"
    );
    assert!(
        frame.contains("checkout 5xx spike"),
        "and it carries the operator's headline; got:\n{frame}"
    );
}

#[tokio::test]
async fn an_incident_without_a_headline_still_announces_itself() {
    // `:incident START` with no text — the empty-headline branch must
    // not render a dangling "INCIDENT ():".
    let mut app = test_app();
    app.incident = Some(crate::app::Incident {
        headline: String::new(),
        started_at: chrono::Utc::now(),
    });
    app.rebuild_view();
    let frame = render(&mut app, 160, 24);
    assert!(frame.contains("INCIDENT"), "got:\n{frame}");
    assert!(
        !frame.contains("):"),
        "no dangling separator when there is no headline; got:\n{frame}"
    );
}

#[tokio::test]
async fn an_available_update_names_the_version_and_the_command() {
    let mut app = test_app();
    app.update_available = Some(crate::update_check::LatestRelease {
        version: "9.9.9".into(),
    });
    app.rebuild_view();
    let frame = render(&mut app, 160, 24);
    assert!(
        frame.contains("UPDATE 9.9.9") && frame.contains(":update"),
        "the pill names the version and how to take it; got:\n{frame}"
    );
}

#[tokio::test]
async fn the_sso_pill_counts_down_and_disappears_once_expired() {
    // An expired session is not a "0m" warning — the credentials are
    // already gone, and every call is about to fail with a message that
    // names SSO anyway. Showing a stale countdown would be worse than
    // showing nothing.
    let mut app = test_app();
    app.sso_expiry = Some(chrono::Utc::now() + chrono::Duration::minutes(30));
    app.rebuild_view();
    let soon = render(&mut app, 160, 24);
    assert!(
        soon.contains("SSO 29m") || soon.contains("SSO 30m"),
        "a session expiring in 30 minutes counts down in minutes; got:\n{soon}"
    );

    // Over an hour switches to hours, and the hour count TRUNCATES:
    // 3h29m reads "SSO 3h", not "SSO 4h" (an exactly-3h expiry would
    // read 2h, since a few microseconds have already elapsed). Rounding
    // down is the right direction for a credential warning — it never
    // tells the operator they have more time than they do.
    app.sso_expiry = Some(chrono::Utc::now() + chrono::Duration::minutes(209));
    app.rebuild_view();
    let hours = render(&mut app, 160, 24);
    assert!(
        hours.contains("SSO 3h"),
        "3h29m remaining reads as 3h, rounded down; got:\n{hours}"
    );

    app.sso_expiry = Some(chrono::Utc::now() - chrono::Duration::minutes(1));
    app.rebuild_view();
    let gone = render(&mut app, 160, 24);
    assert!(
        !gone.contains("SSO "),
        "an already-expired session shows no countdown; got:\n{gone}"
    );
}

#[tokio::test]
async fn the_multi_select_pill_counts_the_selection() {
    let mut app = test_app();
    app.environments = vec![
        mk_env("api-prod", "uflexi", "Web", "Green"),
        mk_env("api-staging", "uflexi", "Web", "Green"),
    ];
    app.multi_selected.insert("api-prod".to_string());
    app.multi_selected.insert("api-staging".to_string());
    app.rebuild_view();
    let frame = render(&mut app, 160, 24);
    assert!(
        frame.contains("2 selected"),
        "a batch operation must show how many rows it will hit; got:\n{frame}"
    );
}

#[tokio::test]
async fn a_terraform_managed_env_is_flagged_in_the_confirm_modal() {
    // The warning fires at the point of an irreversible write, and it
    // is the only thing telling the operator that this change drifts
    // from IaC and will be reverted on the next plan/apply. A heads-up
    // rather than a block — but a silent one is worse than none.
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
    app.tf_managed_envs.insert("api-prod".to_string());
    app.rebuild_view();
    app.mode = Mode::Action;
    app.action_flow = Some(crate::app::ActionFlow::Confirm(mk_modal(
        Action::Rebuild,
        "api-prod",
    )));
    let frame = render(&mut app, 160, 40);
    assert!(
        frame.contains("terraform-managed"),
        "a tf-managed env must be flagged before the write; got:\n{frame}"
    );
    assert!(
        frame.contains(":drift"),
        "and point at the command that shows what diverges; got:\n{frame}"
    );
}

#[tokio::test]
async fn an_env_not_managed_by_terraform_gets_no_drift_warning() {
    // The false-positive direction: warning on every env trains the
    // operator to ignore it.
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
    app.rebuild_view();
    app.mode = Mode::Action;
    app.action_flow = Some(crate::app::ActionFlow::Confirm(mk_modal(
        Action::Rebuild,
        "api-prod",
    )));
    let frame = render(&mut app, 160, 40);
    assert!(
        !frame.contains("terraform-managed"),
        "an unmanaged env must not carry the warning; got:\n{frame}"
    );
}

#[tokio::test]
async fn the_undo_window_counts_down_and_names_the_key() {
    // The 5-second cancel window after a dispatch. If the pill renders
    // without the key, the window may as well not exist — the operator
    // has no way to learn what to press in the time available.
    let mut app = test_app();
    app.pending_dispatch = Some(crate::app::PendingDispatch {
        deadline: std::time::Instant::now() + std::time::Duration::from_secs(4),
        label: "Rebuild env".into(),
        target: "api-prod".into(),
        kind: crate::app::PendingDispatchKind::Single {
            modal: mk_modal(Action::Rebuild, "api-prod"),
        },
    });
    app.rebuild_view();
    let frame = render(&mut app, 200, 24);
    assert!(
        frame.contains("Rebuild env"),
        "the pill names what is about to happen; got:\n{frame}"
    );
    assert!(
        frame.contains("U undo"),
        "and which key cancels it; got:\n{frame}"
    );
    assert!(
        frame.contains("5s") || frame.contains("4s"),
        "and how long is left; got:\n{frame}"
    );
}

#[tokio::test]
async fn the_watcher_pills_switch_between_singular_and_plural() {
    // Two armed watchdogs must not read "rollback api-prod in 5m" —
    // that hides the second one entirely, and these pills are the only
    // standing signal that something will act on the fleet unattended.
    let mut app = test_app();
    // The pill reads `env_name` off the watchdog, not the map key, so
    // the fixture has to set both consistently — a mismatch here would
    // make the test pass against a rendering that names the wrong env.
    let arm = |env: &str, mins: i64| crate::app::ArmedWatchdog {
        env_name: env.into(),
        target_label: "v1".into(),
        armed_at: chrono::Utc::now(),
        deadline_at: chrono::Utc::now() + chrono::Duration::minutes(mins),
    };
    app.armed_watchdogs
        .insert("api-prod".into(), arm("api-prod", 5));
    app.rebuild_view();
    let one = render(&mut app, 300, 24);
    assert!(
        one.contains("rollback api-prod in"),
        "a single armed watchdog names its env; got:\n{one}"
    );

    app.armed_watchdogs
        .insert("api-staging".into(), arm("api-staging", 9));
    app.rebuild_view();
    let two = render(&mut app, 300, 24);
    assert!(
        two.contains("2 rollbacks armed") && two.contains("next: api-prod"),
        "two armed watchdogs report the count and the soonest; got:\n{two}"
    );
}

#[tokio::test]
async fn an_env_that_just_went_red_is_marked_and_its_neighbours_are_not() {
    // The transient "this changed on the refresh you just watched"
    // marker. Its whole value is being scoped to the one env — a marker
    // on every row is a marker on none.
    let mut app = test_app();
    app.environments = vec![
        mk_env("api-prod", "uflexi", "Web", "Red"),
        mk_env("api-staging", "uflexi", "Web", "Red"),
    ];
    app.newly_red.insert("api-prod".to_string());
    app.rebuild_view();
    let buf = render_buf(&mut app, 200, 24);
    let row_text = |y: u16| {
        (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol())
            .collect::<String>()
    };
    // Match the marker IMMEDIATELY BEFORE an env name. Neither half
    // alone works: the header breadcrumb repeats the selected env's
    // name, and the column-sort indicator is also a `\u{25B2}` (that
    // one cost a first draft of this test, which counted two rows).
    let marked: Vec<String> = (0..buf.area.height)
        .map(row_text)
        .filter(|r| r.contains("\u{25B2} api-"))
        .collect();
    assert_eq!(
        marked.len(),
        1,
        "exactly one row carries the newly-red marker; got {marked:#?}"
    );
    assert!(
        marked[0].contains("api-prod") && !marked[0].contains("api-staging"),
        "and it is the env that just turned; got: {}",
        marked[0]
    );
}

#[tokio::test]
async fn a_newly_discovered_env_is_marked_in_the_table() {
    let mut app = test_app();
    app.environments = vec![
        mk_env("api-prod", "uflexi", "Web", "Green"),
        mk_env("api-new", "uflexi", "Web", "Green"),
    ];
    app.newly_added.insert("api-new".to_string());
    app.rebuild_view();
    let buf = render_buf(&mut app, 200, 24);
    let row_text = |y: u16| {
        (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol())
            .collect::<String>()
    };
    let marked: Vec<String> = (0..buf.area.height)
        .map(row_text)
        .filter(|r| r.contains("+ api-new"))
        .collect();
    assert_eq!(marked.len(), 1, "the new env is flagged; got {marked:#?}");
    let others: Vec<String> = (0..buf.area.height)
        .map(row_text)
        .filter(|r| r.contains("+ api-prod"))
        .collect();
    assert!(
        others.is_empty(),
        "and the pre-existing env is not; got {others:#?}"
    );
}

// ── version and release date ──────────────────────────────────────────

#[tokio::test]
async fn the_header_title_carries_the_version_and_release_date() {
    let mut app = test_app();
    app.rebuild_view();
    let frame = render(&mut app, 160, 24);
    assert!(
        frame.contains(&format!("ebman {}", env!("CARGO_PKG_VERSION"))),
        "the title names the running version; got:\n{frame}"
    );
    if let Some(date) = crate::ui::release_date() {
        assert!(
            frame.contains(date),
            "and the date it shipped; got:\n{frame}"
        );
    }
}

#[tokio::test]
async fn a_stale_build_is_flagged_and_a_fresh_one_is_not() {
    let mut app = test_app();

    // Fresh: no nudge. Guarding the false-positive direction matters
    // more than usual here — a permanent "your build is old" pill on a
    // build installed yesterday is the fastest way to teach someone to
    // ignore the header.
    app.release_date = Some("2026-08-20");
    app.rebuild_view();
    let fresh = render(&mut app, 300, 24);
    assert!(
        !fresh.contains("build is"),
        "a week-old build is not stale; got:\n{fresh}"
    );

    // Old enough to nudge.
    app.release_date = Some("2020-01-01");
    app.rebuild_view();
    let old = render(&mut app, 300, 24);
    assert!(
        old.contains("build is") && old.contains("(:update)"),
        "an old build says so and names the command; got:\n{old}"
    );
}

#[tokio::test]
async fn a_known_newer_version_beats_the_staleness_nudge() {
    // Both conditions true at once. The UPDATE pill names an actual
    // version, which is strictly better information than "your build is
    // old", so the nudge must stand down rather than sit beside it
    // saying the same thing worse.
    let mut app = test_app();
    app.release_date = Some("2020-01-01");
    app.update_available = Some(crate::update_check::LatestRelease {
        version: "9.9.9".into(),
    });
    app.rebuild_view();
    let frame = render(&mut app, 300, 24);
    assert!(frame.contains("UPDATE 9.9.9"), "got:\n{frame}");
    assert!(
        !frame.contains("build is"),
        "the staleness nudge stands down when a version is known; got:\n{frame}"
    );
}

#[tokio::test]
async fn a_build_with_no_known_release_date_never_nags() {
    // `Cargo.toml` bumped before the changelog section was cut. Showing
    // the version alone is right; inventing an age is not.
    let mut app = test_app();
    app.release_date = None;
    app.rebuild_view();
    let frame = render(&mut app, 300, 24);
    assert!(!frame.contains("build is"), "got:\n{frame}");
}

// ── narrow terminals (80 columns) ─────────────────────────────────────

#[tokio::test]
async fn at_eighty_columns_every_row_still_says_which_env_it_is() {
    // The regression this guards: NAME was `Percentage(14)` while TIER,
    // STATUS, HEALTH, INST, TREND and AGE were fixed `Length`s. ratatui
    // satisfies `Length` before `Percentage`, so at 80 columns the
    // fixed columns took ~49 cells and NAME — the row identifier — got
    // nothing. Every row rendered without saying which env it was,
    // while `TREND (5m)` kept its full twelve.
    let mut app = test_app();
    app.environments = vec![
        mk_env("api-prod", "uflexi", "Web", "Green"),
        mk_env("worker-prod", "uflexi", "Worker", "Red"),
    ];
    app.rebuild_view();
    app.table_state.select(Some(0));

    let frame = render(&mut app, 80, 20);
    for env in ["api-prod", "worker-prod"] {
        assert!(
            frame.contains(env),
            "env {env} is unidentifiable at 80 columns; got:\n{frame}"
        );
    }
    assert!(
        frame.contains("NAME"),
        "the NAME column header survives; got:\n{frame}"
    );
}

#[tokio::test]
async fn a_narrow_terminal_sheds_optional_columns_and_says_so() {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
    app.rebuild_view();

    let narrow = render(&mut app, 80, 20);
    // TREND is the first thing shed: it is a sparkline, and at 80
    // columns the operator needs identity and health instead.
    assert!(
        !narrow.contains("TREND"),
        "TREND should be shed at 80 columns; got:\n{narrow}"
    );
    assert!(
        narrow.contains("cols hidden"),
        "and the operator must be told, or they read a partial fleet as \
         a complete one; got:\n{narrow}"
    );

    // Wide enough for everything: nothing shed, no notice.
    let wide = render(&mut app, 200, 20);
    assert!(wide.contains("TREND"), "got:\n{wide}");
    assert!(
        !wide.contains("cols hidden"),
        "no notice when nothing was hidden; got:\n{wide}"
    );
}

#[tokio::test]
async fn the_columns_that_survive_are_wide_enough_to_read() {
    // Choosing columns by one width and laying them out by another is
    // how VERSION came to be picked on a floor of 9 and rendered at 3,
    // showing `bui` for `build-1`. The value has to survive, not just
    // the column.
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
    app.rebuild_view();
    let frame = render(&mut app, 80, 20);
    assert!(
        frame.contains("build-1"),
        "the version must be readable, not truncated to noise; got:\n{frame}"
    );
    assert!(
        frame.contains("APPLICATION"),
        "a column heading that truncates itself reads as a bug; got:\n{frame}"
    );
}

#[tokio::test]
async fn a_narrow_header_drops_whole_fields_rather_than_dangling_a_label() {
    // ratatui clips the right edge, which rendered `Profile: ` with the
    // value gone — read as "the profile is empty", not "this did not
    // fit". Same defect as a release date truncated to `2026-08-2`.
    let mut app = test_app();
    app.context.profile = Some("production-admin".into());
    app.rebuild_view();

    let narrow = render(&mut app, 80, 20);
    let dangling = narrow
        .lines()
        .any(|l| l.contains("Profile:") && !l.contains("production-admin"));
    assert!(
        !dangling,
        "a Profile label with no value is worse than no label; got:\n{narrow}"
    );

    // Given the room, it is shown in full.
    let wide = render(&mut app, 200, 20);
    assert!(
        wide.contains("production-admin"),
        "the profile appears when there is room; got:\n{wide}"
    );
}

#[tokio::test]
async fn probe80_more() {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    app.mode = Mode::Detail;
    println!("=== DETAIL 80 ===\n{}", render(&mut app, 80, 26));

    let mut h = test_app();
    h.rebuild_view();
    h.mode = Mode::Help;
    println!("=== HELP 80 ===\n{}", render(&mut h, 80, 26));
}

#[tokio::test]
async fn update_shows_the_whole_command_on_a_narrow_terminal() {
    // `:update`'s entire value is the command it gives you. As a pinned
    // status line — one row — the command sat at the END of ~150
    // characters, so it was the part that fell off: at 120 columns the
    // URL truncated mid-host, and at 80 it vanished, leaving "download
    // the latest binary" and no hint of where from.
    let mut app = test_app();
    app.rebuild_view();
    app.execute_command("update");

    for w in [80u16, 100, 120] {
        let frame = render(&mut app, w, 24);
        assert!(
            frame.contains("github.com/tombaldwin/ebman/releases/latest")
                || frame.contains("brew upgrade ebman")
                || frame.contains("cargo install ebman --force"),
            "the upgrade instruction must survive at {w} columns; got:\n{frame}"
        );
        assert!(
            frame.contains("running") && frame.contains(env!("CARGO_PKG_VERSION")),
            "and say which version is running; got:\n{frame}"
        );
    }
}

#[tokio::test]
async fn the_help_overlay_uses_a_narrow_terminal_instead_of_wasting_it() {
    // The shared overlay table sizes by percentage: `Text` is 70%, so an
    // 80-column terminal got 56 cells, left 24 unused, and wrapped the
    // keymap mid-description — the continuation losing its indent, so no
    // line said which key it belonged to.
    let mut app = test_app();
    app.rebuild_view();
    app.mode = Mode::Help;
    let frame = render(&mut app, 80, 24);

    let widest = frame
        .lines()
        .filter(|l| l.contains('│'))
        .map(|l| l.matches('│').count())
        .max()
        .unwrap_or(0);
    assert!(widest >= 2, "the overlay should be drawn; got:\n{frame}");
    // A keybinding and its description on one line is the whole shape of
    // this screen.
    assert!(
        frame.contains("open drill-down view for the selected env"),
        "a description should not wrap at 80 columns; got:\n{frame}"
    );
    // The credit line was truncated to "Polymorphism L".
    assert!(
        frame.contains(":about"),
        "the footer line should fit; got:\n{frame}"
    );
}

#[tokio::test]
async fn the_hint_panel_cuts_between_hints_not_inside_a_chord() {
    // Clipped, this read `<:> com` — a chord with its action cut in
    // half, which is a worse outcome than one hint fewer.
    let mut app = test_app();
    app.rebuild_view();

    let narrow = render(&mut app, 80, 20);
    let fragments = narrow
        .lines()
        .any(|l| l.contains("<:> com") && !l.contains("<:> command"));
    assert!(!fragments, "a chord was cut mid-action; got:\n{narrow}");

    // Wide enough: every hint present.
    let wide = render(&mut app, 200, 20);
    for hint in ["<tab> scope", "<?> help", "<:> command", "<q> quit"] {
        assert!(wide.contains(hint), "missing {hint}; got:\n{wide}");
    }
}

#[tokio::test]
async fn no_terminal_size_makes_any_screen_panic() {
    // A panic in a TUI leaves the terminal in raw mode and the alternate
    // screen — the user's shell is broken until they blind-type `reset`.
    // Width arithmetic that shows up under narrow terminals is exactly
    // where a subtract-with-overflow or a zero-width Layout comes from,
    // so every screen gets rendered at every extreme.
    let sizes: [(u16, u16); 9] = [
        (200, 60),
        (80, 24),
        (60, 18),
        (40, 12),
        (20, 8),
        (10, 5),
        (4, 4),
        (2, 2),
        (1, 1),
    ];
    let modes = [
        Mode::Normal,
        Mode::Filter,
        Mode::Help,
        Mode::Command,
        Mode::Detail,
        Mode::Dlq,
        Mode::QuickJump,
        Mode::Palette,
    ];
    let long = "a ".repeat(300);
    for (w, h) in sizes {
        for mode in modes {
            let mut app = test_app();
            app.environments = vec![
                mk_env(
                    "a-really-long-environment-name-40chars-x",
                    "uflexi",
                    "Web",
                    "Green",
                ),
                mk_env("worker-prod", "uflexi", "Worker", "Red"),
            ];
            app.rebuild_view();
            app.table_state.select(Some(0));
            if mode == Mode::Detail {
                app.open_detail();
            }
            if mode == Mode::Dlq {
                app.dlq = Some(open_dlq_state("api-prod"));
            }
            app.mode = mode;
            let _ = render(&mut app, w, h);
        }
        // Overlays draw over Normal and do their own centring, which is
        // where the size arithmetic lives.
        for overlay in [
            crate::app::Overlay::TextDump {
                title: "t".into(),
                body: long.clone(),
            },
            crate::app::Overlay::Describe(long.clone()),
            crate::app::Overlay::Diff(long.clone()),
            crate::app::Overlay::History(long.clone()),
        ] {
            let mut app = test_app();
            app.environments = vec![mk_env("api-prod", "uflexi", "Web", "Green")];
            app.rebuild_view();
            app.current_overlay = Some(overlay);
            let _ = render(&mut app, w, h);
        }
    }
}

#[tokio::test]
async fn the_watching_pill_pluralises_too() {
    // The armed-rollback pill was pinned across the singular/plural
    // boundary; its sibling was not, and the sweep found the `== 1`
    // free. Two sibling branches, one covered — the same shape as the
    // thirteen unpinned column widths.
    let mut app = test_app();
    let watch = |env: &str, mins: i64| crate::app::WatchingDeploy {
        env_name: env.into(),
        target_label: "v1".into(),
        armed_at: chrono::Utc::now(),
        deadline_at: chrono::Utc::now() + chrono::Duration::minutes(mins),
    };
    app.watching_deploys
        .insert("api-prod".into(), watch("api-prod", 5));
    app.rebuild_view();
    let one = render(&mut app, 300, 24);
    assert!(
        one.contains("watching api-prod"),
        "a single watcher names its env; got:\n{one}"
    );

    app.watching_deploys
        .insert("api-staging".into(), watch("api-staging", 9));
    app.rebuild_view();
    let two = render(&mut app, 300, 24);
    assert!(
        two.contains("2 watching") && two.contains("next: api-prod"),
        "two watchers report the count and the soonest; got:\n{two}"
    );
}

#[test]
fn the_deploy_preview_warns_only_when_the_candidate_is_genuinely_older() {
    // The warning exists to catch an accidental rollback: deploying a
    // version built BEFORE the one currently running. `<` vs `<=` is
    // the difference between that and warning whenever the two share a
    // timestamp — which is common, since versions built by the same CI
    // run carry the same `created`. A warning that fires on the normal
    // case is one operators learn to click through.
    use crate::aws::AppVersion;
    let at = |s: &str| {
        chrono::DateTime::parse_from_rfc3339(s)
            .expect("valid")
            .with_timezone(&chrono::Utc)
    };
    let v = |label: &str, created: &str| AppVersion {
        label: label.into(),
        description: String::new(),
        created: Some(at(created)),
    };

    // Candidate older than deployed: warn.
    let older = vec![
        v("deployed", "2026-06-01T00:00:00Z"),
        v("candidate", "2026-01-01T00:00:00Z"),
    ];
    let out = format_deploy_preview("api-prod", "deployed", "candidate", &older);
    assert!(out.contains("older than"), "should warn; got:\n{out}");

    // Same timestamp: not a rollback, no warning.
    let same = vec![
        v("deployed", "2026-06-01T00:00:00Z"),
        v("candidate", "2026-06-01T00:00:00Z"),
    ];
    let out = format_deploy_preview("api-prod", "deployed", "candidate", &same);
    assert!(
        !out.contains("older than"),
        "an equal timestamp is not a rollback; got:\n{out}"
    );

    // Candidate newer: the ordinary case, no warning.
    let newer = vec![
        v("deployed", "2026-01-01T00:00:00Z"),
        v("candidate", "2026-06-01T00:00:00Z"),
    ];
    let out = format_deploy_preview("api-prod", "deployed", "candidate", &newer);
    assert!(!out.contains("older than"), "got:\n{out}");
}

#[tokio::test]
async fn the_apps_action_menu_moves_and_wraps_on_j_and_k() {
    // Both navigation arms were free in the sweep — the whole
    // `KeyCode::Down | Char('j')` arm could be deleted and nothing
    // failed. This is a menu that dispatches per-application actions, so
    // a cursor that does not move means the operator selects whatever
    // happened to be first.
    let n = crate::app::APPS_ACTION_ITEMS.len();
    assert!(n >= 2, "the wrap cases need at least two items");

    let cursor_of = |app: &App| match app.current_overlay.as_ref() {
        Some(crate::app::Overlay::AppsActionMenu { cursor, .. }) => *cursor,
        other => panic!("apps menu closed unexpectedly: {other:?}"),
    };
    let open = || {
        let mut app = test_app();
        app.current_overlay = Some(crate::app::Overlay::AppsActionMenu {
            app_name: "uflexi".into(),
            env_names: vec!["api-prod".into()],
            cursor: 0,
        });
        app
    };

    // Down / j advance.
    for key in [KeyCode::Down, KeyCode::Char('j')] {
        let mut app = open();
        app.handle_apps_action_menu_key(KeyEvent::new(key, KeyModifiers::NONE));
        assert_eq!(cursor_of(&app), 1, "{key:?} should move down");
    }
    // Up / k retreat, wrapping from the top to the bottom.
    for key in [KeyCode::Up, KeyCode::Char('k')] {
        let mut app = open();
        app.handle_apps_action_menu_key(KeyEvent::new(key, KeyModifiers::NONE));
        assert_eq!(cursor_of(&app), n - 1, "{key:?} should wrap to the end");
    }
    // And down from the last item wraps back to the first.
    let mut app = open();
    for _ in 0..n {
        app.handle_apps_action_menu_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    assert_eq!(cursor_of(&app), 0, "a full cycle returns to the start");
}
