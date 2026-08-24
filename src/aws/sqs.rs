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
    pub receive_count: i64,
    pub sent_at: Option<DateTime<Utc>>,
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
            .await?;
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
                .send()
                .await
                .wrap_err("ReceiveMessage failed")?;
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
                out.push(QueueMessage {
                    id,
                    receipt_handle: m.receipt_handle.unwrap_or_default(),
                    body: m.body.unwrap_or_default(),
                    receive_count,
                    sent_at,
                });
                if out.len() >= target {
                    break;
                }
            }
        }
        Ok(out)
    }

    pub(crate) async fn send_message(&self, queue_url: &str, body: &str) -> Result<()> {
        self.sqs
            .send_message()
            .queue_url(queue_url)
            .message_body(body)
            .send()
            .await?;
        Ok(())
    }

    pub(crate) async fn delete_message(&self, queue_url: &str, receipt_handle: &str) -> Result<()> {
        self.sqs
            .delete_message()
            .queue_url(queue_url)
            .receipt_handle(receipt_handle)
            .send()
            .await?;
        Ok(())
    }

    pub(crate) async fn purge_queue(&self, queue_url: &str) -> Result<()> {
        self.sqs.purge_queue().queue_url(queue_url).send().await?;
        Ok(())
    }
}
