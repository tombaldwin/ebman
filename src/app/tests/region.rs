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
        mk_env("e1", "poly", "Web", "Green"),
        mk_env("e2", "poly", "Web", "Green"),
        mk_env("e3", "poly", "Web", "Green"),
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
    let mut env = mk_env("api-prod", "poly", "Web", "Green");
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
        Ok(vec![mk_env("api-prod", "poly", "Web", "Green")]),
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
    let mut env = mk_env("api-prod", "poly", "Web", "Green");
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
    let mut homeless = mk_env("home-env", "poly", "Web", "Green");
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
    let mut env = mk_env("api-prod", "poly", "Web", "Green");
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
    let mut env = mk_env("api-prod", "poly", "Web", "Green");
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
    let mut env = mk_env("api-prod", "poly", "Web", "Green");
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
    // A name no other test uses. `cache_dir()` is ONE temp directory per
    // test process, so every test that writes an audit line appends to
    // this same file concurrently — and this test used `api-prod`, which
    // a dozen others use too. It searched the delta for the first line
    // mentioning that name and could pick up a neighbour's, asserting
    // against another test's region. Observed failing once in ~16 full
    // -suite runs while reviewing this session's work.
    let env_name = "region-pair-probe-env";

    let mut app = test_app();
    let mut env = mk_env(env_name, "poly", "Web", "Green");
    env.region = Some("eu-west-2".into());
    app.environments = vec![env];
    app.rebuild_view();
    app.table_state.select(Some(0));

    let path = crate::util::cache_dir().join("audit.log");
    let before = std::fs::read_to_string(&path).unwrap_or_default();

    app.handle_msg(AppMsg::ActionResult {
        gen: app.generation,
        action: crate::app::Action::RestartAppServer,
        env_name: env_name.into(),
        result: Ok(()),
    });

    let after = std::fs::read_to_string(&path).unwrap_or_default();
    // Strict: the log is append-only, so `before` must still be a prefix.
    // Falling back to the whole file on failure is how a concurrent write
    // turned into a search over unrelated history.
    let delta = after
        .strip_prefix(&before)
        .expect("the audit log is append-only; a non-prefix means it was rotated mid-test");
    let matching: Vec<&str> = delta.lines().filter(|l| l.contains(env_name)).collect();
    assert_eq!(
        matching.len(),
        1,
        "exactly one completion line for {env_name} should have been \
         written by this test: {matching:?}"
    );
    let line = matching[0].to_string();
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
    let mut env = mk_env("api-prod", "poly", "Web", "Green");
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
    let mut a = mk_env("api-prod", "poly", "Web", "Green");
    a.region = Some("eu-west-2".into());
    let mut b = mk_env("api-staging", "poly", "Web", "Green");
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
    let mut env = mk_env("api-prod", "poly", "Web", "Green");
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
    // Dispatching a current-epoch rebuild clears the process-global
    // client cache; see `cache_lock_guard`.
    let _cache_guard = crate::aws::CACHE_TEST_LOCK.lock().await;
    // A same-named env in another account or partition is a different
    // environment. Carrying the old answer across would aim a write at
    // a region the new context may not even have.
    let mut app = test_app();
    let mut env = mk_env("api-prod", "poly", "Web", "Green");
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
    // `us-east-1 / poly / api-prod` for an env in eu-west-2 is the
    // confusion this release exists to remove.
    let mut app = test_app();
    let mut env = mk_env("api-prod", "poly", "Web", "Green");
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
        result: Ok(vec![mk_env("home-only", "poly", "Web", "Green")]),
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
            mk_env("wrong-a", "poly", "Web", "Green"),
            mk_env("wrong-b", "poly", "Web", "Green"),
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
            mk_env("eu-1", "poly", "Web", "Green"),
            mk_env("us-1", "poly", "Web", "Green"),
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
    let mut env = mk_env("api-prod", "poly", "Web", "Green");
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

// ── mutation-sweep triage, 2026-08-26 ────────────────────────────────
//
// The TUI rollout-approval arms in `handle_action_key` were the last
// cluster left in `action_flow.rs`. They are the twin of the CLI's
// `--yes` gate — the difference being that this one is reachable from a
// test, so it gets one.

fn awaiting_confirm(regions: Vec<(&str, Option<bool>)>) -> crate::mode_action::RolloutFlow {
    crate::mode_action::RolloutFlow {
        rollout_id: "rollout-20260826T120000Z".into(),
        env_name: "api-prod".into(),
        version_label: "build-900".into(),
        regions: regions
            .into_iter()
            .map(|(r, found)| crate::mode_action::RolloutRegion {
                region: r.into(),
                current_version: Some("build-899".into()),
                env_found: found,
                preflight_error: if found == Some(false) {
                    Some("env not found".into())
                } else {
                    None
                },
                outcome: None,
            })
            .collect(),
        state: crate::mode_action::RolloutState::AwaitingConfirm,
        wait_for_green_secs: None,
    }
}

fn armed_rollout(regions: Vec<(&str, Option<bool>)>) -> App {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "shop", "WebServer", "Green")];
    app.rebuild_view();
    app.table_state.select(Some(0));
    app.mode = crate::app::Mode::Action;
    app.action_flow = Some(crate::mode_action::ActionFlow::Rollout(awaiting_confirm(
        regions,
    )));
    app
}

/// A rollout with nothing to dispatch to must refuse, not proceed.
///
/// `let any_ok = regions.iter().any(|r| r.env_found == Some(true))`
/// followed by `if !any_ok` carried survivors on both the comparison and
/// the negation. Inverted, a plan where *every* region failed pre-flight
/// is the one that dispatches.
#[tokio::test]
async fn a_rollout_with_no_passing_regions_refuses_to_dispatch() {
    // Every region failed pre-flight.
    let mut app = armed_rollout(vec![("us-east-1", Some(false)), ("eu-west-1", Some(false))]);
    press(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);
    assert!(
        app.error_message
            .as_deref()
            .unwrap_or("")
            .contains("no regions passed pre-flight"),
        "a plan with nothing to dispatch to must say so: {:?}",
        app.error_message
    );
    assert!(
        matches!(
            app.action_flow,
            Some(crate::mode_action::ActionFlow::Rollout(_))
        ),
        "and must stay open so the operator can fix it or abort"
    );

    // One region passing IS enough — without this half, "always refuse"
    // would pass the case above.
    let mut app = armed_rollout(vec![("us-east-1", Some(false)), ("eu-west-1", Some(true))]);
    press(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);
    assert!(
        !app.error_message
            .as_deref()
            .unwrap_or("")
            .contains("no regions passed pre-flight"),
        "one passing region is enough to dispatch: {:?}",
        app.error_message
    );
}

/// Every key that declines a rollout at the confirm step. Each arm was
/// separately deletable, so an ignored keypress leaves a multi-region
/// dispatch sitting armed on screen.
#[tokio::test]
async fn a_rollout_confirm_can_be_declined_by_every_documented_key() {
    for decline in [KeyCode::Char('n'), KeyCode::Esc, KeyCode::Char('q')] {
        let mut app = armed_rollout(vec![("us-east-1", Some(true))]);
        press(&mut app, decline, KeyModifiers::NONE);
        assert!(
            app.action_flow.is_none(),
            "{decline:?} must abort the rollout before anything dispatches"
        );
    }

    // And `y` on a passing plan does NOT abort — it advances.
    let mut app = armed_rollout(vec![("us-east-1", Some(true))]);
    press(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);
    assert!(
        app.action_flow.is_some(),
        "`y` starts the dispatch rather than closing the flow"
    );
}

#[tokio::test]
async fn what_discovery_found_does_not_survive_a_context_switch() {
    let _cache_guard = crate::aws::CACHE_TEST_LOCK.lock().await;
    // Another account's `api-prod` has its own alarms, cache and logs:
    // showing the first account's findings on it would be wrong.
    let mut app = test_app();
    let key = app.discovery_key("api-prod");
    app.caches
        .app_tab_discovery
        .discovered
        .insert(key.clone(), Default::default());
    let _ = app
        .caches
        .app_tab_discovery
        .discovering
        .entry(key.clone())
        .or_default()
        .begin();
    app.handle_msg(AppMsg::Rebuild {
        epoch: app.rebuild_epoch,
        result: Ok(Box::new(crate::aws::AwsClient::stub())),
    });
    assert!(app.caches.app_tab_discovery.discovered.is_empty());
    assert!(
        app.caches.app_tab_discovery.discovering.is_empty(),
        "nor the in-flight marker, or it never reruns"
    );
}

#[tokio::test]
async fn discovery_for_a_same_named_env_in_another_region_is_not_this_ones() {
    // Under a fan-out `api-prod` in us-east-1 and `api-prod` in
    // eu-west-2 are two envs with their own databases, caches and logs.
    let mut app = test_app();
    let mut home = mk_env("api-prod", "home-app", "Web", "Green");
    home.region = Some("us-east-1".into());
    let mut away = mk_env("api-prod", "away-app", "Web", "Green");
    away.region = Some("eu-west-2".into());
    app.environments = vec![home, away];
    app.rebuild_view();
    let away_row = (0..app.display_rows().len())
        .find(|&row| {
            app.table_state.select(Some(row));
            app.selected_env()
                .is_some_and(|e| e.region.as_deref() == Some("eu-west-2"))
        })
        .expect("the eu-west-2 row");
    app.table_state.select(Some(away_row));
    app.open_detail();
    assert_eq!(
        app.discovery_key("api-prod"),
        crate::app::EnvKey::new("eu-west-2", "api-prod"),
        "Detail's env, by its own region, not the first row of that name"
    );
    let key = app.discovery_key("api-prod");
    assert_eq!(
        app.discovery_client(&key).region_for_tests(),
        "eu-west-2",
        "the lookups run where the env is"
    );
    // And the spawn itself uses that key, not a name or the home region.
    app.spawn_app_discovery("api-prod".into());
    let running: Vec<_> = app
        .caches
        .app_tab_discovery
        .discovering
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        running,
        std::slice::from_ref(&key),
        "the run is tracked under eu-west-2's key"
    );
    app.caches.app_tab_discovery.discovering.clear();

    let mut found = crate::app_discover::Discovered::default();
    found.discovery.app_log = Some("var/log/app.log".into());
    app.caches
        .app_tab_discovery
        .discovered
        .insert(crate::app::EnvKey::new("us-east-1", "api-prod"), found);
    assert!(
        app.discovered_for("api-prod").is_none(),
        "us-east-1's findings are not shown on eu-west-2's env"
    );

    // Nor does its answer fill the editor open on eu-west-2's env.
    app.demo_mode = true;
    app.detail.as_mut().unwrap().tab_idx = 1;
    assert_eq!(app.detail.as_ref().unwrap().tab(), DetailTab::App);
    press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
    let loading = || Some(crate::form::FormState::Loading);
    assert_eq!(app.form.as_ref().map(|f| f.state.clone()), loading());
    for (region, still_loading) in [("us-east-1", true), ("eu-west-2", false)] {
        app.handle_msg(AppMsg::AppTabDiscovered {
            gen: app.generation,
            region: region.into(),
            env_name: "api-prod".into(),
            ticket: discovery_ticket(&app, "api-prod"),
            found: Box::default(),
        });
        assert_eq!(
            app.form.as_ref().map(|f| f.state.clone()) == loading(),
            still_loading,
            "{region}"
        );
    }
}

/// The discovery key and the application it looks in are the env's own
/// region's, whichever of two same-named rows comes first: each order
/// is the one a lookup by name alone gets right by accident.
#[tokio::test]
async fn a_discovery_is_for_the_envs_own_region_whichever_row_is_first() {
    for away_first in [false, true] {
        let mut app = test_app();
        let mut home = mk_env("api-prod", "home-app", "Web", "Green");
        home.region = Some("us-east-1".into());
        let mut away = mk_env("api-prod", "away-app", "Web", "Green");
        away.region = Some("eu-west-2".into());
        app.environments = if away_first {
            vec![away, home]
        } else {
            vec![home, away]
        };
        app.rebuild_view();
        let away_row = (0..app.display_rows().len())
            .find(|&row| {
                app.table_state.select(Some(row));
                app.selected_env()
                    .is_some_and(|e| e.region.as_deref() == Some("eu-west-2"))
            })
            .expect("the eu-west-2 row");
        app.table_state.select(Some(away_row));
        app.open_detail();
        let key = app.discovery_key("api-prod");
        assert_eq!(key.region, "eu-west-2", "away first: {away_first}");
        assert_eq!(
            app.discovery_app_name(&key),
            "away-app",
            "away first: {away_first}"
        );
        let home_key = crate::app::EnvKey::new("us-east-1", "api-prod");
        assert_eq!(
            app.discovery_app_name(&home_key),
            "home-app",
            "away first: {away_first}"
        );
    }
}

fn firing(name: &str, env: &str) -> crate::aws::FiringAlarm {
    crate::aws::FiringAlarm {
        name: name.into(),
        dimensions: vec![("EnvironmentName".into(), env.into())],
    }
}

/// `api-prod` in us-east-1 and `api-prod` in eu-west-2.
fn same_named_in_two_regions() -> App {
    let mut app = test_app();
    let home = mk_env("api-prod", "poly", "Web", "Green");
    let mut away = mk_env("api-prod", "poly", "Web", "Green");
    away.region = Some("eu-west-2".into());
    app.environments = vec![home, away];
    app.rebuild_view();
    app
}

fn eu_key(name: &str) -> crate::app::EnvKey {
    crate::app::EnvKey::new("eu-west-2", name)
}

#[tokio::test]
async fn firing_alarms_land_on_the_envs_of_the_region_they_came_from() {
    // Two envs with the SAME name in two regions. Keyed by name alone,
    // whichever region was applied last decided both rows, so a quiet
    // region could erase the other's firing alarm. Both orders.
    for eu_first in [true, false] {
        let mut app = same_named_in_two_regions();
        let mut results = vec![
            ("us-east-1".to_string(), Ok(vec![])),
            (
                "eu-west-2".to_string(),
                Ok(vec![firing("eu-5xx", "api-prod")]),
            ),
        ];
        if eu_first {
            results.reverse();
        }
        app.handle_msg(AppMsg::FiringAlarms {
            gen: app.generation,
            round: 0,
            results,
        });
        assert_eq!(
            app.caches.firing_alarms.get(&eu_key("api-prod")),
            Some(&vec!["eu-5xx".to_string()]),
            "eu_first={eu_first}"
        );
        assert_eq!(
            app.caches.firing_alarms.get(&home_key("api-prod")),
            None,
            "eu_first={eu_first}"
        );
    }

    // A failed region keeps its env's last known alarms, marked
    // unknown; the same-named env in the other region is untouched.
    let mut app = same_named_in_two_regions();
    let ok = |eu: Vec<crate::aws::FiringAlarm>| AppMsg::FiringAlarms {
        gen: 0,
        round: 0,
        results: vec![
            ("us-east-1".into(), Ok(vec![])),
            ("eu-west-2".into(), Ok(eu)),
        ],
    };
    app.handle_msg(ok(vec![firing("eu-5xx", "api-prod")]));
    app.handle_msg(AppMsg::FiringAlarms {
        gen: app.generation,
        round: 0,
        results: vec![
            ("us-east-1".into(), Ok(vec![])),
            ("eu-west-2".into(), Err("AccessDenied".into())),
        ],
    });
    assert_eq!(
        app.caches
            .firing_alarms
            .get(&eu_key("api-prod"))
            .map(Vec::len),
        Some(1)
    );
    assert!(app.caches.firing_alarms.is_stale(&eu_key("api-prod")));
    assert!(!app.caches.firing_alarms.is_stale(&home_key("api-prod")));

    // Recovery clears the mark, and an alarm that stopped firing goes.
    app.handle_msg(ok(vec![]));
    assert!(!app.caches.firing_alarms.is_stale(&eu_key("api-prod")));
    assert_eq!(app.caches.firing_alarms.get(&eu_key("api-prod")), None);
}

#[tokio::test]
async fn a_late_alarm_answer_from_an_older_refresh_is_dropped() {
    let mut app = same_named_in_two_regions();
    let answer = |round, alarms| AppMsg::FiringAlarms {
        gen: 0,
        round,
        results: vec![("eu-west-2".into(), Ok(alarms))],
    };
    app.handle_msg(answer(2, vec![]));
    app.handle_msg(answer(1, vec![firing("old", "api-prod")]));
    assert_eq!(
        app.caches.firing_alarms.get(&eu_key("api-prod")),
        None,
        "round 1 answered after round 2 and must not overwrite it"
    );
}

#[tokio::test]
async fn dlq_depths_of_same_named_workers_in_two_regions_stay_apart() {
    let mut app = same_named_in_two_regions();
    for e in &mut app.environments {
        e.tier = "Worker".into();
    }
    app.handle_msg(AppMsg::WorkerQueueCheck {
        gen: app.generation,
        round: 0,
        results: vec![
            (eu_key("api-prod"), Ok(Some(9))),
            (home_key("api-prod"), Ok(Some(0))),
        ],
    });
    let depths: Vec<i64> = app.environments.iter().map(|e| app.dlq_depth(e)).collect();
    assert_eq!(depths, vec![0, 9], "each row reads its own region's depth");
    assert_eq!(app.alerts, 1, "one worker has a DLQ backlog, not two");
}

#[tokio::test]
async fn a_failed_instance_count_keeps_the_last_counts_instead_of_blanking() {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "poly", "Web", "Green")];
    let counts = crate::aws::EnvInstanceCounts {
        healthy: 2,
        total: 3,
    };
    let answer = |outcome| AppMsg::EnvInstanceCountsCheck {
        gen: 0,
        round: 0,
        results: vec![(home_key("api-prod"), outcome)],
    };
    app.handle_msg(answer(Ok(Some(counts))));
    app.handle_msg(answer(Err("ThrottlingException: slow down".into())));
    assert_eq!(
        app.caches.env_instance_counts.get(&home_key("api-prod")),
        Some(&counts)
    );
    assert!(app
        .caches
        .env_instance_counts
        .is_stale(&home_key("api-prod")));
    // And the cell says the counts are the last known ones.
    app.rebuild_view();
    let frame = render(&mut app, 200, 20);
    assert!(frame.contains("2/3?"), "{frame}");
}

#[tokio::test]
async fn a_refresh_forgets_polled_values_of_envs_that_left_even_an_emptied_fleet() {
    let mut app = test_app();
    app.environments = vec![mk_env("kept", "poly", "Worker", "Green")];
    for name in ["kept", "gone"] {
        app.caches.worker_dlq.set(home_key(name), 3);
        app.caches
            .firing_alarms
            .set(home_key(name), vec!["a".into()]);
        app.caches.env_instance_counts.set(
            home_key(name),
            crate::aws::EnvInstanceCounts {
                healthy: 1,
                total: 1,
            },
        );
    }
    app.apply_refresh(
        app.fanout_epoch,
        Ok(vec![mk_env("kept", "poly", "Worker", "Green")]),
        Vec::new(),
    );
    assert!(app.caches.worker_dlq.get(&home_key("kept")).is_some());
    assert!(app.caches.worker_dlq.get(&home_key("gone")).is_none());
    assert!(app.caches.firing_alarms.get(&home_key("gone")).is_none());
    assert!(app
        .caches
        .env_instance_counts
        .get(&home_key("gone"))
        .is_none());

    // No envs left: no poll runs, and the values still go.
    app.apply_refresh(app.fanout_epoch, Ok(vec![]), Vec::new());
    assert!(app.caches.worker_dlq.get(&home_key("kept")).is_none());
    assert!(app.caches.firing_alarms.get(&home_key("kept")).is_none());
    assert!(app
        .caches
        .env_instance_counts
        .get(&home_key("kept"))
        .is_none());
}

#[tokio::test]
async fn firing_alarms_use_the_envs_alarm_prefix() {
    // The same `alarm_match_for` as the Health and App tabs, so an
    // alarm the App tab lists by prefix is on the fleet view too.
    let mut app = test_app();
    app.environments = vec![mk_env("shop-prod", "poly", "Web", "Green")];
    app.rebuild_view();
    let mut tabs = app.cfg.app_tabs.clone();
    tabs.envs
        .entry("shop-prod".into())
        .or_default()
        .alarm_prefix = Some("shop-prod-".into());
    app.set_app_tabs(tabs);
    let by_name = crate::aws::FiringAlarm {
        name: "shop-prod-db-cpu".into(),
        dimensions: vec![("DBInstanceIdentifier".into(), "db".into())],
    };
    app.handle_msg(AppMsg::FiringAlarms {
        gen: app.generation,
        round: 0,
        results: vec![("us-east-1".into(), Ok(vec![by_name]))],
    });
    assert_eq!(
        app.caches
            .firing_alarms
            .get(&home_key("shop-prod"))
            .map(Vec::len),
        Some(1)
    );
}

#[tokio::test]
async fn the_table_marks_only_the_row_of_the_region_whose_alarm_fires() {
    let mut app = same_named_in_two_regions();
    app.multi_regions = vec!["us-east-1".into(), "eu-west-2".into()];
    app.caches
        .firing_alarms
        .set(eu_key("api-prod"), vec!["eu-5xx".into()]);
    app.rebuild_view();
    let buf = render_buf(&mut app, 220, 20);
    let text = |y: u16| -> String { (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect() };
    let rows: Vec<String> = (0..buf.area.height)
        .map(text)
        // Table rows, not the header breadcrumb.
        .filter(|r| r.contains("api-prod") && r.contains("Ready"))
        .collect();
    assert_eq!(rows.len(), 2, "{rows:?}");
    let marked: Vec<bool> = rows.iter().map(|r| r.contains('⚑')).collect();
    let eu: Vec<bool> = rows.iter().map(|r| r.contains("eu-west-2")).collect();
    assert_eq!(
        marked, eu,
        "the marker is on the eu-west-2 row only: {rows:?}"
    );
}

#[tokio::test]
async fn a_same_named_env_going_red_beside_a_green_twin_is_flagged() {
    // Keyed by name, the previous-health map held one reading for both
    // `api-prod`s, so whichever came last decided whether the other's
    // transition to Red was new. And TREND merged their histories.
    let fleet = |eu_health: &str| {
        let home = mk_env("api-prod", "poly", "Web", "Green");
        let mut away = mk_env("api-prod", "poly", "Web", eu_health);
        away.region = Some("eu-west-2".into());
        // The Red one first: a name-keyed map ends on the Green twin.
        vec![away, home]
    };
    let mut app = test_app();
    app.apply_refresh(app.fanout_epoch, Ok(fleet("Green")), Vec::new());
    app.apply_refresh(app.fanout_epoch, Ok(fleet("Red")), Vec::new());
    assert!(app.caches.newly_red.contains(&eu_key("api-prod")));
    assert!(!app.caches.newly_red.contains(&home_key("api-prod")));
    // And on the next refresh, still Red, it is no longer NEW.
    app.apply_refresh(app.fanout_epoch, Ok(fleet("Red")), Vec::new());
    assert!(!app.caches.newly_red.contains(&eu_key("api-prod")));
    let trend = |k| {
        app.caches
            .history
            .get(&k)
            .map(|h| h.iter().cloned().collect::<Vec<_>>())
    };
    assert_eq!(
        trend(eu_key("api-prod")),
        Some(vec!["Green".into(), "Red".into(), "Red".into()])
    );
    assert_eq!(
        trend(home_key("api-prod")),
        Some(vec!["Green".into(), "Green".into(), "Green".into()])
    );
}

#[tokio::test]
async fn sorting_by_alarm_puts_the_most_alarmed_env_first_descending() {
    let mut app = test_app();
    app.environments = vec![
        mk_env("quiet", "poly", "Web", "Green"),
        mk_env("loud", "poly", "Web", "Green"),
        mk_env("unknown", "poly", "Web", "Green"),
    ];
    app.caches
        .firing_alarms
        .set(home_key("loud"), vec!["a".into(), "b".into()]);
    app.caches
        .firing_alarms
        .reconcile(home_key("unknown"), Err(()));
    app.set_sort(SortKey::Alarms, true);
    let order: Vec<&str> = app.environments.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(order, ["loud", "unknown", "quiet"]);

    // Alarms arrive after the refresh sorted the table: they re-sort it.
    app.handle_msg(AppMsg::FiringAlarms {
        gen: app.generation,
        round: 1,
        results: vec![(
            "us-east-1".into(),
            Ok(vec![
                firing("x", "quiet"),
                firing("y", "quiet"),
                firing("z", "quiet"),
            ]),
        )],
    });
    assert_eq!(app.environments[0].name, "quiet");
    assert_eq!(
        SortKey::parse(SortKey::Alarms.label()),
        Some(SortKey::Alarms)
    );
}

#[tokio::test]
async fn the_alarm_column_is_the_alarm_sort_and_wears_its_marker() {
    let mut app = test_app();
    app.environments = vec![mk_env("api-prod", "poly", "Web", "Green")];
    app.set_sort(SortKey::Alarms, true);
    app.rebuild_view();
    let frame = render(&mut app, 200, 12);
    let header = frame
        .lines()
        .find(|l| l.contains("NAME") && l.contains("STATUS"))
        .unwrap_or_else(|| panic!("{frame}"));
    assert!(
        header.contains("ALARM ▼") || header.contains("ALARM ▲"),
        "{header}"
    );
    assert!(
        !header.contains("HEALTH ▼") && !header.contains("● ▼"),
        "{header}"
    );
}

#[tokio::test]
async fn the_cursor_stays_on_its_env_when_the_rows_re_sort() {
    // The cursor is a row index: when alarms or a new listing reorder
    // the table, it stayed on the row and so moved to another env.
    let mut app = test_app();
    app.environments = vec![
        mk_env("a", "poly", "Web", "Green"),
        mk_env("b", "poly", "Web", "Green"),
        mk_env("c", "poly", "Web", "Green"),
    ];
    app.set_sort(SortKey::Alarms, true);
    app.rebuild_view();
    let on = |app: &App| app.selected_env().map(|e| e.name.clone());
    let row_of = |app: &App, name: &str| {
        app.view
            .display()
            .iter()
            .position(|r| matches!(r, DisplayRow::Env(i) if app.environments[*i].name == name))
            .unwrap()
    };
    app.table_state.select(Some(row_of(&app, "b")));
    app.handle_msg(AppMsg::FiringAlarms {
        gen: app.generation,
        round: 1,
        results: vec![("us-east-1".into(), Ok(vec![firing("x", "a")]))],
    });
    // Descending: c, b, a by name before; a has the alarm now, so a, c,
    // b, and b's row moved.
    assert_eq!(app.environments[0].name, "a", "re-sorted");
    assert_eq!(row_of(&app, "b"), 2);
    assert_eq!(on(&app).as_deref(), Some("b"), "the cursor followed b");

    // A refresh that sorts differently: by health, b goes Red.
    app.set_sort(SortKey::Health, false);
    app.rebuild_view();
    app.table_state.select(Some(row_of(&app, "b")));
    let listing = vec![
        mk_env("a", "poly", "Web", "Green"),
        mk_env("b", "poly", "Web", "Red"),
        mk_env("c", "poly", "Web", "Green"),
    ];
    app.apply_refresh(app.fanout_epoch, Ok(listing), Vec::new());
    assert_eq!(on(&app).as_deref(), Some("b"));
}
