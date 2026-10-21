//! Tests for `aws`: SQS worker queues.

use super::*;

#[test]
fn derive_dlq_url_appends_suffix() {
    assert_eq!(
        derive_dlq_url("https://sqs.us-east-1.amazonaws.com/123/awseb-e-foo-queue"),
        Some("https://sqs.us-east-1.amazonaws.com/123/awseb-e-foo-queue-dlq".to_string())
    );
}

#[test]
fn derive_dlq_url_skips_already_dlq() {
    assert_eq!(
        derive_dlq_url("https://sqs.us-east-1.amazonaws.com/123/foo-dlq"),
        None
    );
}

#[test]
fn derive_dlq_url_strips_trailing_slash() {
    assert_eq!(
        derive_dlq_url("https://sqs.us-east-1.amazonaws.com/123/foo/"),
        Some("https://sqs.us-east-1.amazonaws.com/123/foo-dlq".to_string())
    );
}

// ── Regression #2 ────────────────────────────────────────────────────
// `peek_messages` originally made a single `ReceiveMessage` call —
// but SQS may return fewer than the requested batch on any one call
// (it's a maximum, not a guarantee). The fix loops with short long-
// polling, dedupes by message id across iterations, and bails after
// two empty batches in a row.

#[tokio::test]
async fn peek_messages_loops_and_dedupes_across_batches() {
    use aws_sdk_sqs::operation::receive_message::ReceiveMessageOutput;
    use aws_sdk_sqs::types::Message;

    // First call returns 2 messages, second call returns 1 (including
    // a duplicate of msg-1), third returns empty, fourth returns
    // empty → loop should exit. Expect 3 unique messages.
    fn msg(id: &'static str) -> Message {
        Message::builder().message_id(id).body(id).build()
    }
    let rule = mock!(aws_sdk_sqs::Client::receive_message)
        .sequence()
        .output(|| {
            ReceiveMessageOutput::builder()
                .messages(msg("msg-1"))
                .messages(msg("msg-2"))
                .build()
        })
        .output(|| {
            ReceiveMessageOutput::builder()
                .messages(msg("msg-1")) // dup
                .messages(msg("msg-3"))
                .build()
        })
        .output(|| ReceiveMessageOutput::builder().build())
        .output(|| ReceiveMessageOutput::builder().build())
        .build();
    let sqs = mock_client!(aws_sdk_sqs, [&rule]);
    let client = client_with_sqs(sqs);

    let out = client
        .peek_messages("https://sqs.us-east-1.amazonaws.com/123/q", 10)
        .await
        .expect("peek should succeed");
    let ids: Vec<String> = out.iter().map(|m| m.id.clone()).collect();
    assert_eq!(ids, vec!["msg-1", "msg-2", "msg-3"]);
}

/// `send_message` puts the attributes on the wire.
///
/// The source scan in `writes.rs` proves every call SITE passes them.
/// This proves the function then sends them — a `send_message` that
/// accepted attributes and quietly dropped them would satisfy the scan
/// completely, and a mutation replacing this body with `Ok(())` did
/// exactly that until now.
///
/// It matters because of what the attributes are: for a cron-style
/// task the body is a fixed literal and `beanstalk.sqsd.*` carries the
/// entire identity of the message.
#[tokio::test]
async fn send_message_puts_attributes_on_the_wire() {
    use aws_sdk_sqs::operation::send_message::SendMessageOutput;

    let rule = mock!(aws_sdk_sqs::Client::send_message)
        .match_requests(|req| {
            let attrs = req.message_attributes();
            req.message_body() == Some("elasticbeanstalk scheduled job")
                && attrs.is_some_and(|m| {
                    m.get("beanstalk.sqsd.task_name")
                        .and_then(|v| v.string_value())
                        == Some("Remove unattended jobs")
                        && m.get("beanstalk.sqsd.path").map(|v| v.data_type()) == Some("String")
                })
        })
        .then_output(|| SendMessageOutput::builder().build());
    let client = client_with_sqs(mock_client!(aws_sdk_sqs, [&rule]));

    let attrs = vec![
        (
            "beanstalk.sqsd.task_name".to_string(),
            "String".to_string(),
            "Remove unattended jobs".to_string(),
        ),
        (
            "beanstalk.sqsd.path".to_string(),
            "String".to_string(),
            "/STCleanupUnattendedJobs.do".to_string(),
        ),
    ];
    client
        .send_message(
            "https://sqs.us-east-1.amazonaws.com/123/q",
            "elasticbeanstalk scheduled job",
            &attrs,
        )
        .await
        .expect("send");

    // The rule only matches a request carrying both attributes with
    // the right type and value, so reaching here at all is the
    // assertion. `num_calls` makes that explicit rather than implicit.
    assert_eq!(
        rule.num_calls(),
        1,
        "the send must have happened, and matched"
    );
}

/// A peek retains the raw custom attributes, not just the parsed task.
///
/// `task` is the view everything reads; `attributes` is what makes a
/// message restorable. All three resend paths shipped sending the body
/// alone, and for a cron-style task that is total loss — the body is
/// the fixed literal "elasticbeanstalk scheduled job" and every fact
/// about which task it was lives in these attributes.
///
/// Pinned here rather than through the demo fixture, which supplies
/// its own attributes and so cannot see the SDK-side retention at all.
#[tokio::test]
async fn peek_messages_retains_raw_attributes_for_a_restore() {
    use aws_sdk_sqs::operation::receive_message::ReceiveMessageOutput;
    use aws_sdk_sqs::types::{Message, MessageAttributeValue};

    let attr = |v: &str| {
        MessageAttributeValue::builder()
            .data_type("String")
            .string_value(v)
            .build()
            .expect("attr")
    };
    let rule = mock!(aws_sdk_sqs::Client::receive_message)
        .sequence()
        .output(move || {
            ReceiveMessageOutput::builder()
                .messages(
                    Message::builder()
                        .message_id("m-1")
                        .body("elasticbeanstalk scheduled job")
                        .message_attributes(
                            "beanstalk.sqsd.task_name",
                            attr("Remove unattended jobs"),
                        )
                        .message_attributes(
                            "beanstalk.sqsd.path",
                            attr("/STCleanupUnattendedJobs.do"),
                        )
                        .message_attributes("x-app-tenant", attr("acme"))
                        .build(),
                )
                .build()
        })
        .output(|| ReceiveMessageOutput::builder().build())
        .output(|| ReceiveMessageOutput::builder().build())
        .build();
    let client = client_with_sqs(mock_client!(aws_sdk_sqs, [&rule]));

    let out = client
        .peek_messages("https://sqs.us-east-1.amazonaws.com/123/q", 10)
        .await
        .expect("peek");
    let m = out.first().expect("one message");

    let names: Vec<&str> = m.attributes.iter().map(|(n, _, _)| n.as_str()).collect();
    assert!(names.contains(&"beanstalk.sqsd.task_name"), "{names:?}");
    assert!(names.contains(&"beanstalk.sqsd.path"), "{names:?}");
    // An attribute the APPLICATION set, which sqsd never wrote and
    // this code knows nothing about. Re-deriving from the parsed
    // `task` would silently drop it.
    assert!(
        names.contains(&"x-app-tenant"),
        "attributes are kept verbatim so app-set ones survive: {names:?}"
    );
    assert_eq!(
        m.attributes
            .iter()
            .find(|(n, _, _)| n == "beanstalk.sqsd.task_name")
            .map(|(_, t, v)| (t.as_str(), v.as_str())),
        Some(("String", "Remove unattended jobs")),
        "type and value are kept, since a send must reproduce both"
    );
    // The parsed view still works — this adds to it, it does not
    // replace it.
    assert_eq!(
        m.task.as_ref().and_then(|t| t.name.as_deref()),
        Some("Remove unattended jobs")
    );
}

#[tokio::test]
async fn peek_messages_stops_after_two_empty_batches() {
    use aws_sdk_sqs::operation::receive_message::ReceiveMessageOutput;
    // Sequence returns empty twice — should stop without exhausting
    // the call cap.
    let rule = mock!(aws_sdk_sqs::Client::receive_message)
        .sequence()
        .output(|| ReceiveMessageOutput::builder().build())
        .output(|| ReceiveMessageOutput::builder().build())
        // If we reach this, the stop-on-two-empty guard is broken.
        .output(|| {
            ReceiveMessageOutput::builder()
                .messages(
                    aws_sdk_sqs::types::Message::builder()
                        .message_id("late")
                        .body("late")
                        .build(),
                )
                .build()
        })
        .build();
    let sqs = mock_client!(aws_sdk_sqs, [&rule]);
    let client = client_with_sqs(sqs);

    let out = client
        .peek_messages("https://sqs.us-east-1.amazonaws.com/123/q", 10)
        .await
        .expect("peek should succeed");
    assert!(
        out.is_empty(),
        "should have stopped before consuming the 'late' message"
    );
    assert_eq!(
        rule.num_calls(),
        2,
        "exactly two empty-batch calls should terminate the loop"
    );
}

#[tokio::test]
async fn peek_messages_surfaces_sqs_errors_with_op_context() {
    use aws_sdk_sqs::operation::receive_message::ReceiveMessageError;
    let rule = mock!(aws_sdk_sqs::Client::receive_message).then_error(|| {
        ReceiveMessageError::generic(
            aws_smithy_types::error::ErrorMetadata::builder()
                .code("QueueDoesNotExist")
                .message("queue gone")
                .build(),
        )
    });
    let sqs = mock_client!(aws_sdk_sqs, [&rule]);
    let cfg = aws_config::SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    let client = AwsClient::for_tests(
        Client::new(&cfg),
        sqs,
        CwClient::new(&cfg),
        CwLogsClient::new(&cfg),
        S3Client::new(&cfg),
        Ec2Client::new(&cfg),
    );

    let err = client
        .peek_messages("https://sqs.us-east-1.amazonaws.com/123/q", 5)
        .await
        .expect_err("expected SQS error to propagate");
    assert!(
        err.to_string().contains("ReceiveMessage"),
        "expected operation context, got {err}"
    );
}

/// The task attributes, end to end through the SDK.
///
/// `sqsd_task_from` was tested against a hand-built map and the request
/// line was pinned by a source guard — but nothing connected the two.
/// This drives `peek_messages` against a mocked SQS that only answers
/// when `MessageAttributeNames` is present, so the request AND the
/// extraction are covered by the same test.
#[tokio::test]
async fn peek_messages_asks_for_and_extracts_the_sqsd_task() {
    use aws_sdk_sqs::operation::receive_message::ReceiveMessageOutput;
    use aws_sdk_sqs::types::{Message, MessageAttributeValue};

    let attr = |v: &str| {
        MessageAttributeValue::builder()
            .data_type("String")
            .string_value(v)
            .build()
            .expect("valid attribute")
    };

    let rule = mock!(SqsClient::receive_message)
        // The assertion that matters: the request must ASK for custom
        // attributes. Without this the mock never matches and the call
        // fails — which is exactly what happens against real SQS, only
        // silently, as a message with no task.
        .match_requests(|req| req.message_attribute_names().iter().any(|n| n == "All"))
        .then_output(move || {
            ReceiveMessageOutput::builder()
                .messages(
                    Message::builder()
                        .message_id("m-1")
                        .receipt_handle("rh-1")
                        .body("elasticbeanstalk scheduled job")
                        .message_attributes(
                            "beanstalk.sqsd.task_name",
                            attr("Remove unattended jobs"),
                        )
                        .message_attributes(
                            "beanstalk.sqsd.path",
                            attr("/STCleanupUnattendedJobs.do"),
                        )
                        .message_attributes(
                            "beanstalk.sqsd.scheduled_time",
                            attr("2026-09-17 06:04:00 UTC"),
                        )
                        .build(),
                )
                .build()
        });
    let sqs =
        aws_smithy_mocks::mock_client!(aws_sdk_sqs, aws_smithy_mocks::RuleMode::MatchAny, [&rule]);
    let client = client_with_sqs(sqs);

    let msgs = client
        .peek_messages("https://sqs/q-dlq", 1)
        .await
        .expect("the request must carry MessageAttributeNames");
    assert_eq!(msgs.len(), 1);
    let task = msgs[0]
        .task
        .as_ref()
        .expect("an EB worker task must be extracted from the attributes");
    assert_eq!(task.name.as_deref(), Some("Remove unattended jobs"));
    assert_eq!(task.path.as_deref(), Some("/STCleanupUnattendedJobs.do"));
    assert_eq!(
        task.scheduled_at.map(|d| d.to_rfc3339()),
        Some("2026-09-17T06:04:00+00:00".to_string()),
        "and EB's own time format must parse on the way through"
    );
}

/// An ordinary queue message is not a task.
///
/// The peek must not manufacture an empty `SqsdTask` for a message that
/// carries none of the attributes — it would render as a blank "task:"
/// line in the DLQ viewer and in `why`.
#[tokio::test]
async fn peek_messages_leaves_a_plain_message_without_a_task() {
    use aws_sdk_sqs::operation::receive_message::ReceiveMessageOutput;
    use aws_sdk_sqs::types::Message;

    let rule = mock!(SqsClient::receive_message).then_output(|| {
        ReceiveMessageOutput::builder()
            .messages(
                Message::builder()
                    .message_id("m-2")
                    .receipt_handle("rh-2")
                    .body("ORDER-4471-RETRY")
                    .build(),
            )
            .build()
    });
    let sqs =
        aws_smithy_mocks::mock_client!(aws_sdk_sqs, aws_smithy_mocks::RuleMode::MatchAny, [&rule]);
    let client = client_with_sqs(sqs);

    let msgs = client.peek_messages("https://sqs/q", 1).await.expect("ok");
    assert_eq!(msgs.len(), 1);
    assert!(
        msgs[0].task.is_none(),
        "a message with no sqsd attributes must have no task"
    );
    assert_eq!(msgs[0].body, "ORDER-4471-RETRY");
}

/// An SQS failure must name the action AWS refused, not just its class.
///
/// `SdkError`'s `Display` for a modelled service failure is the literal
/// string **"service error"**. Every SQS call took it with a bare `?`,
/// so an operator denied a DLQ delete by an IAM policy saw
/// `AccessDenied: service error` — the class right, and the one
/// sentence naming the missing permission thrown away by the boundary.
///
/// Measured before the fix, not assumed: the probe printed exactly
/// that string.
#[cfg(test)]
mod sqs_error_surfacing {
    use super::*;

    fn client_with_sqs(sqs: SqsClient) -> AwsClient {
        let cfg = aws_config::SdkConfig::builder()
            .region(Region::new("us-east-1"))
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build();
        AwsClient::for_tests(
            Client::new(&cfg),
            sqs,
            CwClient::new(&cfg),
            CwLogsClient::new(&cfg),
            S3Client::new(&cfg),
            Ec2Client::new(&cfg),
        )
    }

    fn denied_delete_client() -> AwsClient {
        let rule = aws_smithy_mocks::mock!(SqsClient::delete_message).then_error(|| {
            aws_sdk_sqs::operation::delete_message::DeleteMessageError::generic(
                aws_smithy_types::error::ErrorMetadata::builder()
                    .code("AccessDeniedException")
                    .message("User is not authorized to perform sqs:DeleteMessage")
                    .build(),
            )
        });
        client_with_sqs(aws_smithy_mocks::mock_client!(
            aws_sdk_sqs,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&rule]
        ))
    }

    #[tokio::test]
    async fn a_denied_delete_names_the_action_and_the_reason() {
        let c = denied_delete_client();
        let e = c
            .delete_message("https://sqs/q", "rh")
            .await
            .expect_err("mocked failure");
        let flat = crate::app::flatten_err_to_string(&e);

        assert!(
            flat.contains("sqs:DeleteMessage"),
            "the service's own sentence is the only thing that says WHICH \
             permission is missing: {flat}"
        );
        assert!(
            flat.starts_with("AccessDenied:"),
            "the class still leads, so existing routing is unaffected: {flat}"
        );
        assert!(
            flat.contains("DeleteMessage failed"),
            "and the operation is named: {flat}"
        );
        assert!(
            !flat.contains("service error"),
            "`service error` is the SDK Display this exists to replace: {flat}"
        );
    }

    /// A code we do not classify must still beat the operation name
    /// alone — the old path fell through to the `Debug` sniff and
    /// surfaced nothing.
    #[tokio::test]
    async fn an_unclassified_code_still_reaches_the_operator() {
        let rule = aws_smithy_mocks::mock!(SqsClient::delete_message).then_error(|| {
            aws_sdk_sqs::operation::delete_message::DeleteMessageError::generic(
                aws_smithy_types::error::ErrorMetadata::builder()
                    .code("ReceiptHandleIsInvalid")
                    .message("The receipt handle has expired")
                    .build(),
            )
        });
        let c = client_with_sqs(aws_smithy_mocks::mock_client!(
            aws_sdk_sqs,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&rule]
        ));
        let e = c
            .delete_message("https://sqs/q", "rh")
            .await
            .expect_err("mocked failure");
        let flat = crate::app::flatten_err_to_string(&e);
        assert!(flat.contains("ReceiptHandleIsInvalid"), "{flat}");
        assert!(flat.contains("The receipt handle has expired"), "{flat}");
    }
}

/// A worker env with no real dead-letter queue must report "no DLQ",
/// not fail the whole discovery.
///
/// Driven through the SDK's TYPED `QueueDoesNotExist` variant, which
/// is what SQS actually deserialises. Every existing fixture for this
/// builds the error with `::generic`, producing the `Unhandled`
/// variant — a shape production never sees, and one whose rendering
/// happens to contain the error code that the old substring check was
/// looking for. So the fixtures passed while the production path
/// returned `Err` for every worker env whose derived `<main>-dlq`
/// guess missed.
///
/// Measured at HEAD before the fix: the chain WITHOUT the metadata
/// layer reads `service error: QueueDoesNotExist: …` and contains no
/// `NonExistentQueue` anywhere.
#[cfg(test)]
mod typed_queue_does_not_exist {
    use super::*;

    #[tokio::test]
    async fn a_missing_dlq_is_reported_as_absent_not_as_a_failure() {
        let rule = aws_smithy_mocks::mock!(SqsClient::get_queue_attributes).then_error(|| {
            aws_sdk_sqs::operation::get_queue_attributes::GetQueueAttributesError::QueueDoesNotExist(
                aws_sdk_sqs::types::error::QueueDoesNotExist::builder()
                    .message("The specified queue does not exist.")
                    .meta(
                        aws_smithy_types::error::ErrorMetadata::builder()
                            .code("AWS.SimpleQueueService.NonExistentQueue")
                            .message("The specified queue does not exist.")
                            .build(),
                    )
                    .build(),
            )
        });
        let sqs = aws_smithy_mocks::mock_client!(
            aws_sdk_sqs,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&rule]
        );
        let cfg = aws_config::SdkConfig::builder()
            .region(Region::new("us-east-1"))
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build();
        let c = AwsClient::for_tests(
            Client::new(&cfg),
            sqs,
            CwClient::new(&cfg),
            CwLogsClient::new(&cfg),
            S3Client::new(&cfg),
            Ec2Client::new(&cfg),
        );

        let e = c.queue_stats("https://sqs/q").await.expect_err("mocked");
        assert_eq!(
            crate::aws::error_code(&e),
            Some("AWS.SimpleQueueService.NonExistentQueue"),
            "the typed variant must still carry its code through the boundary, \
             because that is what the discovery swallow now reads: {e:#}"
        );
        // Sanity: this really is the typed variant. `::generic`
        // produces `Unhandled`, whose Display is
        // `unhandled error ({code})` — and it is precisely because
        // that rendering contains the code that the old substring
        // check passed in tests while failing in production.
        let chain = format!("{e:#}");
        assert!(chain.contains("QueueDoesNotExist"), "{chain}");
        assert!(!chain.contains("unhandled error"), "{chain}");
    }
}
