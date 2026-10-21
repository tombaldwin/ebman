//! Tests for `aws`: Elastic Beanstalk.

use super::*;

#[test]
fn platform_branch_from_arn_takes_full_branch_segment() {
    // The ARN's name segment itself contains " running " — the
    // solution-stack split must not fire first (it used to,
    // returning "on 64bit Amazon Linux 2023/4.0.1").
    assert_eq!(
            platform_branch_from(
                "arn:aws:elasticbeanstalk:us-east-1::platform/Python 3.9 running on 64bit Amazon Linux 2023/4.0.1"
            ),
            "Python 3.9 running on 64bit Amazon Linux 2023"
        );
}

#[test]
fn platform_branch_from_solution_stack_yields_family_prefix() {
    // Bare family — a prefix of the real PlatformBranchName
    // ("Python 3.9 running on …"), which is why the filter uses
    // begins_with rather than `=`.
    assert_eq!(
        platform_branch_from("64bit Amazon Linux 2023 v4.0.1 running Python 3.9"),
        "Python 3.9"
    );
    assert_eq!(platform_branch_from(""), "");
}

#[test]
fn platform_family_from_solution_stack() {
    assert_eq!(
        platform_family("64bit Amazon Linux 2 v3.5.0 running Java 17"),
        "Java 17"
    );
    assert_eq!(
        platform_family("64bit Amazon Linux 2 v3.7.0 running Tomcat 9 Corretto 17"),
        "Tomcat 9 Corretto 17"
    );
    assert_eq!(
        platform_family("64bit Amazon Linux 2023 v6.1.0 running Node.js 18"),
        "Node.js 18"
    );
}

#[test]
fn platform_family_from_arn() {
    assert_eq!(
            platform_family(
                "arn:aws:elasticbeanstalk:us-east-1::platform/Java 17 running on 64bit Amazon Linux 2/3.5.0"
            ),
            "Java 17"
        );
}

#[test]
fn platform_version_from_stack_and_arn() {
    assert_eq!(
        platform_version("64bit Amazon Linux 2023 v6.1.0 running Node.js 20"),
        "6.1.0"
    );
    assert_eq!(
        platform_version(
            "arn:aws:elasticbeanstalk:us-east-1::platform/Python 3.11 running on 64bit Amazon Linux 2023/4.3.2"
        ),
        "4.3.2"
    );
    // Nothing to find is empty, not a guess: no version token, an
    // ARN whose last segment is not a version, and nothing at all.
    assert_eq!(
        platform_version("64bit Amazon Linux 2023 running Corretto 17"),
        ""
    );
    assert_eq!(
        platform_version("arn:aws:elasticbeanstalk:us-east-1::platform/custom-node"),
        ""
    );
    assert_eq!(platform_version(""), "");
}

#[test]
fn an_env_carries_its_platform_version_from_either_source() {
    use aws_sdk_elasticbeanstalk::types::EnvironmentDescription;
    let by_stack = map_env(
        EnvironmentDescription::builder()
            .solution_stack_name("64bit Amazon Linux 2023 v6.1.0 running Node.js 20")
            .build(),
    );
    assert_eq!(by_stack.platform_version, "6.1.0");
    // A custom-platform env reports only the ARN.
    let by_arn = map_env(
        EnvironmentDescription::builder()
            .platform_arn(
                "arn:aws:elasticbeanstalk:us-east-1::platform/Java 17 running on 64bit Amazon Linux 2/3.5.0",
            )
            .build(),
    );
    assert_eq!(by_arn.platform_version, "3.5.0");
}

#[test]
fn platform_family_handles_empty_and_unknown() {
    assert_eq!(platform_family(""), "");
    assert_eq!(platform_family("just a string"), "just a string");
}

#[test]
fn stack_family_version_splits_solution_stack() {
    assert_eq!(
        stack_family_version("64bit Amazon Linux 2023 v6.1.0 running Node.js 18"),
        Some((
            "64bit Amazon Linux 2023 running Node.js 18".to_string(),
            "6.1.0".to_string()
        ))
    );
}

#[test]
fn stack_family_version_rejects_versionless() {
    assert_eq!(stack_family_version(""), None);
    assert_eq!(stack_family_version("some platform with no version"), None);
    // A leading-v word that isn't a dotted number must not be mistaken
    // for the version token.
    assert_eq!(stack_family_version("running via vN stack"), None);
}

#[test]
fn latest_stack_versions_keeps_newest_per_family() {
    let stacks = vec![
        "64bit Amazon Linux 2 v3.1.0 running Node.js 14".to_string(),
        "64bit Amazon Linux 2 v3.10.0 running Node.js 14".to_string(),
        "64bit Amazon Linux 2 v3.2.0 running Node.js 14".to_string(),
        "64bit Amazon Linux 2023 v6.1.0 running Node.js 18".to_string(),
    ];
    let latest = latest_stack_versions(&stacks);
    assert_eq!(
        latest.get("64bit Amazon Linux 2 running Node.js 14"),
        Some(&"3.10.0".to_string())
    );
    assert_eq!(
        latest.get("64bit Amazon Linux 2023 running Node.js 18"),
        Some(&"6.1.0".to_string())
    );
}

#[test]
fn newer_stack_version_flags_only_superseded() {
    let latest =
        latest_stack_versions(&["64bit Amazon Linux 2023 v6.1.0 running Node.js 18".to_string()]);
    // Older patch → flagged.
    assert_eq!(
        newer_stack_version("64bit Amazon Linux 2023 v6.0.3 running Node.js 18", &latest),
        Some("6.1.0".to_string())
    );
    // Already current → not flagged.
    assert_eq!(
        newer_stack_version("64bit Amazon Linux 2023 v6.1.0 running Node.js 18", &latest),
        None
    );
    // Different family (Node 18 vs 20) → not flagged.
    assert_eq!(
        newer_stack_version("64bit Amazon Linux 2023 v1.0.0 running Node.js 20", &latest),
        None
    );
    // No parseable stack → not flagged.
    assert_eq!(newer_stack_version("", &latest), None);
}

#[test]
fn normalize_tier_maps_known_names() {
    assert_eq!(normalize_tier("WebServer"), "Web");
    assert_eq!(normalize_tier("Worker"), "Worker");
    assert_eq!(normalize_tier("Other"), "Other");
}

#[test]
fn summarise_instance_health_rolls_up_buckets() {
    use aws_sdk_elasticbeanstalk::types::InstanceHealthSummary;

    // Mixed: 2 ok + 1 info = 3 healthy; total adds severity buckets.
    let s = InstanceHealthSummary::builder()
        .ok(2)
        .info(1)
        .warning(1)
        .degraded(0)
        .severe(1)
        .pending(0)
        .no_data(0)
        .unknown(0)
        .build();
    let counts = super::summarise_instance_health(Some(&s));
    assert_eq!(counts.healthy, 3, "ok + info");
    assert_eq!(counts.total, 5, "ok + info + warning + degraded + severe");

    // All-Grey buckets contribute to total but not to healthy.
    let s = InstanceHealthSummary::builder()
        .pending(2)
        .no_data(1)
        .build();
    let counts = super::summarise_instance_health(Some(&s));
    assert_eq!(counts.healthy, 0);
    assert_eq!(counts.total, 3);

    // None input → 0/0 default.
    let counts = super::summarise_instance_health(None);
    assert_eq!(counts.healthy, 0);
    assert_eq!(counts.total, 0);

    // All-empty summary → 0/0 (rare in practice but defensive).
    let s = InstanceHealthSummary::builder().build();
    let counts = super::summarise_instance_health(Some(&s));
    assert_eq!(counts.healthy, 0);
    assert_eq!(counts.total, 0);
}

#[tokio::test]
async fn worker_queues_primary_error_with_empty_fallback_is_an_error() {
    // 0.27 re-review C-class: AccessDenied on the primary
    // discovery call + an Ok-but-empty fallback (the COMMON
    // autocreated-queue case — sqsd option settings are empty)
    // used to read as Ok("no queues") and silently clear DLQ
    // alerting. It must surface as Err.
    use aws_sdk_elasticbeanstalk::operation::describe_configuration_settings::DescribeConfigurationSettingsOutput;
    use aws_sdk_elasticbeanstalk::operation::describe_environment_resources::DescribeEnvironmentResourcesError;

    let der = mock!(Client::describe_environment_resources).then_error(|| {
        DescribeEnvironmentResourcesError::generic(
            aws_smithy_types::error::ErrorMetadata::builder()
                .code("AccessDenied")
                .message("not authorized")
                .build(),
        )
    });
    let dcs = mock!(Client::describe_configuration_settings)
        .then_output(|| DescribeConfigurationSettingsOutput::builder().build());
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&der, &dcs]);
    let client = client_with_eb(eb);
    let result = client.describe_worker_queues("app", "wk-env").await;
    assert!(
        result.is_err(),
        "primary error + empty fallback must be Err, got {result:?}"
    );
    assert_eq!(der.num_calls(), 1);
    assert_eq!(dcs.num_calls(), 1);
}

#[tokio::test]
async fn worker_queues_resolves_via_describe_environment_resources_when_autocreated() {
    use aws_sdk_elasticbeanstalk::operation::describe_environment_resources::DescribeEnvironmentResourcesOutput;
    use aws_sdk_elasticbeanstalk::types::{EnvironmentResourceDescription, Queue};

    let der = mock!(Client::describe_environment_resources).then_output(|| {
        DescribeEnvironmentResourcesOutput::builder()
            .environment_resources(
                EnvironmentResourceDescription::builder()
                    .queues(
                        Queue::builder()
                            .name("WorkerQueue")
                            .url("https://sqs.us-east-1.amazonaws.com/123/awseb-e-foo-queue")
                            .build(),
                    )
                    .queues(
                        Queue::builder()
                            .name("WorkerDeadLetterQueue")
                            .url("https://sqs.us-east-1.amazonaws.com/123/awseb-e-foo-queue-dlq")
                            .build(),
                    )
                    .build(),
            )
            .build()
    });
    // Provide an empty configuration-settings response — that's the
    // exact failure mode the bug fix is defending against.
    let dcs = mock!(Client::describe_configuration_settings).then_output(|| {
            aws_sdk_elasticbeanstalk::operation::describe_configuration_settings::DescribeConfigurationSettingsOutput::builder()
                .build()
        });
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&der, &dcs]);
    let client = client_with_eb(eb);

    // We can't actually fetch SQS stats without mocking SQS too, but
    // the URL resolution is the bit that regressed — assert by
    // calling the option-settings-only path that drives the same
    // logic without the stats round-trip.
    // describe_worker_queues calls queue_stats which would fail
    // against the default sqs client. Use a try-await dance to
    // observe at least the call shape via the mock's call counter.
    let _ = client.describe_worker_queues("eb-app", "eb-env").await;
    assert_eq!(
        der.num_calls(),
        1,
        "describe_environment_resources should be the primary path"
    );
}

// ── Happy-path coverage ──────────────────────────────────────────────
// Lock down the most-used path so refactors of `list_environments`
// don't silently break the table-rendering surface.

#[tokio::test]
async fn list_environments_maps_describe_environments_to_env_rows() {
    use aws_sdk_elasticbeanstalk::operation::describe_environments::DescribeEnvironmentsOutput;
    use aws_sdk_elasticbeanstalk::types::{EnvironmentDescription, EnvironmentTier};

    let de = mock!(Client::describe_environments).then_output(|| {
        DescribeEnvironmentsOutput::builder()
            .environments(
                EnvironmentDescription::builder()
                    .environment_name("api-prod")
                    .application_name("api")
                    .status("Ready".into())
                    .health("Green".into())
                    .cname("api-prod.eba.amazonaws.com")
                    .version_label("build-42")
                    .solution_stack_name("64bit Amazon Linux 2 v3.5.0 running Java 17")
                    .tier(EnvironmentTier::builder().name("WebServer").build())
                    .build(),
            )
            .build()
    });
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&de]);
    let client = client_with_eb(eb);

    let envs = client.list_environments().await.expect("ok");
    assert_eq!(envs.len(), 1);
    let e = &envs[0];
    assert_eq!(e.name, "api-prod");
    assert_eq!(e.application, "api");
    assert_eq!(e.tier, "Web", "tier normalises WebServer → Web");
    assert_eq!(e.platform, "Java 17");
    assert_eq!(e.version_label, "build-42");
}

#[tokio::test]
async fn list_application_versions_pages_through_next_token() {
    // Pins the pagination invariant: orgs with hundreds of historical
    // versions per app must see every entry in `:versions` and let
    // `:rollback` find labels that fall past the first page. Two
    // pages of mocked responses; the second carries no next_token so
    // the loop terminates. Both pages' entries must appear in the
    // returned Vec.
    use aws_sdk_elasticbeanstalk::operation::describe_application_versions::DescribeApplicationVersionsOutput;
    use aws_sdk_elasticbeanstalk::types::ApplicationVersionDescription;

    let page1 = mock!(Client::describe_application_versions)
        .match_requests(|req| req.application_name() == Some("poly") && req.next_token().is_none())
        .then_output(|| {
            DescribeApplicationVersionsOutput::builder()
                .application_versions(
                    ApplicationVersionDescription::builder()
                        .version_label("build-101")
                        .description("first")
                        .build(),
                )
                .application_versions(
                    ApplicationVersionDescription::builder()
                        .version_label("build-100")
                        .description("zeroth")
                        .build(),
                )
                .next_token("PAGE_2")
                .build()
        });
    let page2 = mock!(Client::describe_application_versions)
        .match_requests(|req| req.next_token() == Some("PAGE_2"))
        .then_output(|| {
            DescribeApplicationVersionsOutput::builder()
                .application_versions(
                    ApplicationVersionDescription::builder()
                        .version_label("build-099")
                        .description("rolled")
                        .build(),
                )
                .build()
        });
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&page1, &page2]);
    let client = client_with_eb(eb);

    let versions = client.list_application_versions("poly").await.expect("ok");
    let labels: Vec<&str> = versions.iter().map(|v| v.label.as_str()).collect();
    assert_eq!(
        labels,
        vec!["build-101", "build-100", "build-099"],
        "all three versions from both pages should be returned",
    );
    assert_eq!(page1.num_calls(), 1, "first page fetched once");
    assert_eq!(page2.num_calls(), 1, "second page fetched once");
}

// ── MultiSelect picker plumbing ─────────────────────────────────────
//
// `:subnets` / `:security-groups` rely on three helpers that all
// need to round-trip cleanly: VPC discovery via option settings,
// EC2 inventory listing filtered by VPC, and the comma-split
// helper that converts EB's CSV format to a clean Vec<String>.

#[tokio::test]
async fn fetch_env_vpc_context_pulls_vpc_id_subnets_and_sgs() {
    use aws_sdk_elasticbeanstalk::operation::describe_configuration_settings::DescribeConfigurationSettingsOutput;
    use aws_sdk_elasticbeanstalk::types::{
        ConfigurationOptionSetting, ConfigurationSettingsDescription,
    };

    let dcs = mock!(Client::describe_configuration_settings).then_output(|| {
        DescribeConfigurationSettingsOutput::builder()
            .configuration_settings(
                ConfigurationSettingsDescription::builder()
                    .option_settings(
                        ConfigurationOptionSetting::builder()
                            .namespace("aws:ec2:vpc")
                            .option_name("VPCId")
                            .value("vpc-123")
                            .build(),
                    )
                    .option_settings(
                        ConfigurationOptionSetting::builder()
                            .namespace("aws:ec2:vpc")
                            .option_name("Subnets")
                            .value("subnet-a,subnet-b")
                            .build(),
                    )
                    .option_settings(
                        ConfigurationOptionSetting::builder()
                            .namespace("aws:ec2:vpc")
                            .option_name("ELBSubnets")
                            .value("subnet-x,subnet-y")
                            .build(),
                    )
                    .option_settings(
                        ConfigurationOptionSetting::builder()
                            .namespace("aws:autoscaling:launchconfiguration")
                            .option_name("SecurityGroups")
                            .value("sg-1,sg-2,sg-3")
                            .build(),
                    )
                    // Noise — should be ignored.
                    .option_settings(
                        ConfigurationOptionSetting::builder()
                            .namespace("aws:elasticbeanstalk:application:environment")
                            .option_name("LOG_LEVEL")
                            .value("debug")
                            .build(),
                    )
                    .build(),
            )
            .build()
    });
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&dcs]);
    let client = client_with_eb(eb);

    let ctx = client
        .fetch_env_vpc_context("api", "api-prod")
        .await
        .expect("ok");
    assert_eq!(ctx.vpc_id.as_deref(), Some("vpc-123"));
    assert_eq!(ctx.subnets, vec!["subnet-a", "subnet-b"]);
    assert_eq!(ctx.elb_subnets, vec!["subnet-x", "subnet-y"]);
    assert_eq!(ctx.security_groups, vec!["sg-1", "sg-2", "sg-3"]);
}

// ── Write-path coverage ──────────────────────────────────────────────
//
// `update_env_option_settings` is the load-bearing write path —
// every `:capacity`, `:env`, `:tag`, `:subnets`, `:set-option`, etc.
// ultimately funnels through it. Pin the request-shape contract and
// the empty-input guard.

#[tokio::test]
async fn update_env_option_settings_builds_correct_request_shape() {
    use aws_sdk_elasticbeanstalk::operation::update_environment::UpdateEnvironmentOutput;
    // `match_requests` runs the closure against every captured
    // request; returning false means "no rule matched" and the
    // SDK call returns an error, which the test would then trip on.
    // So an assertion-style predicate doubles as the test body.
    let rule = mock!(Client::update_environment)
        .match_requests(|input| {
            if input.environment_name.as_deref() != Some("api-prod") {
                return false;
            }
            let options = input.option_settings();
            if options.len() != 2 {
                return false;
            }
            // Order is preserved from the caller's slice.
            if options[0].namespace.as_deref() != Some("aws:autoscaling:asg")
                || options[0].option_name.as_deref() != Some("MinSize")
                || options[0].value.as_deref() != Some("2")
            {
                return false;
            }
            if options[1].namespace.as_deref() != Some("aws:autoscaling:launchconfiguration")
                || options[1].option_name.as_deref() != Some("InstanceType")
                || options[1].value.as_deref() != Some("t3.medium")
            {
                return false;
            }
            let removes = input.options_to_remove();
            if removes.len() != 1 {
                return false;
            }
            removes[0].namespace.as_deref() == Some("aws:elasticbeanstalk:application:environment")
                && removes[0].option_name.as_deref() == Some("OLD_VAR")
        })
        .then_output(|| UpdateEnvironmentOutput::builder().build());
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&rule]);
    let client = client_with_eb(eb);

    let to_set = vec![
        (
            "aws:autoscaling:asg".to_string(),
            "MinSize".to_string(),
            "2".to_string(),
        ),
        (
            "aws:autoscaling:launchconfiguration".to_string(),
            "InstanceType".to_string(),
            "t3.medium".to_string(),
        ),
    ];
    let to_remove = vec![(
        "aws:elasticbeanstalk:application:environment".to_string(),
        "OLD_VAR".to_string(),
    )];
    client
        .update_env_option_settings("api-prod", &to_set, &to_remove)
        .await
        .expect("expected request shape to match");
    assert_eq!(rule.num_calls(), 1);
}

#[tokio::test]
async fn update_env_option_settings_rejects_empty_input_before_dispatch() {
    // If the guard fails we'd reach the mocked client, which has no
    // rules — that would also error, but with a different message.
    // The empty-input branch must short-circuit *before* any SDK call.
    use aws_sdk_elasticbeanstalk::operation::update_environment::UpdateEnvironmentOutput;
    let trip = mock!(Client::update_environment)
        .then_output(|| UpdateEnvironmentOutput::builder().build());
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&trip]);
    let client = client_with_eb(eb);

    let err = client
        .update_env_option_settings("api-prod", &[], &[])
        .await
        .expect_err("expected guard to fire");
    assert!(
        err.to_string().contains("nothing to do"),
        "expected nothing-to-do guard, got {err}"
    );
    assert_eq!(
        trip.num_calls(),
        0,
        "guard should short-circuit before any SDK call"
    );
}

#[tokio::test]
async fn update_env_option_settings_surfaces_aws_errors() {
    use aws_sdk_elasticbeanstalk::operation::update_environment::UpdateEnvironmentError;
    use aws_sdk_elasticbeanstalk::types::error::InsufficientPrivilegesException;
    let err_rule = mock!(Client::update_environment).then_error(|| {
        UpdateEnvironmentError::InsufficientPrivilegesException(
            InsufficientPrivilegesException::builder()
                .message("not authorized to call UpdateEnvironment")
                .build(),
        )
    });
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&err_rule]);
    let client = client_with_eb(eb);

    let err = client
        .update_env_option_settings(
            "api-prod",
            &[("aws:autoscaling:asg".into(), "MinSize".into(), "2".into())],
            &[],
        )
        .await
        .expect_err("expected AWS error to propagate");
    // The flatten wraps the SDK error string; we just confirm the
    // contextual prefix is present so logs are actionable.
    assert!(
        err.to_string()
            .contains("UpdateEnvironment(option_settings)"),
        "expected wrapped error context, got {err}"
    );
}

// ── Error-path coverage for the load-bearing read methods ────────────
//
// Each of these mocks the SDK to return a typed error and asserts
// our wrapper preserves the operation-name context. Future
// refactors of these methods will trip a test if they accidentally
// drop the `.map_err(|e| eyre!(...))?` prefix and start propagating
// bare SDK errors.

#[tokio::test]
async fn list_environments_throttling_error_is_recognised_by_predicate() {
    // End-to-end contract: when EB returns a Throttling-coded SDK error
    // on DescribeEnvironments, the flattened error string we surface
    // must trip `is_throttling_error` so the refresh loop installs a
    // back-off horizon instead of treating it like a normal failure.
    // Pinning this guards against an SDK / smithy change to the
    // stringification format silently breaking back-off.
    use aws_sdk_elasticbeanstalk::operation::describe_environments::DescribeEnvironmentsError;
    let rule = mock!(Client::describe_environments).then_error(|| {
        DescribeEnvironmentsError::generic(
            aws_smithy_types::error::ErrorMetadata::builder()
                .code("ThrottlingException")
                .message("Rate exceeded")
                .build(),
        )
    });
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&rule]);
    let client = client_with_eb(eb);

    let err = client
        .list_environments()
        .await
        .expect_err("expected throttling error to propagate");
    // Production path: aws error → eyre::Report → flatten_err_to_string
    // (peeks at the Debug form for SDK throttling tokens) → user-facing
    // string → is_throttling_error. Pinning this contract means a future
    // change to either end can't silently break refresh back-off.
    let s = crate::app::flatten_err_to_string(&err);
    assert!(
        crate::app::is_throttling_error(&s),
        "is_throttling_error should fire on the flattened SDK throttling string, got {s:?}"
    );
    // And the user-facing string stays readable — no Debug noise leaks.
    assert!(
        !s.contains("StatusCode") && !s.contains("Extensions"),
        "throttling toast should be clean, got {s:?}"
    );
}

#[tokio::test]
async fn list_environments_expired_token_surfaces_clean_user_message() {
    // When credentials expire mid-session, the SDK returns an
    // `ExpiredToken`-coded error. The toast should not leak Debug
    // noise (HTTP status, headers, body bytes) and must not be
    // misclassified as throttling. Pinning this guards against an
    // SDK stringification change silently turning the toast into a
    // wall of debug output or routing expired-token through the
    // throttle back-off path.
    use aws_sdk_elasticbeanstalk::operation::describe_environments::DescribeEnvironmentsError;
    let rule = mock!(Client::describe_environments).then_error(|| {
        DescribeEnvironmentsError::generic(
            aws_smithy_types::error::ErrorMetadata::builder()
                .code("ExpiredTokenException")
                .message("The security token included in the request is expired")
                .build(),
        )
    });
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&rule]);
    let client = client_with_eb(eb);

    let err = client
        .list_environments()
        .await
        .expect_err("expected expired-token error to propagate");
    let s = crate::app::flatten_err_to_string(&err);
    assert!(
        !crate::app::is_throttling_error(&s),
        "ExpiredToken should not fire the throttling predicate, got {s:?}"
    );
    assert!(
        !s.contains("StatusCode") && !s.contains("Extensions") && !s.contains("SdkBody"),
        "expired-token toast should be clean, got {s:?}"
    );
}

#[tokio::test]
async fn deploy_from_path_chain_dispatches_each_stage() {
    // End-to-end pinning of the multi-stage `:deploy --from PATH` flow:
    //   1. CreateStorageLocation (EB) → returns the managed bucket name
    //   2. PutObject (S3)             → uploads the bundle bytes
    //   3. CreateApplicationVersion   → registers the version
    //   4. UpdateEnvironment          → deploys to the env
    // Each mock asserts the input it receives matches what the previous
    // stage produced, so a future refactor that reorders / drops a stage
    // or rewires the bucket+key threading fails loud here. This is the
    // most multi-step pure-AWS code path in the project and has no other
    // automated coverage today.
    use aws_sdk_elasticbeanstalk::operation::create_application_version::CreateApplicationVersionOutput;
    use aws_sdk_elasticbeanstalk::operation::create_storage_location::CreateStorageLocationOutput;
    use aws_sdk_elasticbeanstalk::operation::update_environment::UpdateEnvironmentOutput;
    use aws_sdk_s3::operation::put_object::PutObjectOutput;

    const BUCKET: &str = "elasticbeanstalk-us-east-1-123456789012";
    const APP: &str = "poly-webapp";
    const ENV: &str = "poly-prod";
    const LABEL: &str = "build-2026-05-20-1234567890";
    const KEY: &str = "applications/poly-webapp/build-2026-05-20-1234567890";
    let bundle_bytes: Vec<u8> = b"PK\x03\x04 ... a real zip would start here".to_vec();

    let csl_rule = mock!(Client::create_storage_location).then_output(|| {
        CreateStorageLocationOutput::builder()
            .s3_bucket(BUCKET)
            .build()
    });

    // Match every PutObject; assert the bucket + key are exactly what
    // we wired upstream (regression guard for the key-threading bug).
    let put_rule = mock!(aws_sdk_s3::Client::put_object)
        .match_requests(|req| req.bucket() == Some(BUCKET) && req.key() == Some(KEY))
        .then_output(|| PutObjectOutput::builder().build());

    let cav_rule = mock!(Client::create_application_version)
        .match_requests(|req| {
            req.application_name() == Some(APP)
                && req.version_label() == Some(LABEL)
                && req.source_bundle().and_then(|s| s.s3_bucket()) == Some(BUCKET)
                && req.source_bundle().and_then(|s| s.s3_key()) == Some(KEY)
                && req.auto_create_application() == Some(false)
        })
        .then_output(|| CreateApplicationVersionOutput::builder().build());

    let upd_rule = mock!(Client::update_environment)
        .match_requests(|req| {
            req.environment_name() == Some(ENV) && req.version_label() == Some(LABEL)
        })
        .then_output(|| UpdateEnvironmentOutput::builder().build());

    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&csl_rule, &cav_rule, &upd_rule]);
    let s3 = mock_client!(aws_sdk_s3, [&put_rule]);
    let client = client_with_eb_and_s3(eb, s3);

    // Stage 1
    let bucket = client
        .create_storage_location()
        .await
        .expect("CreateStorageLocation should return the managed bucket");
    assert_eq!(bucket, BUCKET);

    // Stage 2 — write the bundle to a tempfile and let upload_bundle
    // stream it. Threshold is set very high so this exercises the
    // single-PutObject path (the multipart path is covered by a
    // separate test below).
    let tmp = std::env::temp_dir().join(format!("ebman-test-bundle-{}.zip", std::process::id()));
    std::fs::write(&tmp, &bundle_bytes).expect("write tempfile");
    let upload_res = client
        .upload_bundle_with(&bucket, KEY, &tmp, u64::MAX, 8 * 1024 * 1024)
        .await;
    let _ = std::fs::remove_file(&tmp);
    upload_res.expect("PutObject should succeed");

    // Stage 3
    client
        .create_app_version(APP, LABEL, Some("test deploy"), &bucket, KEY)
        .await
        .expect("CreateApplicationVersion should succeed");

    // Stage 4
    client
        .deploy_version(ENV, LABEL)
        .await
        .expect("UpdateEnvironment should succeed");

    // Each rule should have fired exactly once.
    assert_eq!(csl_rule.num_calls(), 1, "CreateStorageLocation");
    assert_eq!(put_rule.num_calls(), 1, "S3 PutObject");
    assert_eq!(cav_rule.num_calls(), 1, "CreateApplicationVersion");
    assert_eq!(upd_rule.num_calls(), 1, "UpdateEnvironment");
}

#[tokio::test]
async fn list_environments_surfaces_aws_errors_with_op_context() {
    use aws_sdk_elasticbeanstalk::operation::describe_environments::DescribeEnvironmentsError;
    let rule = mock!(Client::describe_environments).then_error(|| {
        DescribeEnvironmentsError::generic(
            aws_smithy_types::error::ErrorMetadata::builder()
                .code("InternalServerError")
                .message("retry later")
                .build(),
        )
    });
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&rule]);
    let client = client_with_eb(eb);

    let err = client
        .list_environments()
        .await
        .expect_err("expected AWS error to propagate");
    assert!(
        err.to_string().contains("DescribeEnvironments"),
        "expected operation context, got {err}"
    );
}

// ── DescribeEvents pagination ──────────────────────────────────────

#[tokio::test]
async fn list_events_since_follows_next_token() {
    // `:event-tail` advances its watermark past the newest event it
    // received. Dropping `next_token` meant that during a burst larger
    // than one batch, the older events behind the token were never
    // returned by any later poll — silently, with no gap marker.
    use aws_sdk_elasticbeanstalk::operation::describe_events::DescribeEventsOutput;
    use aws_sdk_elasticbeanstalk::types::EventDescription;

    fn ev(msg: &str, secs: i64) -> EventDescription {
        EventDescription::builder()
            .message(msg)
            .environment_name("api-prod")
            .event_date(aws_sdk_elasticbeanstalk::primitives::DateTime::from_secs(
                secs,
            ))
            .build()
    }
    let page1 = mock!(Client::describe_events)
        .match_requests(|req| req.next_token().is_none())
        .then_output(|| {
            DescribeEventsOutput::builder()
                .events(ev("newest", 3_000))
                .next_token("PAGE_2")
                .build()
        });
    let page2 = mock!(Client::describe_events)
        .match_requests(|req| req.next_token() == Some("PAGE_2"))
        .then_output(|| {
            DescribeEventsOutput::builder()
                .events(ev("older — behind the token", 2_000))
                .build()
        });
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&page1, &page2]);
    let client = client_with_eb(eb);

    let (events, truncated) = client.list_events_since(1_000_000, 300).await.unwrap();
    assert!(!truncated, "two pages and a clean finish is not truncated");
    let msgs: Vec<&str> = events.iter().map(|e| e.message.as_str()).collect();
    assert_eq!(
        msgs,
        vec!["newest", "older — behind the token"],
        "both pages must be returned before the watermark advances"
    );
    assert_eq!(page1.num_calls(), 1);
    assert_eq!(page2.num_calls(), 1, "next_token must be followed");
}

#[tokio::test]
async fn list_events_display_calls_do_not_paginate() {
    // The two non-watermarked callers want "the newest N" for a panel.
    // Following tokens there would just cost API calls for events
    // nobody renders, so they stay single-page.
    use aws_sdk_elasticbeanstalk::operation::describe_events::DescribeEventsOutput;
    use aws_sdk_elasticbeanstalk::types::EventDescription;

    let page1 = mock!(Client::describe_events).then_output(|| {
        DescribeEventsOutput::builder()
            .events(
                EventDescription::builder()
                    .message("newest")
                    .environment_name("api-prod")
                    .build(),
            )
            .next_token("PAGE_2")
            .build()
    });
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&page1]);
    let client = client_with_eb(eb);

    let events = client.list_events_for_env("api-prod", 100).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        page1.num_calls(),
        1,
        "a display fetch must not chase next_token"
    );
}

// ── multi-region row labelling ─────────────────────────────────────

#[test]
fn stamp_region_labels_every_row_with_the_resolved_region() {
    // Both multi-region entry points share this step. They diverged
    // once — one stamping the REQUESTED region, the other the resolved
    // one — and the difference only showed when a region string failed
    // to bind and the SDK fell back to its chain, at which point the
    // fan-out queried one region and labelled the rows with another.
    fn env(name: &str, region: Option<&str>) -> crate::aws::Environment {
        crate::aws::Environment {
            name: name.into(),
            application: "app".into(),
            status: "Ready".into(),
            health: "Green".into(),
            platform: String::new(),
            solution_stack: String::new(),
            platform_version: String::new(),
            tier: "WebServer".into(),
            cname: String::new(),
            version_label: String::new(),
            arn: None,
            updated: None,
            id: None,
            region: region.map(str::to_string),
        }
    }
    let mut envs = vec![
        env("api-prod", None),
        // A stale label from a previous pass must be overwritten, not
        // preserved.
        env("web-prod", Some("eu-west-1")),
    ];
    super::eb::stamp_region(&mut envs, "us-east-1");
    assert!(envs
        .iter()
        .all(|e| e.region.as_deref() == Some("us-east-1")));
}

#[tokio::test]
async fn single_page_event_fetch_does_not_warn_about_a_cap() {
    // `pages < max_pages` was `1 < 1` for the display callers, so every
    // ordinary fetch took the "cap reached" arm. EB returns a
    // next_token on essentially every real account, so this fired on
    // every refresh tick, every deploy poll and every `:event-tail`
    // open — burying the one warning that means something.
    //
    // Asserted through a tracing subscriber rather than by reading the
    // code, because the bug was invisible to every other test.
    use aws_sdk_elasticbeanstalk::operation::describe_events::DescribeEventsOutput;
    use aws_sdk_elasticbeanstalk::types::EventDescription;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Count(Arc<Mutex<usize>>);
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Count {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if *event.metadata().level() == tracing::Level::WARN {
                *self.0.lock().unwrap() += 1;
            }
        }
    }
    use tracing_subscriber::layer::SubscriberExt;

    let page = mock!(Client::describe_events).then_output(|| {
        DescribeEventsOutput::builder()
            .events(
                EventDescription::builder()
                    .message("newest")
                    .environment_name("api-prod")
                    .build(),
            )
            .next_token("MORE")
            .build()
    });
    let eb = mock_client!(
        aws_sdk_elasticbeanstalk,
        aws_smithy_mocks::RuleMode::MatchAny,
        [&page]
    );
    let client = client_with_eb(eb);

    // `set_default` rather than `with_default` + `block_on`: nesting a
    // second executor inside a `#[tokio::test]` parks the current-thread
    // runtime, so any tokio timer on the path would hang the suite
    // rather than fail it.
    let counter = Count::default();
    let _guard =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(counter.clone()));
    let events = client.list_events_for_env("api-prod", 100).await.unwrap();

    assert_eq!(events.len(), 1);
    assert_eq!(
        *counter.0.lock().unwrap(),
        0,
        "a deliberate single-page fetch must not warn about a cap"
    );
}

#[tokio::test]
async fn list_instances_follows_next_token() {
    // This list is `:ssm-run`'s target set and `spawn_dry_run`'s
    // blast-radius count. Truncated, the shell command silently never
    // reaches the missing instances and the overlay reports N/N
    // success against the wrong N.
    use aws_sdk_elasticbeanstalk::operation::describe_instances_health::DescribeInstancesHealthOutput;
    use aws_sdk_elasticbeanstalk::types::SingleInstanceHealth;

    fn inst(id: &str) -> SingleInstanceHealth {
        SingleInstanceHealth::builder()
            .instance_id(id)
            .health_status("Ok")
            .build()
    }
    let page1 = mock!(Client::describe_instances_health)
        .match_requests(|req| req.next_token().is_none())
        .then_output(|| {
            DescribeInstancesHealthOutput::builder()
                .instance_health_list(inst("i-aaa"))
                .next_token("P2")
                .build()
        });
    let page2 = mock!(Client::describe_instances_health)
        .match_requests(|req| req.next_token() == Some("P2"))
        .then_output(|| {
            DescribeInstancesHealthOutput::builder()
                .instance_health_list(inst("i-bbb"))
                .build()
        });
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&page1, &page2]);
    let client = client_with_eb(eb);

    let instances = client.list_instances("api-prod").await.unwrap();
    let ids: Vec<&str> = instances.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, vec!["i-aaa", "i-bbb"]);
    assert_eq!(page2.num_calls(), 1, "next_token must be followed");
}

#[tokio::test]
async fn list_instances_keeps_enhanced_health_stats() {
    // The App tab's instance rows are this call's ApplicationMetrics
    // and System sections, which used to be fetched and dropped.
    use aws_sdk_elasticbeanstalk::operation::describe_instances_health::DescribeInstancesHealthOutput;
    use aws_sdk_elasticbeanstalk::types::{ApplicationMetrics, SingleInstanceHealth};
    let rule = mock!(Client::describe_instances_health).then_output(|| {
        DescribeInstancesHealthOutput::builder()
            .instance_health_list(
                SingleInstanceHealth::builder()
                    .instance_id("i-aaa")
                    .application_metrics(
                        ApplicationMetrics::builder()
                            .duration(10)
                            .request_count(30)
                            .build(),
                    )
                    .build(),
            )
            .instance_health_list(SingleInstanceHealth::builder().instance_id("i-bbb").build())
            .build()
    });
    let client = client_with_eb(mock_client!(aws_sdk_elasticbeanstalk, [&rule]));
    let instances = client.list_instances("api-prod").await.unwrap();
    assert_eq!(
        instances[0]
            .app_stats
            .as_ref()
            .and_then(|s| s.requests_per_sec),
        Some(3.0)
    );
    assert_eq!(instances[1].app_stats, None, "basic health has none");
}

#[tokio::test]
async fn list_events_since_reports_a_truncated_window() {
    // The page cap used to be a log line only. The caller's watermark
    // advances past the newest event received, and DescribeEvents
    // returns newest-first — so what's behind the token is older and
    // unreachable by any later poll. The tail needs to know.
    use aws_sdk_elasticbeanstalk::operation::describe_events::DescribeEventsOutput;
    use aws_sdk_elasticbeanstalk::types::EventDescription;

    let endless = mock!(Client::describe_events).then_output(|| {
        DescribeEventsOutput::builder()
            .events(
                EventDescription::builder()
                    .message("busy")
                    .environment_name("api-prod")
                    .build(),
            )
            .next_token("MORE")
            .build()
    });
    let eb = mock_client!(
        aws_sdk_elasticbeanstalk,
        aws_smithy_mocks::RuleMode::MatchAny,
        [&endless]
    );
    let client = client_with_eb(eb);

    let (events, truncated) = client.list_events_since(1, 300).await.unwrap();
    assert!(truncated, "a capped window must be reported to the caller");
    assert_eq!(events.len(), 5, "one event per page, up to the cap");
}

#[test]
fn map_platform_maps_every_field_and_tolerates_absent_ones() {
    // The one new pure helper that shipped without a test — extracted
    // because two listings mapped `PlatformSummary` identically, which
    // is exactly when a silent field mix-up survives.
    use aws_sdk_elasticbeanstalk::types::{PlatformStatus, PlatformSummary};

    let full = super::eb::map_platform(
        PlatformSummary::builder()
            .platform_arn("arn:aws:elasticbeanstalk:eu-west-2::platform/Custom/1.2.3")
            .platform_branch_name("Custom running on 64bit AL2")
            .platform_version("1.2.3")
            .platform_status(PlatformStatus::Ready)
            .platform_lifecycle_state("Recommended")
            .build(),
    );
    assert_eq!(
        full.arn,
        "arn:aws:elasticbeanstalk:eu-west-2::platform/Custom/1.2.3"
    );
    assert_eq!(full.branch, "Custom running on 64bit AL2");
    assert_eq!(full.version, "1.2.3");
    assert_eq!(full.status, "Ready");
    assert_eq!(full.lifecycle, "Recommended");

    // Every field is optional in the SDK shape; absent ones must render
    // as empty rather than panic.
    let bare = super::eb::map_platform(PlatformSummary::builder().build());
    assert_eq!(bare.arn, "");
    assert_eq!(bare.branch, "");
    assert_eq!(bare.version, "");
    assert_eq!(bare.status, "");
    assert_eq!(bare.lifecycle, "");
}

// ── mutation-sweep triage, 2026-08-26 ────────────────────────────────
//
// The first complete whole-tree sweep left 105 survivors in `aws/eb.rs`.
// 75 are whole-function replacements — the SDK seam, unreachable without
// a fake client — and are tracked as their own backlog item. These cover
// the reachable logic.

/// EB reports its auto-created queues under named entries. Deleting
/// either arm survived, which means nothing checked that they are
/// recognised at all.
#[test]
fn eb_reported_queues_are_recognised_by_name() {
    let q = super::eb::QueueDiscovery::from_reported([
        ("WorkerQueue".to_string(), "https://sqs/main".to_string()),
        (
            "WorkerDeadLetterQueue".to_string(),
            "https://sqs/dlq".to_string(),
        ),
        ("SomethingElse".to_string(), "https://sqs/other".to_string()),
    ]);
    assert_eq!(q.main_url.as_deref(), Some("https://sqs/main"));
    assert_eq!(q.dlq_url.as_deref(), Some("https://sqs/dlq"));
    assert_eq!(
        q.dlq_origin,
        Some(super::eb::DlqOrigin::Reported),
        "a URL EB actually reported is Reported, not Derived"
    );
}

/// An empty URL is not an answer. EB returns entries with blank URLs,
/// and treating one as found would suppress the derivation fallback and
/// report a queue that doesn't exist.
#[test]
fn an_empty_reported_url_is_not_a_queue() {
    let q = super::eb::QueueDiscovery::from_reported([
        ("WorkerQueue".to_string(), String::new()),
        ("WorkerDeadLetterQueue".to_string(), String::new()),
    ]);
    assert_eq!(q, super::eb::QueueDiscovery::default());
    assert_eq!(q.dlq_origin, None, "no URL means no origin to claim");
}

/// The fallback fires when *either* is missing — an env can report a
/// main queue and leave the DLQ to an override, or the reverse. `&&`
/// here would skip the second API call whenever one was found.
#[test]
fn the_option_fallback_fires_on_either_gap() {
    let both = super::eb::QueueDiscovery::from_reported([
        ("WorkerQueue".to_string(), "m".to_string()),
        ("WorkerDeadLetterQueue".to_string(), "d".to_string()),
    ]);
    assert!(!both.needs_option_fallback(), "nothing missing");

    let main_only =
        super::eb::QueueDiscovery::from_reported([("WorkerQueue".to_string(), "m".to_string())]);
    assert!(main_only.needs_option_fallback(), "DLQ still missing");

    let dlq_only = super::eb::QueueDiscovery::from_reported([(
        "WorkerDeadLetterQueue".to_string(),
        "d".to_string(),
    )]);
    assert!(dlq_only.needs_option_fallback(), "main still missing");

    assert!(super::eb::QueueDiscovery::default().needs_option_fallback());
}

/// sqsd option settings fill gaps and never displace what EB reported.
#[test]
fn sqsd_settings_fill_gaps_without_overriding() {
    let setting = |name: &str, value: &str| {
        (
            "aws:elasticbeanstalk:sqsd".to_string(),
            name.to_string(),
            value.to_string(),
        )
    };

    // Gap-filling on an empty discovery.
    let mut q = super::eb::QueueDiscovery::default();
    q.apply_sqsd_settings([
        setting("WorkerQueueURL", "https://sqs/override-main"),
        setting("DeadLetterQueueURL", "https://sqs/override-dlq"),
    ]);
    assert_eq!(q.main_url.as_deref(), Some("https://sqs/override-main"));
    assert_eq!(q.dlq_url.as_deref(), Some("https://sqs/override-dlq"));
    assert_eq!(q.dlq_origin, Some(super::eb::DlqOrigin::Reported));

    // What EB reported wins.
    let mut q = super::eb::QueueDiscovery::from_reported([
        ("WorkerQueue".to_string(), "https://sqs/eb-main".to_string()),
        (
            "WorkerDeadLetterQueue".to_string(),
            "https://sqs/eb-dlq".to_string(),
        ),
    ]);
    q.apply_sqsd_settings([
        setting("WorkerQueueURL", "https://sqs/override-main"),
        setting("DeadLetterQueueURL", "https://sqs/override-dlq"),
    ]);
    assert_eq!(q.main_url.as_deref(), Some("https://sqs/eb-main"));
    assert_eq!(q.dlq_url.as_deref(), Some("https://sqs/eb-dlq"));

    // An empty override is ignored rather than blanking the field.
    let mut q = super::eb::QueueDiscovery::default();
    q.apply_sqsd_settings([
        setting("WorkerQueueURL", ""),
        setting("DeadLetterQueueURL", ""),
    ]);
    assert_eq!(q, super::eb::QueueDiscovery::default());

    // Another namespace with the same option names must not apply.
    let mut q = super::eb::QueueDiscovery::default();
    q.apply_sqsd_settings([(
        "aws:elasticbeanstalk:application:environment".to_string(),
        "WorkerQueueURL".to_string(),
        "https://sqs/wrong-namespace".to_string(),
    )]);
    assert_eq!(
        q,
        super::eb::QueueDiscovery::default(),
        "only the sqsd namespace configures the worker queue"
    );

    // An unrecognised option name in the right namespace is ignored.
    let mut q = super::eb::QueueDiscovery::default();
    q.apply_sqsd_settings([setting("HttpPath", "/work")]);
    assert_eq!(q, super::eb::QueueDiscovery::default());
}

/// Every bucket distinct and non-zero.
///
/// The existing roll-up test sets `unknown` and `degraded` to 0, so
/// flipping either `+` to `-` changed nothing — both survived. Zero is
/// the one value that hides an arithmetic mutation, and half the buckets
/// were zero.
#[test]
fn summarise_instance_health_counts_every_bucket() {
    use aws_sdk_elasticbeanstalk::types::InstanceHealthSummary;
    let s = InstanceHealthSummary::builder()
        .ok(1)
        .info(2)
        .warning(4)
        .degraded(8)
        .severe(16)
        .pending(32)
        .no_data(64)
        .unknown(128)
        .build();
    let counts = super::summarise_instance_health(Some(&s));
    // Powers of two: any dropped or negated bucket changes the sum, and
    // the amount it changes by says which one.
    assert_eq!(counts.healthy, 3, "ok(1) + info(2)");
    assert_eq!(counts.total, 255, "every bucket, 1+2+4+8+16+32+64+128");
}

/// `parts[parts.len() - 2]` is "second to last", and the sweep found it
/// interchangeable with `parts.len() / 2` — true for every three-segment
/// ARN, which is every ARN the function meets in practice. The `>= 2`
/// guard admits the two-segment case though, and there the two disagree.
#[test]
fn platform_branch_from_takes_the_second_to_last_segment() {
    assert_eq!(
        platform_branch_from("arn:aws:eb::platform/my-branch"),
        "arn:aws:eb::platform"
    );
    assert_eq!(
        platform_branch_from("arn:a/b/c/d/branch-name/1.0.0"),
        "branch-name"
    );
    // Degenerate: no `/` at all fails the `>= 2` guard.
    assert_eq!(platform_branch_from("arn:nothing"), "");
}

/// EB returns every settable key whether or not it is set, so an empty
/// value must not overwrite what a previous setting established. All
/// four guards survived the sweep.
#[test]
fn an_empty_option_value_does_not_clear_the_vpc_context() {
    let setting =
        |ns: &str, name: &str, value: &str| (ns.to_string(), name.to_string(), value.to_string());

    let ctx = super::eb::vpc_context_from_settings([
        setting("aws:ec2:vpc", "VPCId", "vpc-123"),
        setting("aws:ec2:vpc", "Subnets", "subnet-a,subnet-b"),
        setting("aws:ec2:vpc", "ELBSubnets", "subnet-c"),
        setting(
            "aws:autoscaling:launchconfiguration",
            "SecurityGroups",
            "sg-1,sg-2",
        ),
    ]);
    assert_eq!(ctx.vpc_id.as_deref(), Some("vpc-123"));
    assert_eq!(ctx.subnets, vec!["subnet-a", "subnet-b"]);
    assert_eq!(ctx.elb_subnets, vec!["subnet-c"]);
    assert_eq!(ctx.security_groups, vec!["sg-1", "sg-2"]);

    // Empty values for every key: nothing is set, and nothing panics.
    let ctx = super::eb::vpc_context_from_settings([
        setting("aws:ec2:vpc", "VPCId", ""),
        setting("aws:ec2:vpc", "Subnets", ""),
        setting("aws:ec2:vpc", "ELBSubnets", ""),
        setting("aws:autoscaling:launchconfiguration", "SecurityGroups", ""),
    ]);
    assert_eq!(ctx.vpc_id, None);
    assert!(ctx.subnets.is_empty());
    assert!(ctx.elb_subnets.is_empty());
    assert!(ctx.security_groups.is_empty());

    // The ordering case, for EVERY list field.
    //
    // This is the case that actually discriminates, and the first version
    // of this test wrote it for `Subnets` only — so the `ELBSubnets` and
    // `SecurityGroups` guards survived the verification run. For a list,
    // the all-empty case above proves nothing: `split_csv("")` yields an
    // empty vec, which is what the default already is. Only overwriting a
    // resolved value tells the two apart. `VPCId` is the exception, since
    // `Some("")` is distinguishable from `None`.
    for (ns, key, get) in [
        (
            "aws:ec2:vpc",
            "Subnets",
            (|c: &super::eb::EnvVpcContext| c.subnets.clone()) as fn(&_) -> Vec<String>,
        ),
        (
            "aws:ec2:vpc",
            "ELBSubnets",
            |c: &super::eb::EnvVpcContext| c.elb_subnets.clone(),
        ),
        (
            "aws:autoscaling:launchconfiguration",
            "SecurityGroups",
            |c: &super::eb::EnvVpcContext| c.security_groups.clone(),
        ),
    ] {
        let ctx = super::eb::vpc_context_from_settings([
            setting(ns, key, "resolved-a,resolved-b"),
            setting(ns, key, ""),
        ]);
        assert_eq!(
            get(&ctx),
            vec!["resolved-a", "resolved-b"],
            "{key}: an empty value must not blank a list already resolved"
        );
    }
}

/// `default` is the port-80 listener and sorts first; the rest go by
/// numeric port, then by option name. Both rank comparisons survived —
/// flipping either puts the default listener last.
#[test]
fn listener_rows_sort_default_first_then_by_port() {
    let row = |port: &str, opt: &str| (port.to_string(), opt.to_string(), "v".to_string());
    let mut rows = vec![
        row("8443", "SSLCertificateArns"),
        row("443", "Protocol"),
        row("default", "Protocol"),
        row("443", "DefaultProcess"),
    ];
    super::eb::sort_listener_rows(&mut rows);
    let order: Vec<(&str, &str)> = rows
        .iter()
        .map(|(p, o, _)| (p.as_str(), o.as_str()))
        .collect();
    assert_eq!(
        order,
        vec![
            ("default", "Protocol"),
            ("443", "DefaultProcess"),
            ("443", "Protocol"),
            ("8443", "SSLCertificateArns"),
        ],
        "default first, then ports ascending, then option name"
    );

    // Numeric, not lexicographic: "8443" < "9" as strings.
    let mut rows = vec![row("9", "a"), row("8443", "a")];
    super::eb::sort_listener_rows(&mut rows);
    assert_eq!(rows[0].0, "9", "ports compare as numbers");
}

/// The namespace filter is the whole filter.
///
/// Flipped to `!=`, every setting EXCEPT the ones asked for comes back
/// — `:env` would list the entire configuration vocabulary as
/// environment variables, and the RDS panel would show everything but
/// the database.
#[test]
fn settings_in_namespace_selects_only_that_namespace() {
    use super::eb::settings_in_namespace;
    let s = |ns: &str, name: &str, val: &str| {
        (
            Some(ns.to_string()),
            Some(name.to_string()),
            Some(val.to_string()),
        )
    };
    let rows = || {
        vec![
            s(
                "aws:elasticbeanstalk:application:environment",
                "LOG_LEVEL",
                "debug",
            ),
            s("aws:elasticbeanstalk:application:environment", "EMPTY", ""),
            s("aws:rds:dbinstance", "DBEngine", "postgres"),
            s("aws:rds:dbinstance", "DBUnset", ""),
            s("aws:autoscaling:asg", "MinSize", "2"),
        ]
    };

    // Env vars keep an empty value: a var set to "" IS set.
    let env = settings_in_namespace(rows(), "aws:elasticbeanstalk:application:environment", true);
    assert_eq!(
        env,
        vec![
            ("LOG_LEVEL".to_string(), "debug".to_string()),
            ("EMPTY".to_string(), String::new()),
        ],
        "only the env namespace, and an empty value is still a value"
    );

    // RDS drops them: EB returns every settable key whether or not it is
    // configured, and an unset one is not part of the database's config.
    let rds = settings_in_namespace(rows(), "aws:rds:dbinstance", false);
    assert_eq!(
        rds,
        vec![("DBEngine".to_string(), "postgres".to_string())],
        "the RDS panel drops unset keys"
    );

    // A namespace nothing is in yields nothing, rather than everything.
    assert!(settings_in_namespace(rows(), "aws:nowhere", true).is_empty());
    // A setting with no namespace is not in any namespace.
    assert!(settings_in_namespace(
        vec![(None, Some("X".into()), Some("1".into()))],
        "aws:x",
        true
    )
    .is_empty());
}

/// A tag needs both halves. Deleting the arm that requires them drops
/// every tag, and the panel renders as an env with no tags rather than
/// a fetch that lost them.
#[test]
fn tag_pairs_requires_both_halves() {
    use super::eb::tag_pairs;
    let got = tag_pairs(vec![
        (Some("Owner".into()), Some("platform".into())),
        (Some("KeyOnly".into()), None),
        (None, Some("ValueOnly".into())),
        (None, None),
        (Some("Team".into()), Some(String::new())),
    ]);
    assert_eq!(
        got,
        vec![
            ("Owner".to_string(), "platform".to_string()),
            ("Team".to_string(), String::new()),
        ],
        "both halves required; an empty value is still a value"
    );
    assert!(tag_pairs(Vec::new()).is_empty());
}

/// `newest_date` picks the newest. With `<` it picks the oldest, and
/// `:upgrade` offers a downgrade as the latest available platform.
#[test]
fn newest_date_picks_the_latest_not_the_earliest() {
    use super::eb::newest_date;
    let at = |s: &str| {
        chrono::DateTime::parse_from_rfc3339(s)
            .expect("valid")
            .with_timezone(&chrono::Utc)
    };
    let old = at("2026-01-01T00:00:00Z");
    let mid = at("2026-06-01T00:00:00Z");
    let new = at("2026-08-01T00:00:00Z");

    // Order must not matter.
    assert_eq!(newest_date(vec![old, new, mid]), Some(new));
    assert_eq!(newest_date(vec![new, mid, old]), Some(new));
    assert_eq!(newest_date(vec![mid]), Some(mid));
    assert_eq!(newest_date(Vec::new()), None, "no dates, no answer");
    // Equal dates settle on that date rather than looping or panicking.
    assert_eq!(newest_date(vec![mid, mid]), Some(mid));
    // `>` vs `>=` is EQUIVALENT here and always will be: with equal
    // values the comparison only decides which of two identical dates
    // is stored, and the result is the same either way. It shows as a
    // survivor in every sweep. Noted so the next triage skips it rather
    // than trying to write a test that cannot exist.
}

/// `list_environments` must label rows with the region it queried.
///
/// `map_env` sets `region: None` — DescribeEnvironments does not return
/// one — and `stamp_region` was called ONLY by the multi-region fan-out
/// helpers. So every other caller got `region: null`, including the MCP
/// tool that advertises the field in its own description. Found against
/// a live fleet by a client that passed `region` explicitly and got
/// null back on every row.
#[tokio::test]
async fn list_environments_labels_rows_with_the_resolved_region() {
    use aws_sdk_elasticbeanstalk::operation::describe_environments::DescribeEnvironmentsOutput;
    use aws_sdk_elasticbeanstalk::types::EnvironmentDescription;

    let rule = mock!(Client::describe_environments).then_output(|| {
        DescribeEnvironmentsOutput::builder()
            .environments(
                EnvironmentDescription::builder()
                    .environment_name("poly-prod-wk")
                    .application_name("poly")
                    .build(),
            )
            .build()
    });
    let eb = mock_client!(aws_sdk_elasticbeanstalk, [&rule]);
    // `client_with_eb` resolves to us-east-1.
    let client = client_with_eb(eb);
    let envs = client.list_environments().await.expect("ok");

    assert_eq!(envs.len(), 1);
    assert_eq!(
        envs[0].region.as_deref(),
        Some("us-east-1"),
        "a row with no region cannot be acted on region-scoped, and the \
         MCP tool advertises the field"
    );
}

#[cfg(test)]
mod eb_error_regression {
    use super::*;

    /// The EB half of the same defect SQS had. EB is the path every
    /// session uses — env listings, deploys, config reads — and its
    /// errors named the operation and dropped the reason.
    #[tokio::test]
    async fn a_denied_eb_call_names_the_missing_permission() {
        let rule = aws_smithy_mocks::mock!(Client::describe_configuration_settings).then_error(
            || {
                aws_sdk_elasticbeanstalk::operation::describe_configuration_settings::DescribeConfigurationSettingsError::generic(
                    aws_smithy_types::error::ErrorMetadata::builder()
                        .code("AccessDeniedException")
                        .message("User is not authorized to perform elasticbeanstalk:DescribeConfigurationSettings")
                        .build(),
                )
            },
        );
        let eb = aws_smithy_mocks::mock_client!(
            aws_sdk_elasticbeanstalk,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&rule]
        );
        let e = client_with_eb(eb)
            .fetch_env_option_settings("app", "env")
            .await
            .expect_err("mocked failure");
        let flat = crate::app::flatten_err_to_string(&e);

        assert!(
            flat.contains("elasticbeanstalk:DescribeConfigurationSettings"),
            "the service's sentence names WHICH permission is missing; the code \
             alone names only the family: {flat}"
        );
        assert!(flat.starts_with("AccessDenied:"), "{flat}");
    }
}
