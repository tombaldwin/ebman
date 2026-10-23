//! Tests for `aws`: the client itself: construction, caching, pagination, partitions, STS expiry.

use super::*;

// ── STS expiry conversion ──────────────────────────────────────────

#[test]
fn sts_expiry_converts_a_normal_timestamp() {
    let t = super::sts_expiry_to_system_time(1_700_000_000).expect("representable");
    assert_eq!(
        t.duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        1_700_000_000
    );
}

#[test]
fn sts_expiry_refuses_values_it_cannot_represent() {
    // The old `secs() as u64` wrapped these to ~1.8e19, which made
    // `checked_add` return None, which `Credentials::new` reads as
    // "never expires" — so a skewed clock silently produced a session
    // that was never refreshed and failed every call after an hour.
    for bad in [-1_i64, -1_700_000_000, i64::MIN] {
        let err = super::sts_expiry_to_system_time(bad)
            .expect_err("a negative expiry must be refused, not treated as never-expiring");
        let msg = format!("{err}");
        assert!(
            msg.contains("unusable credential expiry"),
            "error should say what happened: {msg}"
        );
    }
    // Zero is the epoch — representable, but an hour-long STS session
    // is never dated 1970, so it means the same thing. It converts
    // rather than erroring; the SDK sees it as long expired and the
    // refresh tick re-assumes, which is the safe direction.
    assert!(super::sts_expiry_to_system_time(0).is_ok());
}

#[test]
fn sts_expiry_never_wraps_a_large_value_into_the_past() {
    // `SystemTime`'s range is platform-dependent and wide enough here
    // that even `i64::MAX` seconds converts. That's fine — the failure
    // this guards is the other direction, where `as u64` turned a
    // NEGATIVE expiry into a huge one. Whatever the platform does with
    // the top of the range, the result must never land before now.
    if let Ok(t) = super::sts_expiry_to_system_time(i64::MAX) {
        assert!(
            t > std::time::SystemTime::now(),
            "a far-future expiry must not wrap into the past"
        );
    }
    // And the signature itself is the real guarantee: this returns
    // `Result`, so there is no longer any input that yields a silent
    // `None` expiry meaning "never expires".
}

// ── global-service endpoint partition ──────────────────────────────

#[test]
fn global_services_stay_inside_the_operators_partition() {
    use super::global_service_region as g;
    // Commercial — unchanged behaviour.
    assert_eq!(g("us-east-1"), "us-east-1");
    assert_eq!(g("eu-west-2"), "us-east-1");
    assert_eq!(g("ap-southeast-2"), "us-east-1");
    // GovCloud and China: hardcoding us-east-1 here was a
    // cross-partition endpoint, so `:explain` and `:cost on` could
    // never have worked for those operators.
    assert_eq!(g("us-gov-west-1"), "us-gov-west-1");
    assert_eq!(g("us-gov-east-1"), "us-gov-west-1");
    assert_eq!(g("cn-north-1"), "cn-north-1");
    assert_eq!(g("cn-northwest-1"), "cn-north-1");
    // ISO partitions. Both SDKs carry real endpoints for these
    // (`ce.us-iso-east-1.c2s.ic.gov` and friends).
    assert_eq!(g("us-iso-east-1"), "us-iso-east-1");
    assert_eq!(g("us-iso-west-1"), "us-iso-east-1");
    assert_eq!(g("us-isob-east-1"), "us-isob-east-1");
    assert_eq!(g("us-isof-south-1"), "us-isof-south-1");
    assert_eq!(g("eu-isoe-west-1"), "eu-isoe-west-1");
    assert_eq!(g("eusc-de-east-1"), "eusc-de-east-1");
    // A region we've never heard of falls back to commercial rather
    // than failing — same as before, and the common case.
    assert_eq!(g(""), "us-east-1");
    assert_eq!(g("mars-central-1"), "us-east-1");
}

#[test]
fn global_service_region_never_crosses_a_partition() {
    use super::global_service_region as g;
    // The oracle here is a HAND-WRITTEN table of AWS's partition
    // names, not a re-implementation of the function under test. An
    // earlier version of this test derived the partition with the same
    // prefix logic `global_service_region` uses, which made
    // `partition(g(r)) == partition(r)` vacuously true — it passed
    // happily while every ISO region resolved to `us-east-1`, in the
    // commercial partition, which is exactly the bug it claimed to
    // rule out.
    const REGION_PARTITION: &[(&str, &str)] = &[
        ("us-east-1", "aws"),
        ("eu-central-1", "aws"),
        ("sa-east-1", "aws"),
        ("ap-northeast-3", "aws"),
        ("us-gov-west-1", "aws-us-gov"),
        ("us-gov-east-1", "aws-us-gov"),
        ("cn-north-1", "aws-cn"),
        ("cn-northwest-1", "aws-cn"),
        ("us-iso-east-1", "aws-iso"),
        ("us-iso-west-1", "aws-iso"),
        ("us-isob-east-1", "aws-iso-b"),
        ("us-isof-south-1", "aws-iso-f"),
        ("eu-isoe-west-1", "aws-iso-e"),
        ("eusc-de-east-1", "aws-eusc"),
    ];
    let lookup = |r: &str| {
        REGION_PARTITION
            .iter()
            .find(|(name, _)| *name == r)
            .map(|(_, p)| *p)
            .unwrap_or_else(|| panic!("{r} missing from the partition table"))
    };
    for (region, partition) in REGION_PARTITION {
        assert_eq!(
            lookup(g(region)),
            *partition,
            "global endpoint for {region} left the {partition} partition"
        );
    }
}

// ── on-demand clients are built on demand ──────────────────────────

#[test]
fn on_demand_clients_are_not_built_until_used() {
    // `list_environments_in_region` constructs a whole `AwsClient` per
    // region on every refresh tick, so anything built eagerly is paid
    // for per region per tick. These six are only reachable from an
    // explicit operator action, so they must stay unbuilt until one
    // happens.
    let cfg = aws_config::SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    let client = AwsClient::for_tests(
        Client::new(&cfg),
        SqsClient::new(&cfg),
        CwClient::new(&cfg),
        CwLogsClient::new(&cfg),
        S3Client::new(&cfg),
        Ec2Client::new(&cfg),
    );
    assert!(client.cost.get().is_none(), "Cost Explorer built eagerly");
    assert!(client.iam.get().is_none(), "IAM built eagerly");
    assert!(client.org.get().is_none(), "Organizations built eagerly");
    assert!(client.secrets.get().is_none(), "Secrets built eagerly");
    assert!(client.acm.get().is_none(), "ACM built eagerly");
    assert!(client.ssm.get().is_none(), "SSM built eagerly");

    // Touching one builds exactly that one.
    let _ = client.iam();
    assert!(client.iam.get().is_some(), "accessor must build it");
    assert!(
        client.cost.get().is_none(),
        "and must not build its neighbours"
    );
}

// ── the shared paginator ───────────────────────────────────────────

#[tokio::test]
async fn paginate_walks_every_page_in_order() {
    let pages = [
        (vec![1, 2], Some("A".to_string())),
        (vec![3], Some("B".to_string())),
        (vec![4, 5], None),
    ];
    let seen = std::cell::RefCell::new(Vec::new());
    let idx = std::cell::Cell::new(0usize);
    let page: super::Paged<i32> = super::paginate("Test", |token| {
        seen.borrow_mut().push(token.clone());
        let i = idx.get();
        idx.set(i + 1);
        let page = pages[i].clone();
        async move { Ok(page) }
    })
    .await
    .unwrap();
    let items = page.items();
    assert_eq!(items, vec![1, 2, 3, 4, 5]);
    assert_eq!(
        *seen.borrow(),
        vec![None, Some("A".to_string()), Some("B".to_string())],
        "each page must be asked for with the previous page's token"
    );
}

#[tokio::test]
async fn paginate_treats_an_empty_token_as_the_end() {
    // AWS sometimes returns `Some("")` rather than `None`. Following it
    // would re-request page one forever.
    let calls = std::cell::Cell::new(0usize);
    let page: super::Paged<i32> = super::paginate("Test", |_token| {
        calls.set(calls.get() + 1);
        async move { Ok((vec![7], Some(String::new()))) }
    })
    .await
    .unwrap();
    assert!(!page.truncated, "an empty token is a clean finish");
    assert_eq!(page.items(), vec![7]);
    assert_eq!(calls.get(), 1, "an empty token means done");
}

#[tokio::test]
async fn paginate_stops_at_the_runaway_cap() {
    // The reason this helper exists: eleven hand-rolled loops followed
    // tokens unbounded, so an endpoint that always returns one would
    // spin the task forever with the operation stuck "loading".
    let calls = std::cell::Cell::new(0usize);
    let page: super::Paged<i32> = super::paginate("Test", |_token| {
        calls.set(calls.get() + 1);
        async move { Ok((vec![0], Some("ALWAYS".to_string()))) }
    })
    .await
    .unwrap();
    assert_eq!(calls.get(), 100, "must stop at the cap, not spin");
    assert!(page.truncated, "and must say the walk was cut short");
    assert_eq!(page.items().len(), 100, "while returning what it collected");
}

#[tokio::test]
async fn paginate_propagates_a_page_error() {
    let result: Result<super::Paged<i32>, _> = super::paginate("Test", |_token| async move {
        Err(color_eyre::eyre::eyre!("AccessDenied"))
    })
    .await;
    let err = result.expect_err("a failing page must surface");
    assert!(format!("{err}").contains("AccessDenied"));
}

// ── profile+region client cache ────────────────────────────────────

#[tokio::test]
async fn cached_client_reuses_one_client_per_profile_and_region() {
    let _serialised = super::CACHE_TEST_LOCK.lock().await;
    // `list_environments_in_region` runs once per region per refresh
    // tick and used to build a whole `AwsClient` each time — twelve SDK
    // clients plus `aws_config::load()`, which re-reads ~/.aws from
    // disk. Reuse is also what the SDK expects: its credential
    // providers cache and refresh internally.
    super::clear_client_cache();
    let first = super::cached_client(None, "us-east-1".into())
        .await
        .expect("built");
    let second = super::cached_client(None, "us-east-1".into())
        .await
        .expect("cached");
    assert!(
        std::sync::Arc::ptr_eq(&first, &second),
        "the same profile+region must hand back the same client"
    );

    // A different region is a different client.
    let other = super::cached_client(None, "eu-west-2".into())
        .await
        .expect("built");
    assert!(!std::sync::Arc::ptr_eq(&first, &other));

    // Clearing forces a rebuild — this is what a context switch does,
    // since that is also when credentials on disk may have changed.
    super::clear_client_cache();
    let rebuilt = super::cached_client(None, "us-east-1".into())
        .await
        .expect("rebuilt");
    assert!(!std::sync::Arc::ptr_eq(&first, &rebuilt));
    super::clear_client_cache();
}

#[test]
fn aws_client_is_send_and_sync() {
    // The cache stores `Arc<AwsClient>` in a `static`, which requires
    // both. Asserted rather than assumed: a future field with interior
    // mutability (a `Cell`, an `Rc`) would break the cache at a
    // distance, and the error would point at the static, not the field.
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<AwsClient>();
    assert_send_sync::<std::sync::Arc<AwsClient>>();
}

#[tokio::test]
async fn a_clear_during_a_build_is_not_undone_by_the_in_flight_builder() {
    let _serialised = super::CACHE_TEST_LOCK.lock().await;
    // The race, driven deterministically: a builder captures the epoch,
    // a `clear_client_cache()` lands while it's awaiting
    // `AwsClient::with`, and the stranded builder then tries to install
    // a client resolved from the PRE-switch `~/.aws`. If it succeeds it
    // repopulates the map the operator's `:profile` switch just
    // emptied, and every fan-out authenticates as the old profile for
    // the full TTL while the header shows the new context.
    //
    // The previous version of this test only checked that the epoch
    // counter increments and that a build started AFTER a clear caches
    // — so deleting the entire guard left it green.
    super::clear_client_cache();
    let key = (None, "us-east-1".to_string());
    let client = super::cached_client(None, "us-east-1".into())
        .await
        .expect("built");

    // Builder captures the epoch...
    let epoch = super::cache_epoch_for_tests();
    // ...operator switches profile, which clears...
    super::clear_client_cache();
    assert!(
        !super::is_cached_for_tests(&key),
        "the clear emptied the map"
    );
    // ...and the stranded builder tries to install.
    let installed = super::install_if_current_for_tests(key.clone(), epoch, client.clone());
    assert!(!installed, "a build from before the clear must not install");
    assert!(
        !super::is_cached_for_tests(&key),
        "the cleared map must stay empty"
    );

    // A build that starts after the clear installs normally.
    let epoch = super::cache_epoch_for_tests();
    assert!(super::install_if_current_for_tests(
        key.clone(),
        epoch,
        client
    ));
    assert!(super::is_cached_for_tests(&key));
    super::clear_client_cache();
}

// ── the truncation convention can't drift ──────────────────────────

#[test]
fn every_paginated_listing_declares_how_it_treats_truncation() {
    // The rule — "a short result is only acceptable where the caller
    // renders it as a list and nothing more" — was applied to some
    // listings and not others in the same pass, leaving the next
    // reader unable to tell which convention held. Anything taking
    // `.items()` must be named here with a reason, so adding a caller
    // is a deliberate choice rather than a default.
    const ITEMS_IS_CORRECT_BECAUSE: &[(&str, &str)] = &[
        (
            "list_secrets",
            "unfiltered browse — a shorter list is just shorter; the \
             filtered path calls .complete() because then a miss is a claim",
        ),
        (
            "cmd_explain",
            "a partial IAM diagnosis is still a diagnosis, and the \
             overlay prints an INCOMPLETE banner so a missing action \
             can't be read as an allowed one",
        ),
    ];

    // Walk all of `src`, not just `src/aws`: `Paged` is pub(crate), so
    // the callers that actually have to make this choice live in
    // `app/` and `cli/`. Scoping the guard to the module that *defines*
    // the type let the first out-of-tree `.items()` land unlabelled.
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    let mut stack = vec![std::path::PathBuf::from("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("src dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs")
                && path.file_name().and_then(|f| f.to_str()) != Some("tests.rs")
                // Test trees too (`aws/tests/`, `app/tests/`): this guard
                // is about production callers.
                && !path.components().any(|c| c.as_os_str() == "tests")
            {
                files.push(path);
            }
        }
    }
    assert!(
        files.len() > 20,
        "the walk found only {} files — it isn't reaching the tree",
        files.len()
    );

    let mut offenders: Vec<String> = Vec::new();
    for path in files {
        let raw = std::fs::read_to_string(&path).expect("read");
        // Comments discuss `.items()` by name, so strip them first —
        // otherwise the guard flags the very comment explaining why a
        // site calls `.complete()` instead.
        let text: String = raw
            .lines()
            .map(|l| match l.find("//") {
                Some(i) => &l[..i],
                None => l,
            })
            .collect::<Vec<_>>()
            .join("\n");
        // Walk `.items()` call sites and name the enclosing fn.
        let mut idx = 0usize;
        while let Some(rel) = text[idx..].find(".items()") {
            let at = idx + rel;
            // Prose mentions it as `` `.items()` ``, and one of them
            // lives in a `#[must_use]` string rather than a comment,
            // so comment-stripping alone doesn't catch it. A real call
            // site is preceded by a receiver — never by a backtick.
            if text[..at].ends_with('`') {
                idx = at + ".items()".len();
                continue;
            }
            let enclosing = text[..at]
                .rmatch_indices("fn ")
                .next()
                .map(|(i, _)| {
                    text[i + 3..]
                        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                        .next()
                        .unwrap_or("")
                        .to_string()
                })
                .unwrap_or_default();
            if !ITEMS_IS_CORRECT_BECAUSE
                .iter()
                .any(|(name, _)| *name == enclosing)
            {
                offenders.push(format!(
                    "{}::{enclosing}",
                    path.file_name().unwrap().to_string_lossy()
                ));
            }
            idx = at + ".items()".len();
        }
    }
    offenders.sort();
    offenders.dedup();
    assert!(
        offenders.is_empty(),
        "these take `.items()` without declaring why a short result is \
         acceptable — either call `.complete()` or add them to \
         ITEMS_IS_CORRECT_BECAUSE with a reason: {offenders:?}"
    );
}

#[tokio::test]
async fn a_walk_that_outruns_the_deadline_reports_itself_truncated() {
    // The page budgets bound round trips, not the wait. `SCAN_PAGES` is
    // worst-case 500 SEQUENTIAL round trips, and three of these walks
    // sit on interactive triage paths with no cancel and no partial
    // render — so a throttled account left the operator on an
    // unbounded spinner. Hitting the deadline is the same signal as
    // hitting the page cap: honest, and `.complete()` turns it into an
    // error rather than a false "no match".
    //
    // A page budget far above the number of pages this can get through
    // in the deadline, so reaching `truncated` can ONLY be the clock.
    let result = super::paginate_until(
        "slow_listing",
        1_000,
        std::time::Duration::from_millis(60),
        |_token| async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            Ok((vec![1u8], Some("more".to_string())))
        },
    )
    .await
    .expect("a deadline is not an error");
    assert!(
        result.truncated,
        "a walk that outruns the deadline must say so"
    );
    let items = result.items();
    assert!(
        !items.is_empty(),
        "and keep what it collected — a partial list beats nothing"
    );
    assert!(
        items.len() < 1_000,
        "it stopped on the clock, not on the page budget: {} pages",
        items.len()
    );
}

#[tokio::test]
async fn a_last_page_that_lands_after_the_deadline_is_still_complete() {
    // Nothing was left to fetch, so nothing is missing: `.complete()`
    // must not turn a slow but whole answer into an error.
    let result = super::paginate_until(
        "slow_last_page",
        10,
        std::time::Duration::ZERO,
        |_token| async move { Ok((vec![1u8], None)) },
    )
    .await
    .expect("ok");
    assert!(!result.truncated, "the last page was the last");
}

#[tokio::test]
async fn a_walk_inside_the_deadline_is_not_marked_truncated() {
    // The deadline must not cry wolf on a healthy account — a false
    // `truncated` turns a complete `.complete()` listing into an error.
    let mut pages = 0usize;
    let result = super::paginate_until(
        "quick_listing",
        10,
        std::time::Duration::from_secs(30),
        |_token| {
            pages += 1;
            let last = pages >= 3;
            async move {
                Ok((
                    vec![1u8],
                    if last { None } else { Some("more".to_string()) },
                ))
            }
        },
    )
    .await
    .expect("ok");
    assert!(!result.truncated, "a complete walk stays complete");
    assert_eq!(result.items().len(), 3);
}

#[tokio::test]
async fn the_role_cache_is_cleared_by_a_context_switch() {
    // Assumed-role clients are cached with the same five-minute TTL as
    // profile clients — safe because an AssumeRole session's hard cap
    // is an hour, so an entry can never outlive its credentials.
    // Without the cache, per-env work under `:account` re-assumes per
    // call, and `spawn_env_instance_counts` builds one client per row
    // on every 15-second tick: an STS AssumeRole storm on a large
    // fleet. But a `:profile` / `:account` switch must still empty it,
    // or the new context is served the old one's session.
    let _guard = super::CACHE_TEST_LOCK.lock().await;
    super::clear_client_cache();
    assert_eq!(
        super::role_cache().lock().expect("lock").len(),
        0,
        "a clear empties the role cache too, not just the profile one"
    );

    // Seed it directly (assuming a role needs a live STS) and prove the
    // clear reaches it.
    super::role_cache().lock().expect("lock").insert(
        ("prod".to_string(), "eu-west-2".to_string()),
        (
            std::time::Instant::now(),
            std::sync::Arc::new(super::AwsClient::stub()),
        ),
    );
    assert_eq!(super::role_cache().lock().expect("lock").len(), 1);
    super::clear_client_cache();
    assert_eq!(
        super::role_cache().lock().expect("lock").len(),
        0,
        "a context switch must not leave the previous account's session behind"
    );
}

/// Every test that can reach the process-global client cache takes
/// `CACHE_TEST_LOCK`.
///
/// The lock existed, and two tests that clear the cache did not take
/// it: `handle_msg(AppMsg::Rebuild { .. Ok .. })` reaches
/// `apply_rebuild`, which calls `clear_client_cache()`. Whenever one of
/// them ran beside `cached_client_reuses_one_client_per_profile_and_region`,
/// the clear landed between that test's two lookups and it failed —
/// seven runs in eight under `cargo test -- region`. The full suite
/// happened to schedule them apart, so CI stayed green and the race
/// stayed invisible from where it fired.
///
/// Conservative on purpose: a test that names an entry point to the
/// cache must take the lock even if its particular inputs happen not
/// to clear (a stale-epoch rebuild returns early). Taking an
/// uncontended lock costs nothing; working out which inputs reach the
/// clear is exactly the reasoning that went wrong.
///
/// Scope, stated: it sees DIRECT entry points only. A test that reaches
/// the cache through a longer production path is not caught here.
#[cfg(test)]
mod cache_lock_guard {
    use syn::spanned::Spanned;
    use syn::visit::Visit;

    /// Calls that reach the global client cache from a test.
    const ENTRY_POINTS: &[&str] = &["apply_rebuild(", "clear_client_cache(", "cached_client("];

    /// Does this test body reach the cache? A `Rebuild` message reaches
    /// it only when DISPATCHED: a test that merely constructs one to
    /// inspect a field (`generation()`) touches nothing.
    fn reaches_cache(body: &str) -> bool {
        ENTRY_POINTS.iter().any(|e| body.contains(e))
            || (body.contains("AppMsg::Rebuild") && body.contains("handle_msg("))
    }

    /// `(fn name, body text)` of every `#[test]` / `#[tokio::test]` fn.
    struct TestFns<'a> {
        lines: &'a [&'a str],
        out: Vec<(String, String)>,
    }

    impl<'ast> Visit<'ast> for TestFns<'_> {
        fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
            let is_test = f.attrs.iter().any(|a| {
                let p = a.path();
                p.is_ident("test") || p.segments.last().is_some_and(|s| s.ident == "test")
            });
            if is_test {
                let span = f.block.span();
                let (start, end) = (span.start().line, span.end().line);
                let body: String = self.lines[start.saturating_sub(1)..end.min(self.lines.len())]
                    .iter()
                    .map(|l| crate::app::tests::scan::strip_line_comment(l))
                    .collect::<Vec<_>>()
                    .join("\n");
                self.out.push((f.sig.ident.to_string(), body));
            }
            syn::visit::visit_item_fn(self, f);
        }
    }

    /// Test fns in `src` that name a cache entry point and do not take
    /// the lock.
    pub(super) fn unlocked(src: &str) -> Vec<String> {
        let Ok(file) = syn::parse_file(src) else {
            return Vec::new();
        };
        let lines: Vec<&str> = src.lines().collect();
        let mut v = TestFns {
            lines: &lines,
            out: Vec::new(),
        };
        v.visit_file(&file);
        v.out
            .into_iter()
            .filter(|(_, body)| reaches_cache(body) && !body.contains("CACHE_TEST_LOCK"))
            .map(|(name, _)| name)
            .collect()
    }

    #[test]
    fn every_test_that_reaches_the_client_cache_takes_the_lock() {
        let mut offenders = Vec::new();
        let mut parsed = 0usize;
        for (path, src) in crate::app::tests::scan::source_files() {
            if syn::parse_file(&src).is_ok() {
                parsed += 1;
            }
            for name in unlocked(&src) {
                offenders.push(format!("{path}: {name}"));
            }
        }
        assert!(
            parsed > 50,
            "only {parsed} files parsed — a guard over nothing passes vacuously"
        );
        assert!(
            offenders.is_empty(),
            "these tests reach the process-global client cache without \
             `crate::aws::CACHE_TEST_LOCK` and race the cache tests: {offenders:#?}"
        );
    }

    /// The detector detects: a test that clears the cache unlocked is
    /// reported, and the same test holding the lock is not.
    #[test]
    fn the_detector_flags_an_unlocked_clear_and_passes_a_locked_one() {
        let unlocked_src =
            "#[tokio::test]\nasync fn racy() {\n    crate::aws::clear_client_cache();\n}\n";
        assert_eq!(unlocked(unlocked_src), vec!["racy".to_string()]);
        let locked_src = "#[tokio::test]\nasync fn safe() {\n    let _g = crate::aws::CACHE_TEST_LOCK.lock().await;\n    crate::aws::clear_client_cache();\n}\n";
        assert!(unlocked(locked_src).is_empty());
        // A comment mentioning the lock does not count as taking it.
        let commented = "#[test]\nfn sly() {\n    // CACHE_TEST_LOCK\n    crate::aws::clear_client_cache();\n}\n";
        assert_eq!(unlocked(commented), vec!["sly".to_string()]);
        // A non-test fn is not a test.
        assert!(unlocked("fn helper() { crate::aws::clear_client_cache(); }").is_empty());
        // A dispatched Rebuild reaches the cache; a constructed one does not.
        let dispatched = "#[tokio::test]\nasync fn switch() {\n    app.handle_msg(AppMsg::Rebuild { epoch: 1, result: r });\n}\n";
        assert_eq!(unlocked(dispatched), vec!["switch".to_string()]);
        let constructed =
            "#[test]\nfn shape() {\n    let m = AppMsg::Rebuild { epoch: 1, result: r };\n}\n";
        assert!(unlocked(constructed).is_empty());
    }
}

/// An AWS error's PLAIN `Display` names what AWS refused.
///
/// The CLI and MCP print errors with `{e}` / `e.to_string()` at some
/// sixty sites. `aws_ctx` used to put the operation in its own outer
/// layer, so all of them printed `"DeleteMessage failed"` and nothing
/// else — only the TUI, which downcasts for the reason, showed it. So
/// 0.44's headline, "errors that name what AWS refused", held in one
/// surface of three.
///
/// Driven through a real SDK call, not a hand-built chain: the shape of
/// the chain is the thing under test.
#[tokio::test]
async fn an_aws_errors_plain_display_names_what_aws_refused() {
    use aws_sdk_sqs::operation::delete_message::DeleteMessageError;
    let rule = mock!(aws_sdk_sqs::Client::delete_message).then_error(|| {
        DeleteMessageError::generic(
            aws_smithy_types::error::ErrorMetadata::builder()
                .code("AccessDenied")
                .message("User is not authorized to perform sqs:DeleteMessage")
                .build(),
        )
    });
    let client = client_with_sqs(mock_client!(aws_sdk_sqs, [&rule]));
    let e = client
        .delete_message("https://sqs.us-east-1.amazonaws.com/123456789012/q", "rh-1")
        .await
        .expect_err("denied");

    let plain = e.to_string();
    // "DeleteMessage failed", not "DeleteMessage": AWS's own sentence
    // ("…perform sqs:DeleteMessage") contains the bare word, so that
    // assertion passed with the operation gone.
    assert!(
        plain.starts_with("DeleteMessage failed:"),
        "leads with the operation: {plain}"
    );
    assert!(
        plain.contains("User is not authorized to perform sqs:DeleteMessage"),
        "a plain `{{e}}` must carry AWS's own sentence: {plain}"
    );

    // The TUI's renderer must not now say it twice. (`{e:#}` is not
    // asserted on: it walks down into the SDK's own error, whose
    // Display repeats the message whatever this layer does.)
    let flat = crate::app::flatten_err_to_string(&e);
    assert!(flat.starts_with("AccessDenied:"), "{flat}");
    assert_eq!(flat.matches("User is not authorized").count(), 1, "{flat}");
}

/// A failure that never reached the service keeps its cause in the
/// plain `Display`.
///
/// Timeouts, DNS, expired SSO sessions and dispatch failures carry no
/// error code and no message — `ProvideErrorMetadata` is empty for
/// anything but a service error — so once the operation moved into the
/// same layer as the reason, their plain text was the bare operation
/// name. The release review found six `{e}` sites that had printed the
/// cause via `{e:#}` and lost it, including an SSM run on an expired
/// session no longer saying "unable to load credentials".
#[test]
fn a_failure_that_never_reached_the_service_keeps_its_cause() {
    use aws_sdk_elasticbeanstalk::config::http::HttpResponse;
    use aws_sdk_elasticbeanstalk::error::SdkError;
    use aws_sdk_elasticbeanstalk::operation::describe_environments::DescribeEnvironmentsError;
    let r: Result<(), SdkError<DescribeEnvironmentsError, HttpResponse>> =
        Err(SdkError::timeout_error("the connection timed out"));
    let e = super::SdkResultExt::aws_ctx(r, "DescribeEnvironments failed").expect_err("failed");
    let plain = e.to_string();
    assert!(plain.starts_with("DescribeEnvironments failed"), "{plain}");
    assert!(
        plain.contains("the connection timed out"),
        "the cause must survive a plain `{{e}}`: {plain}"
    );
}

/// The cause of a non-service failure must not read to the TUI as the
/// service's own answer. The first cut of the fix above stored it in
/// `message`, and `flatten_err_to_string` returns early on a message —
/// before the sniff that adds `ThrottlingException:` — so a rate limit
/// hit while loading credentials no longer armed the refresh back-off.
#[test]
fn a_non_service_cause_is_still_classified_by_the_tui() {
    use aws_sdk_elasticbeanstalk::config::http::HttpResponse;
    use aws_sdk_elasticbeanstalk::error::SdkError;
    use aws_sdk_elasticbeanstalk::operation::describe_environments::DescribeEnvironmentsError;
    let r: Result<(), SdkError<DescribeEnvironmentsError, HttpResponse>> = Err(
        SdkError::construction_failure("TooManyRequestsException: Rate exceeded"),
    );
    let e = super::SdkResultExt::aws_ctx(r, "DescribeEnvironments failed").expect_err("failed");
    let flat = crate::app::flatten_err_to_string(&e);
    assert!(
        crate::app::is_throttling_error(&flat),
        "a throttle that never reached the service must still back off: {flat}"
    );
    assert!(flat.contains("Rate exceeded"), "and keep its cause: {flat}");
}

#[test]
fn enhanced_health_maps_to_per_instance_app_stats() {
    use aws_sdk_elasticbeanstalk::types::{
        ApplicationMetrics, CpuUtilization, Latency, SingleInstanceHealth, StatusCodes,
        SystemStatus,
    };
    let full = SingleInstanceHealth::builder()
        .application_metrics(
            ApplicationMetrics::builder()
                .duration(10)
                .request_count(120)
                .latency(Latency::builder().p99(0.84).build())
                .status_codes(StatusCodes::builder().status4xx(2).status5xx(7).build())
                .build(),
        )
        .system(
            SystemStatus::builder()
                .cpu_utilization(CpuUtilization::builder().idle(59.0).build())
                .build(),
        )
        .build();
    assert_eq!(
        crate::aws::instance_app_stats(&full),
        Some(crate::aws::InstanceAppStats {
            requests_per_sec: Some(12.0),
            p99_secs: Some(0.84),
            pct_4xx: Some(2),
            pct_5xx: Some(7),
            cpu_pct: Some(41.0),
        })
    );

    // No window length: the count is taken as already per second.
    let no_window = SingleInstanceHealth::builder()
        .application_metrics(ApplicationMetrics::builder().request_count(5).build())
        .build();
    assert_eq!(
        crate::aws::instance_app_stats(&no_window).and_then(|s| s.requests_per_sec),
        Some(5.0)
    );

    // Basic health reports neither section.
    assert_eq!(
        crate::aws::instance_app_stats(&SingleInstanceHealth::builder().build()),
        None
    );

    // CPU only, and a bogus idle is clamped rather than going negative.
    let cpu_only = SingleInstanceHealth::builder()
        .system(
            SystemStatus::builder()
                .cpu_utilization(CpuUtilization::builder().idle(140.0).build())
                .build(),
        )
        .build();
    let s = crate::aws::instance_app_stats(&cpu_only).expect("cpu alone is something");
    assert_eq!((s.cpu_pct, s.requests_per_sec), (Some(0.0), None));
}

#[tokio::test(start_paused = true)]
async fn a_bounded_insights_query_is_stopped_on_the_server_when_it_runs_over() {
    // Giving up client-side and leaving the query running would keep it
    // scanning, and billing, with nobody waiting for the answer.
    use aws_sdk_cloudwatchlogs::types::QueryStatus;
    let start = mock!(aws_sdk_cloudwatchlogs::Client::start_query).then_output(|| {
        aws_sdk_cloudwatchlogs::operation::start_query::StartQueryOutput::builder()
            .query_id("q1")
            .build()
    });
    let running = mock!(aws_sdk_cloudwatchlogs::Client::get_query_results).then_output(|| {
        aws_sdk_cloudwatchlogs::operation::get_query_results::GetQueryResultsOutput::builder()
            .status(QueryStatus::Running)
            .build()
    });
    let stop = mock!(aws_sdk_cloudwatchlogs::Client::stop_query)
        .match_requests(|req| req.query_id() == Some("q1"))
        .then_output(|| {
            aws_sdk_cloudwatchlogs::operation::stop_query::StopQueryOutput::builder()
                .success(true)
                .build()
        });
    let client = client_with_cw_logs(mock_client!(
        aws_sdk_cloudwatchlogs,
        aws_smithy_mocks::RuleMode::MatchAny,
        [&start, &running, &stop]
    ));
    let err = client
        .run_insights_query_within(
            &["g".to_string()],
            0,
            1000,
            "fields @message",
            Some(std::time::Duration::from_secs(5)),
        )
        .await
        .expect_err("ran past its bound");
    assert!(
        err.to_string().contains("ran past 5s and was stopped"),
        "{err}"
    );
    assert_eq!(stop.num_calls(), 1, "the query is stopped on the server");
    assert!(running.num_calls() >= 2, "it polled until the bound");
}

#[tokio::test]
async fn a_discovery_scan_that_runs_out_of_pages_is_refused_not_shortened() {
    // A cut-off scan used to come back as if complete, so a namespace
    // past the cut read as "no metrics found".
    let endless = mock!(aws_sdk_cloudwatch::Client::list_metrics).then_output(|| {
        aws_sdk_cloudwatch::operation::list_metrics::ListMetricsOutput::builder()
            .next_token("more")
            .build()
    });
    let client = client_with_cw(mock_client!(
        aws_sdk_cloudwatch,
        aws_smithy_mocks::RuleMode::MatchAny,
        [&endless]
    ));
    let err = client
        .list_metrics_dimensioned_by(&["Environment".to_string()], "e")
        .await
        .expect_err("truncated");
    assert!(err.to_string().contains("page budget"), "{err}");
    assert_eq!(endless.num_calls(), super::DISCOVERY_PAGES);
}

#[tokio::test]
async fn an_envs_own_metrics_are_filtered_by_cloudwatch_not_after() {
    // The filter is the env's dimension name AND value, sent with the
    // request, so the page budget is spent on that env's metrics; and
    // AWS/ namespaces (EB's own carry EnvironmentName too) are dropped.
    use aws_sdk_cloudwatch::types::{Dimension, Metric};
    let rule = mock!(aws_sdk_cloudwatch::Client::list_metrics)
        .match_requests(|r| {
            r.namespace().is_none()
                && r.dimensions().len() == 1
                && r.dimensions()[0].name() == Some("Environment")
                && r.dimensions()[0].value() == Some("Shop-prod")
        })
        .then_output(|| {
            let m = |ns: &str, name: &str| {
                Metric::builder()
                    .namespace(ns)
                    .metric_name(name)
                    .dimensions(
                        Dimension::builder()
                            .name("Environment")
                            .value("Shop-prod")
                            .build(),
                    )
                    .build()
            };
            aws_sdk_cloudwatch::operation::list_metrics::ListMetricsOutput::builder()
                .metrics(m("Shop", "usersActive"))
                .metrics(m("AWS/ElasticBeanstalk", "EnvironmentHealth"))
                .build()
        });
    let client = client_with_cw(mock_client!(
        aws_sdk_cloudwatch,
        aws_smithy_mocks::RuleMode::MatchAny,
        [&rule]
    ));
    let got = client
        .list_metrics_dimensioned_by(&["Environment".to_string()], "Shop-prod")
        .await
        .expect("listed");
    let names: Vec<(&str, &str)> = got
        .iter()
        .map(|m| (m.namespace.as_str(), m.name.as_str()))
        .collect();
    assert_eq!(names, [("Shop", "usersActive")]);
    assert_eq!(rule.num_calls(), 1);
}

#[tokio::test]
async fn one_dimension_names_failure_does_not_discard_what_another_found() {
    use aws_sdk_cloudwatch::types::{Dimension, Metric};
    let found = mock!(aws_sdk_cloudwatch::Client::list_metrics)
        .match_requests(|r| r.dimensions().first().and_then(|d| d.name()) == Some("Environment"))
        .then_output(|| {
            aws_sdk_cloudwatch::operation::list_metrics::ListMetricsOutput::builder()
                .metrics(
                    Metric::builder()
                        .namespace("Shop")
                        .metric_name("users")
                        .dimensions(Dimension::builder().name("Environment").value("e").build())
                        .build(),
                )
                .build()
        });
    let denied = mock!(aws_sdk_cloudwatch::Client::list_metrics)
        .match_requests(|r| r.dimensions().first().and_then(|d| d.name()) == Some("Env"))
        .then_error(|| {
            aws_sdk_cloudwatch::operation::list_metrics::ListMetricsError::unhandled("Throttling")
        });
    let client = client_with_cw(mock_client!(
        aws_sdk_cloudwatch,
        aws_smithy_mocks::RuleMode::MatchAny,
        [&found, &denied]
    ));
    let got = client
        .list_metrics_dimensioned_by(&["Env".to_string(), "Environment".to_string()], "e")
        .await
        .expect("what Environment found stands");
    assert_eq!(got.len(), 1);
    // Nothing found and a name failed: not an answer.
    let err = client
        .list_metrics_dimensioned_by(&["Env".to_string()], "e")
        .await
        .expect_err("none found is not an answer when a name failed");
    assert!(format!("{err:?}").contains("ListMetrics"), "{err:?}");
}

/// Every SDK call that propagates its failure goes through `aws_ctx`.
///
/// `SdkError`'s `Display` for a modelled service failure is the literal
/// string **"service error"**. A call finished with a bare `?` or a
/// plain `wrap_err` therefore reports the operation and throws away the
/// service's own sentence — the one that says WHICH permission is
/// missing, or WHY the receipt handle was rejected. Seventy-eight of
/// eighty call sites did exactly that, each one individually
/// reasonable-looking, because `.wrap_err("Op failed")?` is shorter
/// than the correct form used to be.
///
/// That is why the fix was a postfix trait and not a rule: `aws_ctx` is
/// now the *shortest* way to finish the call, and this test is what
/// stops site eighty-one from finding a shorter wrong one.
///
/// **Deliberately not an allowlist.** The rule is stated in terms of
/// what the code does, so the legitimate shapes fall out of it rather
/// than being named:
///
/// - a discarded result (`let _ = … .send().await;`) has no `?`, so it
///   carries nothing to an operator and is not matched;
/// - a stored future (`… .send();`) has no `.await` in its statement
///   and is checked where it is awaited;
/// - a site that keeps the TYPED error on purpose — `ssm.rs` calls
///   `as_service_error()` to tell `InvocationDoesNotExist` from a real
///   permission failure — never converts to a `Report`, so it has no
///   `?` here either.
///
/// Every one of those would have needed a name on a list, and a list is
/// what turns "the guard fired" into "add the name".
#[test]
fn every_sdk_call_that_propagates_goes_through_aws_ctx() {
    let mut offenders: Vec<String> = Vec::new();
    let mut checked = 0usize;

    for (path, text) in crate::app::tests::scan::source_files() {
        if crate::app::tests::scan::is_test_path(&path) {
            continue;
        }
        if !(path == "src/aws.rs" || path.starts_with("src/aws/")) {
            continue;
        }
        let prod = crate::app::tests::scan::production_half(&text);
        let lines: Vec<&str> = prod.lines().collect();
        for (n, line) in lines.iter().enumerate() {
            let code = crate::app::tests::scan::strip_line_comment(line);
            // `.send()` is the direct case. A STORED future is the
            // deferred one: `let x_fut = … .send();` for
            // `tokio::try_join!` is awaited on a line that names no
            // operation, so a scan looking only for `.send()` cannot
            // see where its failure is handled.
            //
            // Matched on the NAME, not on `_fut.await`. The first
            // version required both on one line and `cargo fmt` splits
            // them — so that rule matched nothing, passed, and was
            // found only by mutating the thing it claimed to guard.
            let uses_stored_future = code.contains("_fut") && !code.contains("_fut = ");
            if !code.contains(".send()") && !uses_stored_future {
                continue;
            }
            // The statement this line belongs to. Bounded: a
            // continuation ends at a `;`, at an opening `{`, or at the
            // `}` closing the block it is the tail of — and at 12 lines
            // regardless, so a runaway scan cannot swallow an unrelated
            // `aws_ctx` and report compliance.
            let mut stmt = String::new();
            for l in lines.iter().skip(n).take(12) {
                let c = crate::app::tests::scan::strip_line_comment(l);
                stmt.push_str(c);
                stmt.push('\n');
                let t = c.trim_end();
                if t.ends_with(';') || t.ends_with('{') || t.ends_with('}') || t.ends_with("},") {
                    break;
                }
            }
            if !stmt.contains(".await") {
                // A stored future's BINDING half. It carries no failure
                // itself; the convention is what lets the await half be
                // found at all, so the convention is asserted here.
                if stmt.trim_end().ends_with(".send();") {
                    let binds_fut = lines[n.saturating_sub(8)..=n]
                        .iter()
                        .any(|l| l.contains("_fut = "));
                    assert!(
                        binds_fut,
                        "{path}:{}: a stored SDK future must bind a name ending \
                         `_fut`, because that is how the await half of this guard \
                         finds it",
                        n + 1
                    );
                }
                continue;
            }
            checked += 1;
            let propagates = stmt.contains('?') || stmt.contains(".wrap_err");
            let contextualised = stmt.contains("aws_ctx");
            if propagates && !contextualised {
                offenders.push(format!("{path}:{}", n + 1));
            }
        }
    }

    // A floor, because a filter that stops matching reports clean.
    assert!(
        checked > 60,
        "only {checked} SDK calls inspected — the statement scan has stopped \
         matching and this guard is passing over nothing"
    );
    assert!(
        offenders.is_empty(),
        "these SDK calls propagate a failure without `aws_ctx`, so the service's \
         own message is discarded and the operator sees the operation name over \
         the SDK's literal \"service error\": {offenders:?}"
    );
}

#[test]
fn seeding_a_lazy_client_wins_over_get_or_init() {
    // How the mock-AWS tests inject: seed the cell, and `get_or_init`
    // hands back the mock rather than constructing a real client.
    let cfg = aws_config::SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    let client = AwsClient::for_tests(
        Client::new(&cfg),
        SqsClient::new(&cfg),
        CwClient::new(&cfg),
        CwLogsClient::new(&cfg),
        S3Client::new(&cfg),
        Ec2Client::new(&cfg),
    );
    let seeded = aws_sdk_iam::Client::new(&cfg);
    assert!(client.iam.set(seeded).is_ok());
    // Accessor returns the seeded instance, not a fresh one.
    let got = client.iam() as *const _;
    let stored = client.iam.get().unwrap() as *const _;
    assert_eq!(got, stored);
}

#[tokio::test]
async fn assume_role_under_test_refuses_without_calling_sts() {
    // A role client for a row in another region under `:account NAME`
    // resolves through here. Unseeded, it must fail at once rather
    // than read `~/.aws` and send an AssumeRole.
    let spec = crate::config::AccountSpec {
        role_arn: "arn:aws:iam::111111111111:role/poly-readonly".into(),
        source_profile: Some("poly-dev".into()),
        region: Some("eu-west-2".into()),
        ..Default::default()
    };
    let err = crate::aws::cached_role_client("poly-unseeded", &spec)
        .await
        .err()
        .expect("an unseeded role client is refused under test");
    assert!(
        err.to_string().contains("not reachable from a test"),
        "refused for the right reason: {err}"
    );
}

#[tokio::test]
async fn with_under_test_reads_no_aws_config_and_holds_no_credentials() {
    // A spawned `:region all` refresh reaches `AwsClient::with` from a
    // test. Built from `~/.aws` it would carry the developer's
    // credentials, and a yield in the test would let it call AWS.
    let client = crate::aws::AwsClient::with(Some("poly-dev".into()), Some("eu-west-2".into()))
        .await
        .expect("offline client");
    assert!(
        client.config.credentials_provider().is_none(),
        "a test client must hold no credentials"
    );
    assert_eq!(client.context.region, "eu-west-2");
    assert_eq!(
        client.config.region().map(|r| r.as_ref()),
        Some("eu-west-2"),
        "the SDK config is named for the region too"
    );
    assert_eq!(client.context.profile.as_deref(), Some("poly-dev"));
}
