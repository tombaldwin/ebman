//! DLQ inspection mode — viewer for the worker tier's main + dead-letter
//! SQS queues with peek / delete / purge / resend flows.
//!
//! Type cluster lives here; the methods that *use* these types
//! (`spawn_dlq_fetch`, `spawn_dlq_delete_one`, `spawn_dlq_purge`,
//! `spawn_dlq_resend_selected`, `handle_dlq_key`, `open_dlq`, `close_dlq`)
//! still sit on [`crate::app::App`] because every step reaches into the
//! shared AwsClient + status toasts + audit log. Same split rationale as
//! `mode_action.rs` — types first, App-coupled handlers later if the
//! coupling can be cleanly broken.
//!
//! Types are re-exported from `crate::app` so consumers of
//! `crate::app::DlqState` / `crate::app::QueueView` keep working without
//! a sweep across the call sites.

use chrono::{DateTime, Duration, Utc};
use ratatui::widgets::ListState;
use tui_common::TextInput;

use crate::aws::QueueMessage;

/// One in-flight DLQ session. Owned by `App.dlq` while the operator is
/// in the queue viewer (entered from Detail's Queue tab via `d`).
/// Cleared on `Esc` / `q` / context switch.
pub struct DlqState {
    pub env_name: String,
    pub main_queue_url: String,
    pub dlq_url: String,
    pub messages: Vec<QueueMessage>,
    pub list_state: ListState,
    pub loading: bool,
    pub error: Option<String>,
    pub confirm_purge: bool,
    pub purge_typed: TextInput,
    /// Which queue is currently loaded — DLQ (default) or the main worker
    /// queue. Toggled by `m`. The same UI surfaces both; resend / purge are
    /// disabled in Main view because purging a working queue is too dangerous.
    pub viewing: QueueView,
    /// Pending single-message delete confirmation. Holds the index of the
    /// message the user pressed `x` on; `y` confirms, anything else cancels.
    pub confirm_delete_idx: Option<usize>,
    /// When `Some`, the time-windowed replay prompt is open and this holds
    /// the operator's typed spec (`all` / a count / a window like `24h`).
    /// Dead-letter view only.
    pub replay_input: Option<TextInput>,
}

/// What subset of the loaded DLQ messages a replay should cover. Parsed
/// from the replay prompt by [`parse_replay_spec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplaySpec {
    /// Every message currently loaded in the viewer.
    All,
    /// The N most-recently-sent loaded messages.
    Last(usize),
    /// Loaded messages sent within the given window before "now".
    Within(Duration),
}

/// Parse the DLQ replay-prompt input into a [`ReplaySpec`]:
/// - empty or `all` → every loaded message;
/// - a bare positive integer → the N newest loaded messages;
/// - `<n><unit>` with unit `m` / `h` / `d` → messages sent within that window.
///
/// Returns `None` for anything unparseable or non-positive. Note "every
/// loaded message" means every message *currently peeked into the viewer* —
/// SQS has no cheap full-queue enumeration, so a deep DLQ replays a page.
pub fn parse_replay_spec(input: &str) -> Option<ReplaySpec> {
    let s = input.trim().to_lowercase();
    if s.is_empty() || s == "all" {
        return Some(ReplaySpec::All);
    }
    if let Ok(n) = s.parse::<usize>() {
        return (n > 0).then_some(ReplaySpec::Last(n));
    }
    let unit = s.chars().last()?;
    let num: i64 = s[..s.len() - unit.len_utf8()].parse().ok()?;
    if num <= 0 {
        return None;
    }
    let window = match unit {
        'm' => Duration::minutes(num),
        'h' => Duration::hours(num),
        'd' => Duration::days(num),
        _ => return None,
    };
    Some(ReplaySpec::Within(window))
}

/// Resolve a [`ReplaySpec`] against the loaded messages, returning the
/// indices to replay, oldest-sent first so the main queue receives them in
/// roughly chronological order. Messages with no `sent_at` are excluded
/// from a `Within` window (their age can't be confirmed).
pub fn select_replay_indices(
    messages: &[QueueMessage],
    spec: &ReplaySpec,
    now: DateTime<Utc>,
) -> Vec<usize> {
    let mut idx: Vec<usize> = match spec {
        ReplaySpec::All => (0..messages.len()).collect(),
        ReplaySpec::Last(n) => {
            let mut all: Vec<usize> = (0..messages.len()).collect();
            // Newest first; messages without a timestamp rank oldest.
            all.sort_by_key(|&i| std::cmp::Reverse(messages[i].sent_at));
            all.into_iter().take(*n).collect()
        }
        ReplaySpec::Within(window) => {
            let cutoff = now - *window;
            (0..messages.len())
                .filter(|&i| messages[i].sent_at.map(|t| t >= cutoff).unwrap_or(false))
                .collect()
        }
    };
    idx.sort_by_key(|&i| messages[i].sent_at);
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_replay_spec_covers_each_form() {
        assert_eq!(parse_replay_spec(""), Some(ReplaySpec::All));
        assert_eq!(parse_replay_spec("  all "), Some(ReplaySpec::All));
        assert_eq!(parse_replay_spec("20"), Some(ReplaySpec::Last(20)));
        assert_eq!(
            parse_replay_spec("24h"),
            Some(ReplaySpec::Within(Duration::hours(24)))
        );
        assert_eq!(
            parse_replay_spec("30m"),
            Some(ReplaySpec::Within(Duration::minutes(30)))
        );
        assert_eq!(
            parse_replay_spec("7d"),
            Some(ReplaySpec::Within(Duration::days(7)))
        );
    }

    #[test]
    fn parse_replay_spec_rejects_garbage() {
        assert_eq!(parse_replay_spec("0"), None); // non-positive count
        assert_eq!(parse_replay_spec("0h"), None); // non-positive window
        assert_eq!(parse_replay_spec("12y"), None); // unknown unit
        assert_eq!(parse_replay_spec("abc"), None);
        assert_eq!(parse_replay_spec("h"), None); // no number
    }

    fn msg(id: &str, mins_ago: Option<i64>, now: DateTime<Utc>) -> QueueMessage {
        QueueMessage {
            id: id.into(),
            receipt_handle: format!("rh-{id}"),
            body: String::new(),
            receive_count: 1,
            sent_at: mins_ago.map(|m| now - Duration::minutes(m)),
        }
    }

    #[test]
    fn select_replay_indices_all_returns_oldest_first() {
        let now = Utc::now();
        let msgs = vec![msg("a", Some(5), now), msg("b", Some(60), now)];
        // `b` is older, so it comes first.
        assert_eq!(
            select_replay_indices(&msgs, &ReplaySpec::All, now),
            vec![1, 0]
        );
    }

    #[test]
    fn select_replay_indices_last_n_picks_newest() {
        let now = Utc::now();
        let msgs = vec![
            msg("old", Some(120), now),
            msg("mid", Some(60), now),
            msg("new", Some(5), now),
        ];
        // Last(2) = the two newest (mid, new), returned oldest-first.
        assert_eq!(
            select_replay_indices(&msgs, &ReplaySpec::Last(2), now),
            vec![1, 2]
        );
    }

    #[test]
    fn select_replay_indices_within_filters_by_age_and_skips_undated() {
        let now = Utc::now();
        let msgs = vec![
            msg("recent", Some(30), now),
            msg("stale", Some(600), now),
            msg("undated", None, now),
        ];
        // Within 1h keeps only `recent`; `undated` is excluded.
        assert_eq!(
            select_replay_indices(&msgs, &ReplaySpec::Within(Duration::hours(1)), now),
            vec![0]
        );
    }
}

/// Which queue the operator is currently inspecting. The DLQ viewer
/// surfaces both the dead-letter queue and the main worker queue via the
/// same UI; `m` toggles between them. The variant gates destructive
/// operations: resend (DLQ → main) and purge are both disabled in
/// `Main` view because purging a working queue is too dangerous.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueView {
    Dlq,
    Main,
}
