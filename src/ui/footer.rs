//! The footer: the key strip, the status line, and the health hint.
//!
//! Moved verbatim out of `src/ui.rs` (5,046 lines) in the 0.31 split;
//! visibility widened to `pub(crate)` where a sibling needs it, and
//! nothing else changed. `use super::*` picks up the shared vocabulary
//! the way `detail.rs` and `overlays.rs` already do.

use super::*;

pub(crate) fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    // First-run hint sits ABOVE the regular footer rows when this
    // is the operator's first launch (no `state.toml` on disk).
    // Clears on first input event — the renderer just reads the
    // flag every frame. Adds one row to the footer when present;
    // the layout below stays the same shape otherwise so existing
    // mode-aware logic is untouched.
    let constraints: &[Constraint] = if app.first_run_hint {
        &[
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ]
    } else {
        &[Constraint::Length(1), Constraint::Length(1)]
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints.to_vec())
        .split(area);
    let (hint_row, top_row, bottom_row) = if app.first_run_hint {
        (Some(rows[0]), rows[1], rows[2])
    } else {
        (None, rows[0], rows[1])
    };

    // First-run hint row — bright accent, single line, dismisses
    // on any input. Wording emphasises the three discovery
    // surfaces an adopter most needs to know about.
    if let Some(area) = hint_row {
        let theme = &app.theme;
        let line = Line::from(vec![
            Span::styled(
                glyph(theme.icons, "  ★ ", "  * "),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "First launch — press ",
                Style::default()
                    .fg(theme.title)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "?",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" for help, ", Style::default().fg(theme.title)),
            Span::styled(
                ":",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" for commands, ", Style::default().fg(theme.title)),
            Span::styled(
                "Ctrl-K",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " for fuzzy search.  (any key dismisses)",
                Style::default().fg(theme.title),
            ),
        ]);
        f.render_widget(Paragraph::new(line), area);
    }

    let rows = [top_row, bottom_row];

    // Top row: contextual state (filter input, command input, active filter, status/error message, or blank).
    let mut top: Vec<Span> = Vec::new();
    let theme = &app.theme;
    match app.mode {
        Mode::Filter => {
            top.push(Span::styled(
                " /",
                Style::default()
                    .fg(theme.health_yellow)
                    .add_modifier(Modifier::BOLD),
            ));
            top.push(Span::raw(" "));
            top.extend(input_caret_spans(
                app.view.filter().text(),
                app.view.filter().cursor_col(),
                Style::default().fg(theme.text),
                Style::default()
                    .fg(theme.health_yellow)
                    .add_modifier(Modifier::SLOW_BLINK),
                theme,
            ));
            top.push(Span::styled(
                "  [enter] apply  [esc] cancel",
                Style::default().fg(theme.muted),
            ));
        }
        Mode::Command => {
            top.push(Span::styled(
                " :",
                Style::default()
                    .fg(theme.title)
                    .add_modifier(Modifier::BOLD),
            ));
            top.extend(input_caret_spans(
                app.command_input.text(),
                app.command_input.cursor_col(),
                Style::default().fg(theme.text),
                Style::default()
                    .fg(theme.title)
                    .add_modifier(Modifier::SLOW_BLINK),
                theme,
            ));
            top.push(Span::styled(
                "   [enter] run  [esc] cancel",
                Style::default().fg(theme.muted),
            ));
        }
        Mode::QuickJump => {
            top.push(Span::styled(
                " '",
                Style::default()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::BOLD),
            ));
            top.push(Span::raw(" "));
            top.extend(input_caret_spans(
                app.quickjump_input.text(),
                app.quickjump_input.cursor_col(),
                Style::default().fg(app.theme.text),
                Style::default()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::SLOW_BLINK),
                theme,
            ));
            top.push(Span::styled(
                "   jump to env by name prefix   [enter] keep   [esc] cancel",
                Style::default().fg(app.theme.muted),
            ));
        }
        _ => {
            if let Some(msg) = &app.error_message {
                top.push(Span::styled(
                    format!(" {msg}"),
                    Style::default().fg(theme.health_red),
                ));
            } else if let Some(msg) = &app.status_message {
                top.push(Span::styled(
                    format!(" {msg}"),
                    Style::default().fg(theme.health_yellow),
                ));
            } else if !app.view.filter().is_empty() {
                top.push(Span::styled(
                    format!(" filter: {}", app.view.filter().text()),
                    Style::default().fg(theme.health_yellow),
                ));
            } else if let Some(hint) = context_hint(app) {
                // Context-aware nudge — only fires when the status / error
                // / filter slots are empty so it doesn't trample anything
                // the user is actively reading.
                top.push(Span::styled(
                    format!(" {}{hint}", hint_glyph(theme.icons)),
                    Style::default().fg(theme.muted),
                ));
            }
        }
    }
    f.render_widget(Paragraph::new(Line::from(top)), rows[0]);

    // Bottom row: key strip — always visible, mode-aware.
    let keys: String = match app.mode {
        Mode::Filter => " type to filter   [enter] apply   [esc] cancel".into(),
        Mode::Help => " j/k scroll   ? / esc / q   close help".into(),
        Mode::Picker => " j/k move   type to filter   [enter] select   [esc] cancel".into(),
        Mode::Command => " type a command   [enter] run   [esc] cancel  (try :help)".into(),
        Mode::QuickJump => " type env name prefix   [enter] keep selection   [esc] cancel".into(),
        Mode::Palette => " type to fuzzy-find   ↑/↓ move   [enter] run   [esc] cancel".into(),
        Mode::Normal => {
            // Focus-aware key strip: the events panel has its own navigation.
            match app.focus {
                crate::app::Focus::Events if app.event_panel.visible => {
                    " EVENTS  j/k cursor   y yank line   ^] back to table   ^E hide   esc / q".into()
                }
                _ => " j/k move  enter drill  a actions  / filter  : command  ^K palette  r region  p profile  ^R refresh  ? help  q quit".into(),
            }
        }
        Mode::Detail => match app.detail.as_ref().map(|d| d.tab()) {
            Some(crate::app::DetailTab::Instances) => {
                " INSTANCES  j/k move  enter console  s ssm shell  y yank id  x terminate  a actions  ^R refresh  ? help  esc / q back".into()
            }
            _ => " tab/shift-tab switch  j/k scroll  a actions  ^R refresh  R auto-refresh  ? help  esc / q back".into(),
        },
        Mode::Action => " j/k move  enter confirm  ? help  esc / q cancel".into(),
        Mode::Dlq => match app.dlq.as_ref().map(|d| d.viewing) {
            Some(crate::app::QueueView::Main) => {
                " MAIN  j/k move  enter view body  x delete  m → DLQ  ^R refresh  ? help  esc / q back".into()
            }
            _ => " DLQ  j/k move  enter view body  r resend  R replay  x delete  p purge  m → MAIN  ^R refresh  ? help  esc / q back".into(),
        },
        Mode::Shell => {
            // Keystrokes are forwarded to the subprocess; F12 detaches.
            " SHELL  keys → subprocess  ·  F12 detach back to ebman  ·  ^D / exit closes".into()
        }
        Mode::Form => " FORM  tab/↓↑ field  type to edit  ^S submit  esc cancel".into(),
    };
    // No Wrap — the strip is intentionally compact; longer mode-specific
    // strips that exceed one row get a horizontal scroll bar visually
    // (truncation) rather than wrapping into the body region. Mode key
    // strips are kept ≤ ~150 chars to fit standard terminals.
    // Whole hints only — see `hints_to_fit`.
    let keys = hints_to_fit(&keys, rows[1].width);
    f.render_widget(
        Paragraph::new(Span::styled(keys, Style::default().fg(theme.muted))),
        rows[1],
    );
}

/// Returns a short human recommendation when an env has been Red/Yellow for a
/// non-trivial number of consecutive samples. Counts trailing samples in the
/// env's history. Cheap; only invoked from the Detail header.
pub(crate) fn health_recommendation(env: &crate::aws::Environment, app: &App) -> Option<String> {
    let history = app.history.get(&env.name)?;
    if history.is_empty() {
        return None;
    }
    let last = history.back()?.to_lowercase();
    let is_bad = matches!(last.as_str(), "red" | "severe" | "yellow" | "warning");
    if !is_bad {
        return None;
    }
    let target = last.clone();
    let consecutive = history
        .iter()
        .rev()
        .take_while(|s| s.to_lowercase() == target)
        .count();
    // Need at least 4 consecutive (≈ 1 min at 15s tick) to be worth a callout.
    if consecutive < 4 {
        return None;
    }
    let secs = consecutive as u64 * app.refresh_interval.as_secs();
    let approx = humanize_duration(secs);
    let label = if target.eq_ignore_ascii_case("red") || target.eq_ignore_ascii_case("severe") {
        "Red"
    } else {
        "Yellow"
    };
    Some(format!("≥ {approx} in {label}"))
}
