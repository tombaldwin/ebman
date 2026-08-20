//! Shared state + key surface for the two streaming tail overlays —
//! `:logs-tail` (`Overlay::LogTail`) and `:event-tail`
//! (`Overlay::EventTail`). Extracted (0.26, queued since the 0.25
//! pre-tag review) so the twins stop drifting: the interactive
//! surface (scroll / follow / regex filter) and the poller-teardown
//! contract live once, here; the overlays keep only their
//! stream-specific fields (log group, event buffers, watermarks).

use crossterm::event::{KeyCode, KeyEvent};
use tui_common::TextInput;

/// The interactive state every tail overlay shares: scroll offset
/// (0 = pinned to tail), follow flag, and the `/`-filter machinery.
/// Embedded in `Overlay::LogTail` / `Overlay::EventTail` as `view`.
#[derive(Debug, Clone)]
pub struct TailView {
    /// Rows scrolled up from the tail. 0 with `following` = pinned.
    pub scroll: u16,
    /// Snap to the newest line when new data lands. Cleared by
    /// scrolling up; restored by `G` / scrolling back to the tail.
    pub following: bool,
    pub filter_input: TextInput,
    /// True while the footer is in filter-entry mode ( `/` pressed,
    /// Enter/Esc not yet). Printable keys go to `filter_input`.
    pub filter_active: bool,
    /// Compiled on Enter; `None` = no filter (or the regex failed to
    /// compile — invalid patterns clear rather than erroring).
    pub filter_pattern: Option<regex::Regex>,
}

impl Default for TailView {
    fn default() -> Self {
        Self::new()
    }
}

impl TailView {
    pub fn new() -> Self {
        Self {
            scroll: 0,
            following: true,
            filter_input: TextInput::new(),
            filter_active: false,
            filter_pattern: None,
        }
    }
}

/// What [`handle_tail_key`] decided. `Close` means the operator
/// dismissed the overlay — the caller must reap the polling task
/// (see [`reap_tail_task`]) and drop the overlay; the view itself
/// can't reach the task handle.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TailKeyOutcome {
    /// Esc / q outside filter mode: dismiss the overlay.
    Close,
    /// Key handled (or ignored) within the view.
    Consumed,
}

/// The shared tail key surface: j/k scroll, g/G jump, `/` filter
/// entry (Esc cancels, Enter compiles), `n` clear-filter, esc/q
/// close. Pure over the view — overlay-specific keys (LogTail's Tab
/// group-switcher) are handled by the caller before delegating.
pub(crate) fn handle_tail_key(view: &mut TailView, key: KeyEvent) -> TailKeyOutcome {
    if view.filter_active {
        match key.code {
            KeyCode::Esc => {
                view.filter_active = false;
                view.filter_input.clear();
                view.filter_pattern = None;
            }
            KeyCode::Enter => {
                view.filter_active = false;
                if view.filter_input.is_empty() {
                    view.filter_pattern = None;
                } else {
                    view.filter_pattern = regex::RegexBuilder::new(view.filter_input.text())
                        .case_insensitive(true)
                        .build()
                        .ok();
                }
            }
            // TextInput consumes editing keys (cursor / Ctrl-W);
            // the regex is compiled on Enter.
            _ => {
                view.filter_input.handle_key(key);
            }
        }
        return TailKeyOutcome::Consumed;
    }
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => return TailKeyOutcome::Close,
        KeyCode::Char('j') | KeyCode::Down => {
            if view.scroll > 0 {
                view.scroll -= 1;
            }
            if view.scroll == 0 {
                view.following = true;
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            view.scroll = view.scroll.saturating_add(1);
            view.following = false;
        }
        KeyCode::Char('G') | KeyCode::End => {
            view.scroll = 0;
            view.following = true;
        }
        KeyCode::Char('g') | KeyCode::Home => {
            view.scroll = u16::MAX;
            view.following = false;
        }
        KeyCode::Char('/') => {
            view.filter_active = true;
            view.filter_input.clear();
            view.filter_pattern = None;
        }
        KeyCode::Char('n') => {
            view.filter_input.clear();
            view.filter_pattern = None;
        }
        _ => {}
    }
    TailKeyOutcome::Consumed
}

/// The poller-teardown contract, in one place: abort the task (if
/// any) and bump the session id so any already-queued message from
/// the aborted task — including a late `*Opened` that would re-open
/// a dismissed overlay — is dropped at the session guard. Every
/// tail teardown site (close key, re-spawn, context switch, the
/// overlay-replaced reap in msg.rs) must go through here.
pub(crate) fn reap_tail_task(task: &mut Option<tokio::task::JoinHandle<()>>, session: &mut u64) {
    if let Some(handle) = task.take() {
        handle.abort();
    }
    *session = session.wrapping_add(1);
}

/// Pure window math shared by both tail renderers: index of the
/// first visible line given the post-filter line count, the body
/// height, and the view's follow/scroll state. Following pins to
/// the tail; paused honours `scroll` rows up from the tail,
/// saturating at the top.
pub(crate) fn tail_window_start(total: usize, body_rows: usize, view: &TailView) -> usize {
    let max_start = total.saturating_sub(body_rows);
    if view.following {
        max_start
    } else {
        max_start.saturating_sub(view.scroll as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn scroll_keys_toggle_following() {
        let mut v = TailView::new();
        assert!(v.following);
        assert_eq!(
            handle_tail_key(&mut v, key(KeyCode::Char('k'))),
            TailKeyOutcome::Consumed
        );
        assert_eq!(v.scroll, 1);
        assert!(!v.following, "scrolling up pauses follow");
        handle_tail_key(&mut v, key(KeyCode::Char('j')));
        assert_eq!(v.scroll, 0);
        assert!(v.following, "scrolling back to the tail resumes follow");
        handle_tail_key(&mut v, key(KeyCode::Char('g')));
        assert_eq!(v.scroll, u16::MAX);
        assert!(!v.following);
        handle_tail_key(&mut v, key(KeyCode::Char('G')));
        assert_eq!(v.scroll, 0);
        assert!(v.following);
    }

    #[test]
    fn filter_mode_swallows_keys_and_compiles_on_enter() {
        let mut v = TailView::new();
        handle_tail_key(&mut v, key(KeyCode::Char('/')));
        assert!(v.filter_active);
        // In filter mode, `q` is input, not close.
        assert_eq!(
            handle_tail_key(&mut v, key(KeyCode::Char('q'))),
            TailKeyOutcome::Consumed
        );
        handle_tail_key(&mut v, key(KeyCode::Enter));
        assert!(!v.filter_active);
        let pat = v.filter_pattern.as_ref().expect("compiled on Enter");
        assert!(pat.is_match("Quiet"), "case-insensitive");
        // `n` clears the filter.
        handle_tail_key(&mut v, key(KeyCode::Char('n')));
        assert!(v.filter_pattern.is_none());
        // Esc cancels filter entry without compiling.
        handle_tail_key(&mut v, key(KeyCode::Char('/')));
        handle_tail_key(&mut v, key(KeyCode::Esc));
        assert!(!v.filter_active && v.filter_pattern.is_none());
    }

    #[test]
    fn invalid_regex_clears_rather_than_errors() {
        let mut v = TailView::new();
        handle_tail_key(&mut v, key(KeyCode::Char('/')));
        handle_tail_key(&mut v, key(KeyCode::Char('(')));
        handle_tail_key(&mut v, key(KeyCode::Enter));
        assert!(v.filter_pattern.is_none());
    }

    #[test]
    fn close_only_outside_filter_mode() {
        let mut v = TailView::new();
        assert_eq!(
            handle_tail_key(&mut v, key(KeyCode::Esc)),
            TailKeyOutcome::Close
        );
        assert_eq!(
            handle_tail_key(&mut v, key(KeyCode::Char('q'))),
            TailKeyOutcome::Close
        );
    }

    #[test]
    fn window_start_follows_or_honours_scroll() {
        let mut v = TailView::new();
        // Following: pinned to the tail regardless of scroll.
        assert_eq!(tail_window_start(100, 20, &v), 80);
        assert_eq!(tail_window_start(10, 20, &v), 0, "short buffer starts at 0");
        // Paused: scroll rows up from the tail, saturating at the top.
        v.following = false;
        v.scroll = 30;
        assert_eq!(tail_window_start(100, 20, &v), 50);
        v.scroll = u16::MAX;
        assert_eq!(tail_window_start(100, 20, &v), 0);
    }
}
