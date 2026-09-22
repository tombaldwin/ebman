//! SQS: the worker tier's main queue and its dead-letter queue —
//! depths, message peeking, redrive and purge.

use super::*;

#[derive(Clone, Debug, Default)]
pub(crate) struct QueueStats {
    pub visible: i64,
    pub in_flight: i64,
    pub delayed: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct QueueMessage {
    pub id: String,
    pub receipt_handle: String,
    pub body: String,
    /// SQS's `ApproximateReceiveCount`. **Every** receive increments it,
    /// including ebman's own non-destructive peeks — so this is not a
    /// retry count and must not be displayed as one. Measured climbing
    /// 2 → 4 on a live DLQ message purely from being looked at.
    pub receive_count: i64,
    pub sent_at: Option<DateTime<Utc>>,
    /// The Elastic Beanstalk worker task this message carries, when it
    /// is one.
    pub task: Option<SqsdTask>,
    /// The raw custom message attributes, kept verbatim.
    ///
    /// `task` is the parsed view and is what everything reads; this is
    /// what makes a message RESTORABLE. Re-sending a body alone loses
    /// the `beanstalk.sqsd.*` attributes, and for a cron-style task
    /// that is the whole message: the body is the fixed literal
    /// "elasticbeanstalk scheduled job" and every fact about which task
    /// failed lives out here. A restore that dropped these would put
    /// back something unidentifiable and call it the message.
    ///
    /// Kept as `(name, data_type, value)` rather than re-derived from
    /// `task`, so attributes an APPLICATION set — which sqsd never
    /// wrote and this code does not know about — survive too.
    pub attributes: Vec<(String, String, String)>,
}

/// The `beanstalk.sqsd.*` attributes EB's worker daemon puts on a
/// queued task.
///
/// Exactly three exist, all `DataType: String`, confirmed against a
/// live message rather than guessed. `Body` for a cron-style task is
/// the fixed literal "elasticbeanstalk scheduled job" and carries
/// nothing — all of the signal is here, which is why a peek without
/// these attributes could say a task failed but never WHICH.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SqsdTask {
    /// `beanstalk.sqsd.task_name` — e.g. "Remove unattended jobs".
    pub name: Option<String>,
    /// `beanstalk.sqsd.path` — e.g. "/STCleanupUnattendedJobs.do".
    pub path: Option<String>,
    /// `beanstalk.sqsd.scheduled_time`, verbatim. Preformatted as
    /// `"YYYY-MM-DD HH:MM:SS UTC"` — NOT ISO-8601 and not epoch — so it
    /// is kept as sent and parsed separately. The raw string is never
    /// dropped: if the format changes, an operator should still see
    /// what EB actually said.
    pub scheduled_time_raw: Option<String>,
    /// `scheduled_time_raw` parsed, when it parses.
    pub scheduled_at: Option<DateTime<Utc>>,
}

/// Pull the `beanstalk.sqsd.*` attributes out of a received message.
///
/// `None` when the message carries none of them — an ordinary queue
/// message rather than an EB worker task. A message with SOME of them
/// still yields a task: EB is the only writer of this namespace, and
/// showing two of three fields beats showing nothing.
pub(crate) fn sqsd_task_from(
    attrs: Option<&std::collections::HashMap<String, aws_sdk_sqs::types::MessageAttributeValue>>,
) -> Option<SqsdTask> {
    let attrs = attrs?;
    let get = |k: &str| -> Option<String> {
        attrs
            .get(k)
            .and_then(|v| v.string_value.clone())
            .filter(|s| !s.is_empty())
    };
    let name = get("beanstalk.sqsd.task_name");
    let path = get("beanstalk.sqsd.path");
    let scheduled_time_raw = get("beanstalk.sqsd.scheduled_time");
    if name.is_none() && path.is_none() && scheduled_time_raw.is_none() {
        return None;
    }
    let scheduled_at = scheduled_time_raw
        .as_deref()
        .and_then(parse_sqsd_scheduled_time);
    Some(SqsdTask {
        name,
        path,
        scheduled_time_raw,
        scheduled_at,
    })
}

/// Parse `beanstalk.sqsd.scheduled_time`.
///
/// EB sends a preformatted human string with a trailing literal `UTC`
/// (`"2026-09-17 06:04:00 UTC"`), which no standard parser accepts.
/// Returns `None` rather than guessing when it does not match — the
/// caller keeps the raw string either way.
pub(crate) fn parse_sqsd_scheduled_time(raw: &str) -> Option<DateTime<Utc>> {
    chrono::NaiveDateTime::parse_from_str(raw.trim(), "%Y-%m-%d %H:%M:%S UTC")
        .ok()
        .map(|naive| naive.and_utc())
}

/// Convention-based DLQ derivation for EB-managed worker queues. EB names the
/// main queue `awseb-<env-id>-<random>` and the DLQ `awseb-<env-id>-<random>-dlq`.
/// If the main queue URL doesn't match the pattern, returns None and the caller
/// just shows no DLQ.
pub(crate) fn derive_dlq_url(main: &str) -> Option<String> {
    let trimmed = main.trim_end_matches('/');
    if trimmed.ends_with("-dlq") {
        return None;
    }
    Some(format!("{trimmed}-dlq"))
}

impl AwsClient {
    pub(crate) async fn queue_stats(&self, queue_url: &str) -> Result<QueueStats> {
        use aws_sdk_sqs::types::QueueAttributeName as Q;
        let resp = self
            .sqs
            .get_queue_attributes()
            .queue_url(queue_url)
            .attribute_names(Q::ApproximateNumberOfMessages)
            .attribute_names(Q::ApproximateNumberOfMessagesNotVisible)
            .attribute_names(Q::ApproximateNumberOfMessagesDelayed)
            .send()
            .await
            .aws_ctx("GetQueueAttributes failed")?;
        let attrs = resp.attributes.unwrap_or_default();
        let parse = |k: Q| -> i64 {
            attrs
                .get(&k)
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0)
        };
        Ok(QueueStats {
            visible: parse(Q::ApproximateNumberOfMessages),
            in_flight: parse(Q::ApproximateNumberOfMessagesNotVisible),
            delayed: parse(Q::ApproximateNumberOfMessagesDelayed),
        })
    }

    /// Peek up to `max` messages from `queue_url` with a short visibility
    /// timeout (so we don't disrupt real consumers). SQS `ReceiveMessage`
    /// returns at most 10 per call AND, because the queue is partitioned, a
    /// single call commonly returns fewer than requested even with a deep
    /// queue. We therefore loop with a short long-poll, accumulating unique
    /// messages until we hit `max`, until two consecutive calls return zero,
    /// or until the per-call budget runs out. De-duplication is by message
    /// id — a partition can return the same message across calls within the
    /// visibility-timeout window if we're slow.
    pub(crate) async fn peek_messages(
        &self,
        queue_url: &str,
        max: i32,
    ) -> Result<Vec<QueueMessage>> {
        use aws_sdk_sqs::types::MessageSystemAttributeName as M;
        let target = max.clamp(1, 100) as usize;
        let mut out: Vec<QueueMessage> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut empty_in_a_row = 0;
        // Cap total iterations so a sparse queue can't spin forever.
        for _ in 0..((target / 10).max(1) + 4) {
            if out.len() >= target {
                break;
            }
            let resp = self
                .sqs
                .receive_message()
                .queue_url(queue_url)
                .max_number_of_messages(((target - out.len()).clamp(1, 10)) as i32)
                // Visibility timeout long enough to read + dedupe across the
                // loop without holding messages back from real consumers for
                // any noticeable time.
                .visibility_timeout(5)
                // Short long-poll: SQS will wait up to 1s for messages from
                // additional partitions before returning. Trades a little
                // latency for much better recall.
                .wait_time_seconds(1)
                .message_system_attribute_names(M::ApproximateReceiveCount)
                .message_system_attribute_names(M::SentTimestamp)
                // The `beanstalk.sqsd.*` attributes are CUSTOM message
                // attributes, a different request field from the system
                // ones above — asking for the system set does not bring
                // them. Without them a peek can say a worker task
                // failed but never which one, which is the only thing
                // the operator needs. `All` rather than the three known
                // names so a task posted by an application rather than
                // by sqsd cron is not silently truncated.
                .message_attribute_names("All")
                .send()
                .await
                .aws_ctx("ReceiveMessage failed")?;
            let batch = resp.messages.unwrap_or_default();
            if batch.is_empty() {
                empty_in_a_row += 1;
                if empty_in_a_row >= 2 {
                    break;
                }
                continue;
            }
            empty_in_a_row = 0;
            for m in batch {
                let id = m.message_id.clone().unwrap_or_default();
                if !id.is_empty() && !seen.insert(id.clone()) {
                    continue;
                }
                let attrs = m.attributes.unwrap_or_default();
                let receive_count = attrs
                    .get(&M::ApproximateReceiveCount)
                    .and_then(|v| v.parse::<i64>().ok())
                    .unwrap_or(0);
                let sent_at = attrs
                    .get(&M::SentTimestamp)
                    .and_then(|v| v.parse::<i64>().ok())
                    .and_then(DateTime::from_timestamp_millis);
                let task = sqsd_task_from(m.message_attributes.as_ref());
                let attributes = m
                    .message_attributes
                    .as_ref()
                    .map(|map| {
                        map.iter()
                            .filter_map(|(k, v)| {
                                v.string_value
                                    .as_ref()
                                    .map(|sv| (k.clone(), v.data_type.clone(), sv.clone()))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                out.push(QueueMessage {
                    id,
                    receipt_handle: m.receipt_handle.unwrap_or_default(),
                    body: m.body.unwrap_or_default(),
                    attributes,
                    receive_count,
                    sent_at,
                    task,
                });
                if out.len() >= target {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Send a message, optionally carrying custom attributes back.
    ///
    /// `attrs` empty is an ordinary send. Non-empty is what makes a
    /// restore faithful: the `beanstalk.sqsd.*` attributes are where a
    /// cron task's identity lives, so a resend or an undo that dropped
    /// them would deliver something nothing could identify.
    pub(crate) async fn send_message(
        &self,
        queue_url: &str,
        body: &str,
        attrs: &[(String, String, String)],
    ) -> Result<()> {
        let mut req = self
            .sqs
            .send_message()
            .queue_url(queue_url)
            .message_body(body);
        for (name, data_type, value) in attrs {
            req = req.message_attributes(
                name,
                aws_sdk_sqs::types::MessageAttributeValue::builder()
                    .data_type(data_type)
                    .string_value(value)
                    .build()?,
            );
        }
        req.send().await.aws_ctx("SendMessage failed")?;
        Ok(())
    }

    pub(crate) async fn delete_message(&self, queue_url: &str, receipt_handle: &str) -> Result<()> {
        self.sqs
            .delete_message()
            .queue_url(queue_url)
            .receipt_handle(receipt_handle)
            .send()
            .await
            .aws_ctx("DeleteMessage failed")?;
        Ok(())
    }

    pub(crate) async fn purge_queue(&self, queue_url: &str) -> Result<()> {
        self.sqs
            .purge_queue()
            .queue_url(queue_url)
            .send()
            .await
            .aws_ctx("PurgeQueue failed")?;
        Ok(())
    }
}

#[cfg(test)]
mod sqsd_task_tests {
    use super::{parse_sqsd_scheduled_time, sqsd_task_from, SqsdTask};
    use aws_sdk_sqs::types::MessageAttributeValue;
    use std::collections::HashMap;

    fn attr(v: &str) -> MessageAttributeValue {
        MessageAttributeValue::builder()
            .data_type("String")
            .string_value(v)
            .build()
            .expect("valid attribute")
    }

    /// Built from a REAL dead-letter message captured off a live worker
    /// environment mid-incident, not from the API docs. These are the
    /// shapes EB actually sends.
    fn fixture() -> HashMap<String, MessageAttributeValue> {
        HashMap::from([
            (
                "beanstalk.sqsd.task_name".to_string(),
                attr("Remove unattended jobs"),
            ),
            (
                "beanstalk.sqsd.path".to_string(),
                attr("/STCleanupUnattendedJobs.do"),
            ),
            (
                "beanstalk.sqsd.scheduled_time".to_string(),
                attr("2026-09-17 06:04:00 UTC"),
            ),
        ])
    }

    #[test]
    fn a_real_worker_task_is_extracted_whole() {
        let t = sqsd_task_from(Some(&fixture())).expect("an EB task");
        assert_eq!(t.name.as_deref(), Some("Remove unattended jobs"));
        assert_eq!(t.path.as_deref(), Some("/STCleanupUnattendedJobs.do"));
        assert_eq!(
            t.scheduled_time_raw.as_deref(),
            Some("2026-09-17 06:04:00 UTC"),
            "the raw string must be kept exactly as sent"
        );
        assert_eq!(
            t.scheduled_at.map(|d| d.to_rfc3339()),
            Some("2026-09-17T06:04:00+00:00".to_string())
        );
    }

    /// EB sends a preformatted human string with a trailing literal
    /// `UTC` — not ISO-8601, not epoch. Every standard parser rejects
    /// it, which is the whole reason this field needs its own parse.
    #[test]
    fn the_scheduled_time_format_is_ebs_own() {
        assert!(
            parse_sqsd_scheduled_time("2026-09-17 06:04:00 UTC").is_some(),
            "the format EB actually sends must parse"
        );
        for not_it in [
            "2026-09-17T06:04:00Z",
            "2026-09-17T06:04:00+00:00",
            "1789625040",
            "1789625040068",
            "",
            "not a time at all",
        ] {
            assert!(
                parse_sqsd_scheduled_time(not_it).is_none(),
                "{not_it:?} is not EB's format and must not silently parse"
            );
        }
    }

    /// A message with none of these attributes is an ordinary queue
    /// message, not a task — it must not become an empty `SqsdTask`
    /// that renders as a blank "task:" line.
    #[test]
    fn a_plain_message_is_not_a_task() {
        assert_eq!(sqsd_task_from(None), None);
        assert_eq!(sqsd_task_from(Some(&HashMap::new())), None);

        let other = HashMap::from([("my.app.attribute".to_string(), attr("something"))]);
        assert_eq!(
            sqsd_task_from(Some(&other)),
            None,
            "a non-sqsd attribute must not make this look like an EB task"
        );
    }

    /// A partial set still yields a task: EB owns this namespace, so two
    /// fields of three is information, not corruption — and an
    /// unparseable time must not discard the raw string.
    #[test]
    fn a_partial_task_keeps_what_it_has() {
        let partial = HashMap::from([
            (
                "beanstalk.sqsd.task_name".to_string(),
                attr("Nightly sweep"),
            ),
            (
                "beanstalk.sqsd.scheduled_time".to_string(),
                attr("whenever EB feels like it"),
            ),
        ]);
        let t = sqsd_task_from(Some(&partial)).expect("still a task");
        assert_eq!(t.name.as_deref(), Some("Nightly sweep"));
        assert_eq!(t.path, None);
        assert_eq!(
            t.scheduled_time_raw.as_deref(),
            Some("whenever EB feels like it"),
            "an unparseable time must still be shown verbatim"
        );
        assert_eq!(t.scheduled_at, None);
        assert_ne!(t, SqsdTask::default(), "and must not be an empty task");
    }

    /// The peek must ASK for custom message attributes.
    ///
    /// System attributes and custom ones are different request fields:
    /// asking for `ApproximateReceiveCount` and `SentTimestamp` brings
    /// back nothing from the `beanstalk.sqsd.*` namespace. Without this
    /// line every test above still passes and every message arrives
    /// with no task — a peek that can say a worker task failed but
    /// never which one, which is exactly the gap a live incident hit.
    ///
    /// Source-scanned because the call needs SQS. The slice stops at
    /// the test module so the guard cannot match the needle in its own
    /// body.
    #[test]
    fn the_peek_requests_custom_message_attributes() {
        let src = std::fs::read_to_string("src/aws/sqs.rs").expect("read own source");
        let prod = crate::app::tests::scan::production_half(&src);
        assert!(
            prod.contains("receive_message()"),
            "the scan is not finding the peek at all"
        );
        assert!(
            prod.contains(".message_attribute_names("),
            "the peek must request custom attributes, or no message ever \
             carries a task"
        );
    }
}
