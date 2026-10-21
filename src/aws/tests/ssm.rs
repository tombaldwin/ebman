//! Tests for `aws`: SSM.

use super::*;

#[tokio::test(start_paused = true)]
async fn run_shell_command_collects_per_instance_result_on_success() {
    // Happy path: one instance, SendCommand returns a command_id,
    // first GetCommandInvocation poll returns Success with
    // stdout/stderr/exit_code. `start_paused = true` + advance()
    // skips the actual 2s sleep so the test runs in ms.
    use aws_sdk_ssm::operation::get_command_invocation::GetCommandInvocationOutput;
    use aws_sdk_ssm::operation::send_command::SendCommandOutput;
    use aws_sdk_ssm::types::{Command, CommandInvocationStatus};

    const CMD_ID: &str = "01234567-89ab-cdef-0123-456789abcdef";

    let send_rule = mock!(aws_sdk_ssm::Client::send_command)
        .match_requests(|req| {
            req.document_name() == Some("AWS-RunShellScript")
                && req.instance_ids().contains(&"i-aaa".to_string())
        })
        .then_output(|| {
            SendCommandOutput::builder()
                .command(Command::builder().command_id(CMD_ID).build())
                .build()
        });
    let poll_rule = mock!(aws_sdk_ssm::Client::get_command_invocation)
        .match_requests(|req| {
            req.command_id() == Some(CMD_ID) && req.instance_id() == Some("i-aaa")
        })
        .then_output(|| {
            GetCommandInvocationOutput::builder()
                .command_id(CMD_ID)
                .instance_id("i-aaa")
                .status(CommandInvocationStatus::Success)
                .response_code(0)
                .standard_output_content("up 3 days")
                .build()
        });
    let ssm = mock_client!(aws_sdk_ssm, [&send_rule, &poll_rule]);
    let client = client_with_ssm(ssm);

    // Background the run + advance the paused clock past the
    // first 2s sleep so the poll fires.
    let handle = tokio::spawn(async move {
        client
            .run_shell_command(&["i-aaa".to_string()], "uptime", 60)
            .await
    });
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let results = handle.await.unwrap().expect("ok");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].instance_id, "i-aaa");
    assert_eq!(results[0].status, "Success");
    assert_eq!(results[0].exit_code, 0);
    assert_eq!(results[0].stdout, "up 3 days");
    assert_eq!(results[0].stderr, "");
}

#[tokio::test(start_paused = true)]
async fn run_shell_command_synthesises_local_timeout_when_deadline_passes() {
    // If GetCommandInvocation keeps returning InProgress past the
    // wall-clock deadline, the function emits a synthetic
    // `TimedOut(local)` row for the still-pending instance rather
    // than hanging. Pinning this catches regressions in the
    // deadline-bound break of the poll loop.
    use aws_sdk_ssm::operation::get_command_invocation::GetCommandInvocationOutput;
    use aws_sdk_ssm::operation::send_command::SendCommandOutput;
    use aws_sdk_ssm::types::{Command, CommandInvocationStatus};

    const CMD_ID: &str = "deadbeef-0000-0000-0000-000000000000";

    let send_rule = mock!(aws_sdk_ssm::Client::send_command).then_output(|| {
        SendCommandOutput::builder()
            .command(Command::builder().command_id(CMD_ID).build())
            .build()
    });
    // Permanent InProgress — never resolves.
    let stuck = mock!(aws_sdk_ssm::Client::get_command_invocation).then_output(|| {
        GetCommandInvocationOutput::builder()
            .command_id(CMD_ID)
            .instance_id("i-stuck")
            .status(CommandInvocationStatus::InProgress)
            .response_code(0)
            .build()
    });
    let ssm = mock_client!(aws_sdk_ssm, [&send_rule, &stuck]);
    let client = client_with_ssm(ssm);

    let handle = tokio::spawn(async move {
        // 1s wall-clock — much shorter than the 2s poll interval
        // so we hit the deadline on the FIRST loop iteration.
        client
            .run_shell_command(&["i-stuck".to_string()], "sleep 999", 1)
            .await
    });
    // Advance well past the 1s deadline + the 2s poll interval.
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    let results = handle.await.unwrap().expect("ok");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].instance_id, "i-stuck");
    assert_eq!(
        results[0].status, "TimedOut(local)",
        "synthetic timeout row should signal which instance didn't finish"
    );
    assert_eq!(results[0].exit_code, -1);
}

// ── concurrent SSM polling keeps results attributed correctly ──────

#[tokio::test(start_paused = true)]
async fn run_shell_command_polls_instances_concurrently_without_mixing_results() {
    // The cycle now polls instances concurrently, because doing it
    // sequentially cost one round trip per instance with the deadline
    // checked only afterwards — so a large env could burn its whole
    // wall clock on one cycle and write every instance off as
    // `TimedOut(local)` while the command was running fine.
    //
    // The risk a concurrent cycle introduces is pairing the wrong
    // response to the wrong instance, so that is what this pins.
    use aws_sdk_ssm::operation::get_command_invocation::GetCommandInvocationOutput;
    use aws_sdk_ssm::operation::send_command::SendCommandOutput;
    use aws_sdk_ssm::types::{Command, CommandInvocationStatus};

    const CMD_ID: &str = "cmd-concurrent";
    let send_rule = mock!(aws_sdk_ssm::Client::send_command).then_output(|| {
        SendCommandOutput::builder()
            .command(Command::builder().command_id(CMD_ID).build())
            .build()
    });
    // One rule per instance, each returning that instance's own output.
    let mk = |id: &'static str, out: &'static str| {
        mock!(aws_sdk_ssm::Client::get_command_invocation)
            .match_requests(move |req| req.instance_id() == Some(id))
            .then_output(move || {
                GetCommandInvocationOutput::builder()
                    .command_id(CMD_ID)
                    .instance_id(id)
                    .status(CommandInvocationStatus::Success)
                    .response_code(0)
                    .standard_output_content(out)
                    .build()
            })
    };
    let a = mk("i-aaa", "host-a");
    let b = mk("i-bbb", "host-b");
    let c = mk("i-ccc", "host-c");
    let ssm = mock_client!(
        aws_sdk_ssm,
        aws_smithy_mocks::RuleMode::MatchAny,
        [&send_rule, &a, &b, &c]
    );
    let client = client_with_ssm(ssm);

    let handle = tokio::spawn(async move {
        client
            .run_shell_command(
                &[
                    "i-aaa".to_string(),
                    "i-bbb".to_string(),
                    "i-ccc".to_string(),
                ],
                "hostname",
                60,
            )
            .await
    });
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let results = handle.await.unwrap().expect("ok");

    assert_eq!(results.len(), 3, "every instance resolves in one cycle");
    // Results are sorted by instance id, so this also pins the pairing.
    let pairs: Vec<(&str, &str)> = results
        .iter()
        .map(|r| (r.instance_id.as_str(), r.stdout.as_str()))
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("i-aaa", "host-a"),
            ("i-bbb", "host-b"),
            ("i-ccc", "host-c")
        ],
        "each instance must carry its own output"
    );
    assert!(results.iter().all(|r| r.status == "Success"));
}

#[tokio::test]
async fn ssm_run_chunks_past_the_fifty_instance_cap() {
    // `SendCommand` caps `InstanceIds` at 50. The whole list went in
    // one call, so `:ssm-run` on an env with more than fifty instances
    // failed outright — on a triage path reached precisely when a large
    // env is misbehaving.
    use aws_sdk_ssm::operation::get_command_invocation::GetCommandInvocationOutput;
    use aws_sdk_ssm::operation::send_command::SendCommandOutput;
    use aws_sdk_ssm::types::{Command, CommandInvocationStatus};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let sends = std::sync::Arc::new(AtomicUsize::new(0));
    let max_ids = std::sync::Arc::new(AtomicUsize::new(0));
    let (s, m) = (sends.clone(), max_ids.clone());
    let send = mock!(aws_sdk_ssm::Client::send_command)
        .match_requests(move |req| {
            let n = req.instance_ids().len();
            m.fetch_max(n, Ordering::SeqCst);
            true
        })
        .then_output(move || {
            let n = s.fetch_add(1, Ordering::SeqCst);
            SendCommandOutput::builder()
                .command(Command::builder().command_id(format!("cmd-{n}")).build())
                .build()
        });
    let poll = mock!(aws_sdk_ssm::Client::get_command_invocation).then_output(|| {
        GetCommandInvocationOutput::builder()
            .status(CommandInvocationStatus::Success)
            .response_code(0)
            .standard_output_content("ok")
            .build()
    });
    let ssm = mock_client!(
        aws_sdk_ssm,
        aws_smithy_mocks::RuleMode::MatchAny,
        [&send, &poll]
    );
    let client = client_with_sub!(ssm = ssm);

    let ids: Vec<String> = (0..120).map(|i| format!("i-{i:04}")).collect();
    let out = client
        .run_shell_command(&ids, "uptime", 60)
        .await
        .expect("120 instances must not fail outright");

    assert_eq!(
        sends.load(Ordering::SeqCst),
        3,
        "120 instances is three SendCommand calls, not one"
    );
    assert!(
        max_ids.load(Ordering::SeqCst) <= 50,
        "no call may exceed the API's 50-instance cap, saw {}",
        max_ids.load(Ordering::SeqCst)
    );
    assert_eq!(out.len(), 120, "every instance is accounted for");
}

#[cfg(test)]
mod ssm_error_surfacing {
    use super::*;

    /// A failed `SendCommand` chunk must report the service's reason
    /// on every affected instance, not just the operation name.
    ///
    /// `aws_ctx` returns a Report whose plain `Display` is the
    /// OUTERMOST wrap — the operation name. So `format!("{e}")` after
    /// the conversion renders strictly less than the
    /// `eyre!("SendCommand failed: {e}")` it replaced, which at least
    /// reached the SDK's "service error". Found by review: the same
    /// sweep that fixed this shape in `s3.rs` missed it here.
    #[tokio::test]
    async fn a_failed_send_reports_the_reason_on_every_instance() {
        let rule = aws_smithy_mocks::mock!(SsmClient::send_command).then_error(|| {
            aws_sdk_ssm::operation::send_command::SendCommandError::generic(
                aws_smithy_types::error::ErrorMetadata::builder()
                    .code("AccessDeniedException")
                    .message("User is not authorized to perform ssm:SendCommand")
                    .build(),
            )
        });
        let ssm = aws_smithy_mocks::mock_client!(
            aws_sdk_ssm,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&rule]
        );
        let cfg = aws_config::SdkConfig::builder()
            .region(Region::new("us-east-1"))
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build();
        let c = AwsClient::for_tests(
            Client::new(&cfg),
            SqsClient::new(&cfg),
            CwClient::new(&cfg),
            CwLogsClient::new(&cfg),
            S3Client::new(&cfg),
            Ec2Client::new(&cfg),
        );
        assert!(
            c.ssm.set(ssm).is_ok(),
            "the ssm cell must be unset in a fresh test client"
        );

        // Every chunk fails, so this takes the all-failed branch —
        // the common shape for a permissions problem, and the one that
        // used to discard the detail it had just collected.
        let e = c
            .run_shell_command(&["i-0abc".to_string()], "df -h", 5)
            .await
            .expect_err("every send failed");
        let text = format!("{e:#}");
        assert!(
            text.contains("ssm:SendCommand"),
            "the operator must learn WHICH permission is missing, not just \
             that SendCommand failed: {text}"
        );
    }
}
