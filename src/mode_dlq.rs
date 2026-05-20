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

use ratatui::widgets::ListState;

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
    pub purge_typed: String,
    /// Which queue is currently loaded — DLQ (default) or the main worker
    /// queue. Toggled by `m`. The same UI surfaces both; resend / purge are
    /// disabled in Main view because purging a working queue is too dangerous.
    pub viewing: QueueView,
    /// Pending single-message delete confirmation. Holds the index of the
    /// message the user pressed `x` on; `y` confirms, anything else cancels.
    pub confirm_delete_idx: Option<usize>,
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
