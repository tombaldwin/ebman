//! Rendering. `&App` in, frame out — nothing here mutates AWS state.
//!
//! This file is the dispatcher: [`draw`] picks the layout for the
//! current [`Mode`] and hands each region to the module that owns it.
//!
//! ```text
//! chrome    blocks, pills, glyphs, colours — the shared vocabulary
//! header    the pill chain, breadcrumb, and its width arithmetic
//! table     the environments + applications tables and their cells
//! events    the events panel, severity and timestamp formatting
//! footer    key strip, status line, health hint
//! detail    the per-env Detail view (all seven tabs)
//! overlays  every `Overlay::*` — forms, text dumps, pickers
//! action    the confirm modal every destructive action passes
//! dlq       the DLQ viewer
//! shell     the embedded SSM pane
//! help      the help screen
//! ```
//!
//! The glob re-exports below keep `use super::*` resolving for every
//! sibling — the convention `detail` / `overlays` / `help` already
//! relied on — so moving an item between view modules doesn't touch
//! its callers.

// ARCHITECTURE.md rule 5, enforced by the compiler rather than by
// memory: the alternate screen swallows stdout/stderr, so a stray
// `println!` here does not appear — it corrupts the frame. Use
// `tracing::*`; output goes to ~/.cache/ebman/ebman.log.
//
// Module-scoped, so the CLI's legitimate printing is unaffected. There
// were zero violations when this went in, so it costs nothing today and
// catches the next one at compile time instead of in a review.
#![cfg_attr(
    not(test),
    deny(clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro)
)]

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        Axis, Block, BorderType, Borders, Cell, Chart, Clear, Dataset, GraphType, List, ListItem,
        Padding, Paragraph, Row, Table, Wrap,
    },
    Frame,
};

use crate::aws::Environment;
use crate::overlay::{centered_overlay, OverlaySize};
use crate::theme::{IconStyle, Theme};

use crate::app::{
    Action, ActionFlow, App, ConfirmKind, DetailTab, DisplayRow, LoadState, Mode, Overlay, Scope,
    SortKey, ToastKind, ViewMode, ACTIONS,
};

mod action;
mod chrome;
mod detail;
mod dlq;
mod events;
mod footer;
mod header;
mod help;
mod overlays;
mod shell;
mod table;

pub(crate) use action::*;
pub(crate) use chrome::*;
pub(crate) use dlq::*;
pub(crate) use events::*;
pub(crate) use footer::*;
pub(crate) use header::*;
pub(crate) use shell::*;
pub(crate) use table::*;

use detail::*;
use help::*;
use overlays::*;

/// `App::mode` is a `Copy` tag and each mode's state lives in its own
/// `Option` beside it, so `Mode::Detail` with `detail == None` is
/// representable — and means a bug.
///
/// The implication runs ONE way. Being in a mode requires its state;
/// holding the state does not require the mode, deliberately: the
/// background layer dispatches on `detail.is_some()` so that opening
/// Help or the action menu over Detail keeps Detail behind the popup
/// instead of flashing the main table. See `draw` below.
///
/// So the violation is mode-without-state, and it is worth catching
/// because of how it fails rather than how often: input routes by mode
/// while the background renders by state, so the operator gets the
/// envs table on screen with their keystrokes going to Detail
/// handlers. Nothing panics and nothing is logged — it just behaves
/// like the wrong screen.
///
/// Same treatment `ViewState::assert_fresh` gives the view cache, for
/// the same reason: the types are not enforcing this, so be loud the
/// moment it breaks. Panics in debug, logs once per surface in release
/// — a panic in the TUI is worse than a wrong frame.
///
/// Merging the state INTO `Mode` would make it unrepresentable and was
/// considered. It does not fit. `Mode` is `Copy`, and
/// `shell_return_mode = self.mode` stores one to return to on F12, so a
/// payload-carrying `Mode` would copy a 42-field `DetailState` on every
/// shell attach and force a lightweight tag to be reinvented for the
/// uses `Mode` already serves.
fn assert_mode_state_coherent(app: &App) {
    let missing = match app.mode {
        Mode::Detail if app.detail.is_none() => Some("Detail"),
        Mode::Dlq if app.dlq.is_none() => Some("Dlq"),
        Mode::Action if app.action_flow.is_none() => Some("Action"),
        Mode::Form if app.form.is_none() => Some("Form"),
        Mode::Picker if app.picker.is_none() => Some("Picker"),
        Mode::Shell if app.current_shell.is_none() => Some("Shell"),
        _ => return,
    };
    if let Some(surface) = missing {
        missing_mode_state(surface);
    }
}

pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    assert_mode_state_coherent(app);
    // Shell mode takes the whole screen; nothing else draws.
    if app.mode == Mode::Shell && app.current_shell.is_some() {
        draw_shell(f, f.area(), app);
        return;
    }
    // Background — Dlq / Detail use a full-screen alternative layout; otherwise
    // draw the main header + table + events + footer.
    //
    // The mode check used to also gate this — meaning pressing `?` or `a` in
    // Detail would temporarily render the main table behind the popup
    // because mode transitioned to Help/Action. We now use the state-Option
    // as the source of truth: if a Detail/Dlq view is open, that's the
    // background, regardless of whether a help/action/overlay modal is on
    // top of it.
    if app.dlq.is_some() {
        draw_dlq(f, f.area(), app);
    } else if app.detail.is_some() {
        draw_detail(f, f.area(), app);
    } else {
        let events_height: u16 = if app.event_panel.visible {
            app.event_panel.height
        } else {
            0
        };
        // Header rows: crumb + line1 (Account/Region/Profile) + line2
        // (Sort/Status/Envs/Last/Caller/Filter) + chain (alerts/redact/sso/
        // etc.) + optional filter-chip row. At wide-enough terminals the
        // chain merges onto line2 — `header_layout` decides per-frame.
        let (header_height, merge_pills) = header_layout(app, f.area().width);
        let mut constraints: Vec<Constraint> =
            vec![Constraint::Length(header_height), Constraint::Min(3)];
        if events_height > 0 {
            constraints.push(Constraint::Length(events_height));
        }
        // Footer is 2 rows normally (status row + key strip); the
        // first-run nudge inserts a third row above so adopters
        // see the discovery hints without the existing layout
        // shifting around once they dismiss.
        let footer_height: u16 = if app.first_run_hint { 3 } else { 2 };
        constraints.push(Constraint::Length(footer_height));

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(f.area());

        draw_header(f, chunks[0], app, merge_pills);
        match app.scope {
            Scope::Envs => draw_table(f, chunks[1], app),
            Scope::Apps => draw_apps_table(f, chunks[1], app),
        }
        if app.event_panel.visible {
            app.event_panel.area = Some(chunks[2]);
            draw_events(f, chunks[2], app);
            draw_footer(f, chunks[3], app);
        } else {
            app.event_panel.area = None;
            draw_footer(f, chunks[2], app);
        }
    }

    // Overlays and modal popups — paint on top of whichever background was
    // drawn above. Keeping these unconditional means a `D`-press from Detail
    // still surfaces the describe overlay; previously the early return swallowed it.
    if app.mode == Mode::Help {
        draw_help(f, f.area(), app);
    } else {
        // Reset the cached max so a stale value doesn't survive across
        // hides; the next help open will recompute it on the first frame.
        app.help.max_scroll = 0;
    }
    if app.mode == Mode::Picker {
        draw_picker(f, f.area(), app);
    }
    if app.mode == Mode::Action {
        draw_action(f, f.area(), app);
    }
    // WhyRed drawn separately (its renderer needs `&mut App`); every
    // other overlay matches by REFERENCE — the previous per-frame
    // `.clone()` deep-copied the whole overlay each draw, including up
    // to 2000 tail events, in exactly the busy-fleet hot path the tail
    // overlays exist for.
    if matches!(app.current_overlay, Some(Overlay::WhyRed { .. })) {
        draw_why_red_overlay(f, f.area(), app);
    } else if let Some(overlay) = app.current_overlay.as_ref() {
        match overlay {
            Overlay::Describe(text) => draw_describe(f, f.area(), app, text),
            Overlay::Whatsnew(text) => draw_whatsnew(f, f.area(), app, text),
            Overlay::History(text) => draw_history_overlay(f, f.area(), app, text),
            Overlay::Alarms { body, .. } => draw_alarms_overlay(f, f.area(), app, body),
            Overlay::Diff(text) => draw_diff_overlay(f, f.area(), app, text),
            Overlay::SavedConfigs(text) => draw_saved_configs_overlay(f, f.area(), app, text),
            Overlay::SavedConfigsInteractive {
                items,
                cursor,
                confirm_delete,
            } => draw_saved_configs_interactive(f, f.area(), app, items, *cursor, *confirm_delete),
            Overlay::TextDump { title, body } => {
                draw_text_dump_overlay(f, f.area(), app, title, body)
            }
            Overlay::LogTail { .. } => draw_log_tail_overlay(f, f.area(), app),
            Overlay::EventTail { .. } => draw_event_tail_overlay(f, f.area(), app),
            // Handled above — the renderer needs &mut App.
            Overlay::WhyRed { .. } => {}
            Overlay::AppsActionMenu {
                app_name,
                env_names,
                cursor,
            } => draw_apps_action_menu(f, f.area(), app, app_name, env_names, *cursor),
            Overlay::ReportBug { body } => draw_report_bug_overlay(f, f.area(), app, body),
            Overlay::About(opened) => draw_about(f, f.area(), app, *opened),
        }
    }
    if app.mode == Mode::Palette {
        draw_palette(f, f.area(), app);
    }
    if app.mode == Mode::Form {
        draw_form(f, f.area(), app);
    }
    // Toasts render last so they overlay everything else.
    if !app.toasts.is_empty() {
        draw_toasts(f, f.area(), app);
    }
}

/// The mode said a surface was active but its state was `None`.
///
/// `App::mode` is a `Copy` tag and each mode's state lives in its own
/// `Option` field beside it, so `Mode::Detail` with `detail == None` is
/// representable and means a bug. Every draw entry point handled it by
/// returning early — silently, so the symptom was a blank pane with
/// nothing in the log and no way to tell it from "nothing to show".
///
/// This is the same treatment `ViewState::assert_fresh` gives the view
/// cache, for the same reason: the type system is not enforcing the
/// invariant here, so the next best thing is to be loud the moment it
/// breaks rather than to paper over it. Panics in debug; logs once per
/// surface in release, because a panic in the TUI is worse than a wrong
/// frame.
///
/// Merging the state INTO `Mode` would make this unrepresentable, and
/// was considered. It does not fit: `Mode` is `Copy` and
/// `shell_return_mode = self.mode` stores one to return to on F12, so a
/// payload-carrying `Mode` would copy a 42-field `DetailState` on every
/// shell attach and force a separate lightweight tag to be reinvented
/// for exactly the uses `Mode` already serves.
pub(crate) fn missing_mode_state(surface: &'static str) {
    debug_assert!(
        false,
        "mode is {surface} but its state is None — a mode was entered \
         without its state, or the state was cleared without leaving the \
         mode"
    );
    static LOGGED: std::sync::Mutex<Option<std::collections::BTreeSet<&'static str>>> =
        std::sync::Mutex::new(None);
    if let Ok(mut g) = LOGGED.lock() {
        let seen = g.get_or_insert_with(std::collections::BTreeSet::new);
        if seen.insert(surface) {
            tracing::error!(
                surface,
                "mode is active but its state is None — rendering an empty pane"
            );
        }
    }
}

#[cfg(test)]
mod tests;
