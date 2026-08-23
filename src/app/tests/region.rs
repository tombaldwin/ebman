//! Rule 2 — per-env work uses the row's region, not the
//! session's. Includes the multi-region fan-out.
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
fn console_url_includes_region_app_env() {
    let url = console_url("us-east-1", "myapp", "myenv");
    let url = url.expect("commercial partition has a console host");
    assert!(url.contains("us-east-1.console.aws.amazon.com"));
    assert!(url.contains("region=us-east-1"));
    assert!(url.contains("applicationName=myapp"));
    assert!(url.contains("environmentName=myenv"));
}

#[test]
fn render_secrets_overlay_empty_with_filter_explains_region_scope() {
    let body = crate::app::render_secrets_overlay(&[], Some("prod-db"));
    assert!(body.contains("No secrets matching 'prod-db'"));
    assert!(body.contains("region-scoped"));
}

#[tokio::test]
async fn batch_action_undo_cancels_whole_fanout() {
    let mut app = test_app();
    app.environments = vec![
        mk_env("e1", "uflexi", "Web", "Green"),
        mk_env("e2", "uflexi", "Web", "Green"),
        mk_env("e3", "uflexi", "Web", "Green"),
    ];
    for name in ["e1", "e2", "e3"] {
        app.multi_selected.insert(name.into());
    }
    app.cmd_batch_action(Action::RestartAppServer);
    assert!(app.pending_dispatch.is_some());
    app.cancel_pending_dispatch();
    assert!(
        app.pending_dispatch.is_none(),
        "cancel should drop the whole batch, not just one env"
    );
    let msg = app.status_message.as_deref().unwrap_or("");
    assert!(
        msg.contains("undone") && msg.contains("3 env(s)"),
        "status should call out the 3-env batch; got: {msg:?}"
    );
}

#[tokio::test]
async fn rollout_advances_to_the_next_eligible_region() {
    // The dispatch-advance branch used to re-derive "not done" with
    // `next_eligible.expect("checked by done")`. Region 1 failed
    // pre-flight, so the advance must skip it and land on region 2.
    use crate::mode_action::{ActionFlow, RolloutFlow, RolloutRegion, RolloutState};
    let region = |name: &str, found: bool| RolloutRegion {
        region: name.into(),
        current_version: Some("v1".into()),
        env_found: Some(found),
        preflight_error: None,
        outcome: None,
    };
    let mut app = test_app();
    app.action_flow = Some(ActionFlow::Rollout(RolloutFlow {
        rollout_id: "rollout-test".into(),
        env_name: "api-prod".into(),
        version_label: "v2".into(),
        regions: vec![
            region("eu-west-1", true),
            region("eu-west-2", false), // failed pre-flight — must be skipped
            region("us-east-1", true),
        ],
        state: RolloutState::Dispatching { next_index: 0 },
        wait_for_green_secs: None,
    }));

    app.handle_msg(AppMsg::RolloutDispatched {
        gen: app.generation,
        region: "eu-west-1".into(),
        result: Ok(()),
    });

    let Some(ActionFlow::Rollout(flow)) = app.action_flow.as_ref() else {
        panic!("rollout flow should still be active");
    };
    assert_eq!(
        flow.state,
        RolloutState::Dispatching { next_index: 2 },
        "must skip the region that failed pre-flight"
    );
    assert!(flow.regions[0].outcome.is_some());
    assert!(flow.regions[1].outcome.is_none(), "skipped, not dispatched");
}

#[tokio::test]
async fn rollout_halts_on_a_failed_region() {
    // The complement branch: an Err outcome ends the rollout even
    // though an eligible region remains.
    use crate::mode_action::{ActionFlow, RolloutFlow, RolloutRegion, RolloutState};
    let region = |name: &str| RolloutRegion {
        region: name.into(),
        current_version: Some("v1".into()),
        env_found: Some(true),
        preflight_error: None,
        outcome: None,
    };
    let mut app = test_app();
    app.action_flow = Some(ActionFlow::Rollout(RolloutFlow {
        rollout_id: "rollout-test".into(),
        env_name: "api-prod".into(),
        version_label: "v2".into(),
        regions: vec![region("eu-west-1"), region("us-east-1")],
        state: RolloutState::Dispatching { next_index: 0 },
        wait_for_green_secs: None,
    }));

    app.handle_msg(AppMsg::RolloutDispatched {
        gen: app.generation,
        region: "eu-west-1".into(),
        result: Err("UpdateEnvironment refused".into()),
    });

    let Some(ActionFlow::Rollout(flow)) = app.action_flow.as_ref() else {
        panic!("rollout flow should still be active");
    };
    assert_eq!(flow.state, RolloutState::Done, "halt on first failure");
    assert!(flow.regions[1].outcome.is_none(), "never dispatched");
}

#[test]
fn parse_access_denied_handles_every_partition() {
    for partition in ["aws", "aws-us-gov", "aws-cn", "aws-iso", "aws-iso-b"] {
        let msg = format!(
            "User: arn:{partition}:sts::1:assumed-role/R/S is not authorized to perform: s3:GetObject"
        );
        let (principal, _) = crate::app::parse_access_denied(&msg).expect("parsed");
        assert_eq!(
            principal,
            format!("arn:{partition}:iam::1:role/R"),
            "the rebuilt role ARN must stay in its own partition"
        );
    }
}

#[test]
fn console_url_follows_the_partition() {
    let gov = console_url("us-gov-west-1", "myapp", "myenv").expect("govcloud has a console");
    assert!(
        gov.contains("us-gov-west-1.console.amazonaws-us-gov.com"),
        "got {gov}"
    );
    let cn = console_url("cn-north-1", "myapp", "myenv").expect("china has a console");
    assert!(cn.contains("cn-north-1.console.amazonaws.cn"), "got {cn}");
    // No guessed hostname for the ISO partitions.
    assert!(console_url("us-iso-east-1", "myapp", "myenv").is_none());
}

#[tokio::test]
async fn explain_accepts_an_arn_from_any_partition() {
    // The guard matched the literal `arn:aws:`, so `:explain` refused
    // its own documented argument form for every operator outside the
    // commercial partition — one level above the rewrite that was
    // fixed for exactly the same reason.
    for arn in [
        "arn:aws:iam::123456789012:role/EbAdmin",
        "arn:aws-us-gov:iam::123456789012:role/EbAdmin",
        "arn:aws-cn:iam::123456789012:role/EbAdmin",
        "arn:aws-iso-b:iam::123456789012:role/EbAdmin",
    ] {
        let mut app = test_app();
        app.execute_command(&format!("explain {arn} elasticbeanstalk:UpdateEnvironment"));
        assert!(
            !app.error_message
                .as_deref()
                .unwrap_or_default()
                .starts_with("usage:"),
            "{arn} was rejected as malformed: {:?}",
            app.error_message
        );
    }
}

#[tokio::test]
async fn row_region_is_used_for_links_and_cli_snippets() {
    // Under a fan-out the selected row can be in a different region
    // from `context.region`. The home region opens a console dashboard
    // where the environment doesn't exist, and produces a CLI snippet
    // that returns an empty array — or the WRONG environment when a
    // same-named one exists at home.
    //
    // The previous version of this test re-implemented the fixed
    // expression inline and never called a production function, so
    // reverting the fix left it green.
    let mut app = test_app();
    let mut env = mk_env("api-prod", "uflexi", "Web", "Green");
    env.region = Some("eu-west-2".into());
    app.environments = vec![env.clone()];
    app.rebuild_view();
    app.table_state.select(Some(0));
    assert_eq!(app.context.region, "us-east-1", "home region differs");

    // The accessor all three sites share.
    assert_eq!(app.region_for(&env), "eu-west-2");
    // A row with no region of its own falls back to the home region.
    let mut homeless = env.clone();
    homeless.region = None;
    assert_eq!(app.region_for(&homeless), "us-east-1");

    // And the snippet actually copied uses it.
    app.yank_cli();
    let cmd = app.last_yanked_cli.as_deref().unwrap_or_default();
    assert!(
        cmd.contains("--region eu-west-2"),
        "the copied CLI must name the row's region: {cmd}"
    );
}

#[tokio::test]
async fn a_region_that_fails_the_fan_out_is_reported_not_dropped() {
    // The fan-out only reported an error when EVERY region failed, so
    // one region throttling or exceeding its page budget removed all
    // of its environments from the table with nothing on screen. That
    // was survivable while a truncated walk returned a short list;
    // once `list_environments` started refusing partial results it
    // meant a whole region could vanish silently.
    let mut app = test_app();
    app.apply_refresh(
        app.fanout_epoch,
        Ok(vec![mk_env("api-prod", "uflexi", "Web", "Green")]),
        vec!["eu-west-2: DescribeEnvironments failed".to_string()],
    );
    let err = app.error_message.as_deref().unwrap_or_default();
    assert!(
        err.contains("eu-west-2") && err.contains("NOT shown"),
        "a partially-failed fan-out must say which region is missing: {err:?}"
    );
    assert_eq!(
        app.environments.len(),
        1,
        "the rows that arrived still render"
    );
}

#[tokio::test]
async fn a_write_dispatches_to_the_rows_region() {
    // The worst case in this class: a restart / terminate / deploy on a
    // fan-out row went to the home region, where it either failed as
    // "environment not found" or — with a same-named env at home, which
    // is what a fleet with per-region copies looks like — dispatched a
    // destructive action against the wrong environment entirely.
    let mut app = test_app();
    let mut env = mk_env("api-prod", "uflexi", "Web", "Green");
    env.region = Some("eu-west-2".into());
    app.environments = vec![env];
    app.rebuild_view();
    app.table_state.select(Some(0));

    assert_eq!(
        app.client_for_region(&app.region_for_name("api-prod"))
            .region_for_tests(),
        "eu-west-2",
        "the write client follows the row"
    );
    // A row with no region of its own stays on the home client, which
    // may be an AssumeRole session `cached_client` can't rebuild.
    let mut homeless = mk_env("home-env", "uflexi", "Web", "Green");
    homeless.region = None;
    app.environments.push(homeless);
    app.rebuild_view();
    assert_eq!(
        app.client_for_region(&app.region_for_name("home-env"))
            .region_for_tests(),
        "us-east-1"
    );
}

#[tokio::test]
async fn demo_mode_never_resolves_a_remote_region() {
    // The demo fleet's regions are fictional and its client is a stub;
    // resolving one would reach real AWS for a region the fixture
    // invented, during a screencast.
    let mut app = test_app();
    app.demo_mode = true;
    let mut env = mk_env("api-prod", "uflexi", "Web", "Green");
    env.region = Some("ap-southeast-4".into());
    app.environments = vec![env];
    app.rebuild_view();
    assert!(
        app.client_for_region("ap-southeast-4").is_home_for_tests(),
        "demo mode stays on the stub"
    );
}

#[tokio::test]
async fn a_cross_region_row_under_an_assumed_role_re_assumes() {
    // `assume_role` puts the friendly ACCOUNT name in
    // `context.profile` as the header breadcrumb. So resolving a
    // cross-region row through `cached_client(context.profile, …)`
    // went looking for an AWS profile called `prod` that was never a
    // profile — the fix for wrong-region data would have traded it for
    // a confusing "profile not found". Re-assume into the same account
    // pointed at the other region, exactly as `:org-health` does.
    let mut app = test_app();
    app.cfg.accounts.insert(
        "prod".into(),
        crate::config::AccountSpec {
            role_arn: "arn:aws:iam::1:role/EbmanReadOnly".into(),
            region: Some("us-east-1".into()),
            ..Default::default()
        },
    );
    app.context.profile = Some("prod".into());
    let mut env = mk_env("api-prod", "uflexi", "Web", "Green");
    env.region = Some("eu-west-2".into());
    app.environments = vec![env];
    app.rebuild_view();

    let client = app.client_for_region("eu-west-2");
    assert_eq!(
        client.account_for_tests().as_deref(),
        Some("prod"),
        "it must re-assume, not look for a profile named after the account"
    );
    assert_eq!(
        client.region_for_tests(),
        "eu-west-2",
        "and point the assumed session at the row's region, not the spec's"
    );

    // The home region keeps the LIVE session rather than re-assuming —
    // that client already holds valid credentials.
    assert!(app.client_for_region("us-east-1").is_home_for_tests());
}

#[tokio::test]
async fn a_write_audits_the_region_it_actually_went_to() {
    // The audit log is the record of what was done to production. A
    // dispatch that went to eu-west-2 while the journal said
    // us-east-1 is worse than no line at all — it's a confident wrong
    // answer during an incident review.
    let mut app = test_app();
    let mut env = mk_env("api-prod", "uflexi", "Web", "Green");
    env.region = Some("eu-west-2".into());
    app.environments = vec![env];
    app.rebuild_view();
    app.table_state.select(Some(0));

    assert_eq!(
        app.region_for_name("api-prod"),
        "eu-west-2",
        "the audit region comes from this lookup at every dispatch site"
    );
    // The home region is still what an env we hold no row for gets.
    assert_eq!(app.region_for_name("ghost"), "us-east-1");
}

#[tokio::test]
async fn a_dispatch_and_its_completion_agree_on_the_region() {
    // The two lines are a pair — `ebman audit` correlates them by
    // action + target. If the dispatch names the row's region and the
    // completion names the home one, a grep across the pair reports an
    // action that started in eu-west-2 and finished in us-east-1.
    let mut app = test_app();
    let mut env = mk_env("api-prod", "uflexi", "Web", "Green");
    env.region = Some("eu-west-2".into());
    app.environments = vec![env];
    app.rebuild_view();
    app.table_state.select(Some(0));

    let path = crate::util::cache_dir().join("audit.log");
    let before = std::fs::read_to_string(&path).unwrap_or_default();

    app.handle_msg(AppMsg::ActionResult {
        gen: app.generation,
        action: crate::app::Action::RestartAppServer,
        env_name: "api-prod".into(),
        result: Ok(()),
    });

    let after = std::fs::read_to_string(&path).unwrap_or_default();
    let line = after
        .strip_prefix(&before)
        .unwrap_or(&after)
        .lines()
        .find(|l| l.contains("api-prod"))
        .expect("a completion line was written")
        .to_string();
    assert!(
        line.contains("region=eu-west-2"),
        "the completion must name where the work went: {line}"
    );
}

#[tokio::test]
async fn a_cross_region_role_client_comes_from_the_cache() {
    // The pre-tag review added the role cache and a test that it gets
    // CLEARED — which passed while the code path that was supposed to
    // read it still called `assume_role` directly, because the edit
    // routing it through silently failed. A cache nothing reads is not
    // a fix, and "the clear works" could never have caught that.
    //
    // Per-env work under `:account` resolves once per call, and
    // `spawn_env_instance_counts` builds a client per row on every
    // 15-second tick: a fresh AssumeRole each time is an STS storm for
    // a session that stays valid for another hour.
    let _guard = crate::aws::CACHE_TEST_LOCK.lock().await;
    crate::aws::clear_client_cache();

    let mut app = test_app();
    app.cfg.accounts.insert(
        "prod".into(),
        crate::config::AccountSpec {
            role_arn: "arn:aws:iam::1:role/R".into(),
            region: Some("us-east-1".into()),
            ..Default::default()
        },
    );
    app.context.profile = Some("prod".into());

    // Seed the cache for the key the accessor will build. Assuming for
    // real needs live STS, so this proves the READ path — which is the
    // half that was broken.
    let seeded = std::sync::Arc::new(crate::aws::AwsClient::stub());
    crate::aws::seed_role_cache_for_tests("prod", "eu-west-2", seeded.clone());

    let client = app.client_for_region("eu-west-2");
    assert_eq!(client.account_for_tests().as_deref(), Some("prod"));
    let resolved = client.resolve().await.expect("cache hit, no STS call");
    assert!(
        std::sync::Arc::ptr_eq(&resolved, &seeded),
        "resolve must come from the role cache, not a fresh AssumeRole"
    );

    crate::aws::clear_client_cache();
}

#[tokio::test]
async fn a_detail_env_that_left_the_table_keeps_its_region() {
    // `region_for_name` looks in `self.environments`, but Detail's
    // snapshot is taken at open time and is NOT torn down when a
    // refresh drops the row — a terminated env, or a region whose
    // fetch failed under a fan-out. The action menu targets Detail's
    // env, so without the snapshot fallback a restart / terminate
    // dispatched there fell back to the HOME region: the original
    // wrong-region bug, in a narrow window, and silently.
    let mut app = test_app();
    let mut env = mk_env("api-prod", "uflexi", "Web", "Green");
    env.region = Some("eu-west-2".into());
    app.environments = vec![env];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();
    assert!(app.detail.is_some(), "detail open on the fan-out row");

    // The refresh that drops it — eu-west-2 failed this tick.
    app.environments.clear();
    app.view.invalidate();
    app.rebuild_view();

    assert_eq!(
        app.region_for_name("api-prod"),
        "eu-west-2",
        "Detail's snapshot still knows where this env lives"
    );
    assert_eq!(app.detail_client().region_for_tests(), "eu-west-2");
    // A name neither the table nor Detail knows still falls back.
    assert_eq!(app.region_for_name("ghost"), "us-east-1");
}

#[tokio::test]
async fn current_env_client_and_client_for_env_are_not_interchangeable() {
    // `current_env_client` is Detail-first, matching how `:alarms` and
    // `:alarm-history` pick their env. Most commands instead operate on
    // `selected_env()`. The two agree almost always — opening Detail
    // uses the selection — but a refresh that reorders or filters the
    // table moves the selection while Detail keeps its snapshot, and
    // then they name different environments in different regions.
    //
    // `:alarm-create` / `:alarm-delete` were resolving through the
    // Detail-first accessor while operating on the selection, so the
    // alarm would have been written to one region and audited as
    // another. This pins the distinction so the accessors don't get
    // swapped back for looking similar.
    let mut app = test_app();
    let mut a = mk_env("api-prod", "uflexi", "Web", "Green");
    a.region = Some("eu-west-2".into());
    let mut b = mk_env("api-staging", "uflexi", "Web", "Green");
    b.region = Some("ap-south-1".into());
    app.environments = vec![a, b];
    app.rebuild_view();

    // Detail on the first row.
    app.table_state.select(Some(0));
    app.open_detail();
    // Selection moves to the second — what a re-sorted refresh does.
    app.table_state.select(Some(1));

    assert_eq!(
        app.current_env_client().region_for_tests(),
        "eu-west-2",
        "Detail-first: the env on screen"
    );
    let selected = app.selected_env().expect("row 1").name.clone();
    assert_eq!(selected, "api-staging");
    assert_eq!(
        app.client_for_env(&selected).region_for_tests(),
        "ap-south-1",
        "selection-based: the env the command operates on"
    );
    // And the audit region for a selection-based command follows the
    // selection too, so the client and the journal agree.
    assert_eq!(app.region_for_name(&selected), "ap-south-1");
}

#[tokio::test]
async fn a_write_whose_row_left_the_table_still_goes_to_its_region() {
    // The confirm modal carries a target NAME, and there is an undo
    // window between the operator confirming and `tick_pending_dispatch`
    // firing. A 15-second refresh landing in that window — a terminated
    // env, or a region whose fetch failed under a fan-out — dropped the
    // row, and the dispatch fell back to the home region. Silently.
    //
    // The Detail-snapshot fallback covers this only when Detail happens
    // to be open on that env; an action started from the table has no
    // snapshot at all.
    let mut app = test_app();
    let mut env = mk_env("api-prod", "uflexi", "Web", "Green");
    env.region = Some("eu-west-2".into());

    // The refresh that put it on screen is what remembers the region.
    app.handle_msg(AppMsg::Refresh {
        gen: app.generation,
        fanout: app.fanout_epoch,
        result: Ok(vec![env]),
        partial_errors: Vec::new(),
    });
    assert_eq!(app.region_for_name("api-prod"), "eu-west-2");
    assert!(app.detail.is_none(), "no Detail snapshot to lean on");

    // The next tick drops it — eu-west-2 failed, or the env terminated.
    app.handle_msg(AppMsg::Refresh {
        gen: app.generation,
        fanout: app.fanout_epoch,
        result: Ok(vec![]),
        partial_errors: vec!["region eu-west-2: throttled".into()],
    });
    assert!(app.environments.is_empty(), "the row is gone");

    assert_eq!(
        app.region_for_name("api-prod"),
        "eu-west-2",
        "a write in its undo window must not silently retarget the home region"
    );
    assert_eq!(
        app.client_for_env("api-prod").region_for_tests(),
        "eu-west-2"
    );
    // A name we have never seen still falls back.
    assert_eq!(app.region_for_name("ghost"), "us-east-1");
}

#[tokio::test]
async fn remembered_regions_do_not_survive_a_context_switch() {
    // A same-named env in another account or partition is a different
    // environment. Carrying the old answer across would aim a write at
    // a region the new context may not even have.
    let mut app = test_app();
    let mut env = mk_env("api-prod", "uflexi", "Web", "Green");
    env.region = Some("eu-west-2".into());
    app.handle_msg(AppMsg::Refresh {
        gen: app.generation,
        fanout: app.fanout_epoch,
        result: Ok(vec![env]),
        partial_errors: Vec::new(),
    });
    assert_eq!(app.region_for_name("api-prod"), "eu-west-2");

    app.handle_msg(AppMsg::Rebuild {
        epoch: app.rebuild_epoch,
        result: Ok(Box::new(crate::aws::AwsClient::stub())),
    });
    assert_eq!(
        app.region_for_name("api-prod"),
        app.context.region,
        "the new context's home region, not the old context's answer"
    );
}

#[tokio::test]
async fn the_breadcrumb_names_the_region_of_the_env_it_names() {
    // The crumb reads `REGION / app / env`. It used to render
    // `context.region` unconditionally, which was accidentally
    // truthful while Detail showed home-region data — and became a lie
    // the moment Detail started fetching from the row's region.
    // `us-east-1 / uflexi / api-prod` for an env in eu-west-2 is the
    // confusion this release exists to remove.
    let mut app = test_app();
    let mut env = mk_env("api-prod", "uflexi", "Web", "Green");
    env.region = Some("eu-west-2".into());
    app.environments = vec![env];
    app.rebuild_view();
    app.table_state.select(Some(0));
    assert_eq!(app.context.region, "us-east-1", "home region differs");

    let out = render(&mut app, 160, 40);
    assert!(
        out.contains("eu-west-2"),
        "the crumb must name the selected env's region:\n{out}"
    );

    // Detail replaces the screen with its own header and draws no
    // crumb, so there is no wrong region to show there — but no region
    // at all either. Recorded in BACKLOG as a gap rather than fixed
    // here: adding one is a UI addition, not a stale workaround.
    app.open_detail();
    let out = render(&mut app, 160, 40);
    assert!(
        !out.contains("us-east-1"),
        "Detail must not show the SESSION's region beside another region's env:\n{out}"
    );

    // Nothing selected: the session's region is the right answer.
    let mut empty = test_app();
    let out = render(&mut empty, 160, 40);
    assert!(
        out.contains("us-east-1"),
        "session region with no env:\n{out}"
    );
}

// ---------------------------------------------------------------
// `:region all` / `:region off` — fan-out epoch
// ---------------------------------------------------------------

#[tokio::test]
async fn fanout_mode_change_bumps_the_fanout_epoch() {
    let mut app = test_app();
    assert_eq!(app.fanout_epoch, 0);

    app.execute_command("region all");
    assert_eq!(app.fanout_epoch, 1, ":region all changes the region set");

    app.execute_command("region off");
    assert_eq!(app.fanout_epoch, 2, ":region off changes it back");

    // A plain `:region <name>` goes through the picker, which rebuilds
    // the client and bumps `generation` instead — that already
    // supersedes the listing, so this axis must not move.
    let before = app.fanout_epoch;
    app.execute_command("sort name");
    assert_eq!(
        app.fanout_epoch, before,
        "unrelated commands leave it alone"
    );
}

#[tokio::test]
async fn a_listing_from_a_superseded_fanout_mode_is_dropped_and_refetched() {
    // The bug: `spawn_refresh` returns early while a listing is already
    // in flight, so `cmd_region`'s own call was a no-op. The old mode's
    // listing then arrived, matched on `generation`, and was applied —
    // putting single-region rows on screen under a `:region all`
    // header until the next 15s tick healed it.
    let mut app = test_app();
    app.handle_msg(AppMsg::Refresh {
        gen: app.generation,
        fanout: app.fanout_epoch,
        result: Ok(vec![mk_env("home-only", "uflexi", "Web", "Green")]),
        partial_errors: Vec::new(),
    });
    assert_eq!(app.environments.len(), 1);

    app.execute_command("region all");
    let stale = 0;
    assert_ne!(stale, app.fanout_epoch);

    // The single-region listing launched before the switch lands.
    app.handle_msg(AppMsg::Refresh {
        gen: app.generation,
        fanout: stale,
        result: Ok(vec![
            mk_env("wrong-a", "uflexi", "Web", "Green"),
            mk_env("wrong-b", "uflexi", "Web", "Green"),
        ]),
        partial_errors: Vec::new(),
    });
    assert_eq!(
        app.environments.len(),
        1,
        "a listing from the superseded mode must not replace the table"
    );
    assert_eq!(app.environments[0].name, "home-only");

    // ...and dropping it must not wedge refresh. `spawn_refresh` skips
    // while Loading, so the drop has to clear the flag AND re-spawn or
    // nothing fetches the new mode's rows.
    assert!(
        matches!(app.load_state, crate::app::LoadState::Loading),
        "the drop must launch a replacement listing, not just discard"
    );

    // A listing stamped with the current epoch applies normally.
    app.handle_msg(AppMsg::Refresh {
        gen: app.generation,
        fanout: app.fanout_epoch,
        result: Ok(vec![
            mk_env("eu-1", "uflexi", "Web", "Green"),
            mk_env("us-1", "uflexi", "Web", "Green"),
        ]),
        partial_errors: Vec::new(),
    });
    assert_eq!(app.environments.len(), 2);
}

#[tokio::test]
async fn fanout_change_does_not_bump_generation() {
    // Why this is a separate axis rather than a `generation` bump.
    // `:region all` changes which regions the FLEET LISTING covers; it
    // does not change account or credentials. `generation` is the
    // context-switch axis, and bumping it here would drop every
    // in-flight per-env result that is still perfectly valid —
    // including `ActionResult` for a dispatched write, whose
    // `complete_pending` would never run, leaving the header's `⏳ N`
    // chip stuck forever. `apply_rebuild` clears `pending_actions` for
    // exactly that reason; there is nothing to clear here.
    let mut app = test_app();
    let gen_before = app.generation;

    app.execute_command("region all");
    app.execute_command("region off");

    assert_eq!(
        app.generation, gen_before,
        "a fan-out change is not a context switch"
    );
    assert_eq!(app.fanout_epoch, 2, "it moved the narrower axis instead");
}

#[tokio::test]
async fn detail_header_names_the_rows_region_not_the_sessions() {
    // Detail replaces the whole screen and draws no breadcrumb, so
    // under a `:region all` fan-out nothing on screen said which region
    // the instances, metrics and log groups had been fetched from.
    // Since 0.30 made per-env work follow the ROW's region, "probably
    // the session's" is not a safe assumption either.
    let mut app = test_app();
    let mut env = mk_env("api-prod", "uflexi", "Web", "Green");
    env.region = Some("eu-west-2".into());
    app.environments = vec![env];
    app.view.invalidate();
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.open_detail();

    assert_eq!(
        app.context.region, "us-east-1",
        "precondition: the session is somewhere else entirely"
    );

    let out = render(&mut app, 160, 40);
    assert!(
        out.contains("Region: eu-west-2"),
        "Detail must name the row's region.\n{out}"
    );
    assert!(
        !out.contains("Region: us-east-1"),
        "naming the session's region here is the bug, not the fix.\n{out}"
    );

    // The label is only worth anything if it agrees with where the
    // pane's data actually comes from. Same expression, so it cannot
    // drift: `detail_client` resolves through `region_for` too.
    assert_eq!(
        app.detail_client().region_for_tests(),
        "eu-west-2",
        "the header would be describing a different region than the fetch"
    );
}
