//! The DLQ viewer (`Mode::Dlq`).
//!
//! Moved verbatim out of `src/ui.rs` (5,046 lines) in the 0.31 split;
//! visibility widened to `pub(crate)` where a sibling needs it, and
//! nothing else changed. `use super::*` picks up the shared vocabulary
//! the way `detail.rs` and `overlays.rs` already do.

use super::*;

pub(crate) fn draw_dlq(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = app.theme.clone();
    let Some(dlq) = app.dlq.as_mut() else { return };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(area);

    // Header — adapts to which queue is currently loaded.
    let (window_title, view_label, accent) = match dlq.viewing {
        crate::app::QueueView::Main => ("Main Worker Queue", "MAIN", theme.health_yellow),
        crate::app::QueueView::Dlq => ("Dead-Letter Queue", "DLQ", theme.health_red),
    };
    let header = Paragraph::new(Line::from(vec![
        Span::styled(format!("{view_label}: "), Style::default().fg(theme.muted)),
        Span::styled(
            dlq.env_name.clone(),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled(
            format!("{} messages", dlq.messages.len()),
            Style::default().fg(theme.health_yellow),
        ),
        if dlq.confirm_delete_id.is_some() {
            Span::styled(
                format!("   {}delete this message? y / n", warn_glyph(theme.icons)),
                Style::default()
                    .fg(theme.health_red)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::raw("")
        },
    ]))
    .block(titled_block(&theme, window_title, true, accent));
    f.render_widget(header, chunks[0]);

    // Message list
    let block = rounded_block(&theme, true);
    if dlq.messages.is_empty() {
        let p = Paragraph::new(Span::styled(
            if dlq.loading {
                "loading messages…"
            } else {
                "no messages in DLQ"
            },
            Style::default().fg(theme.muted),
        ))
        .block(block);
        f.render_widget(p, chunks[1]);
    } else {
        let now = chrono::Utc::now();
        let items: Vec<ListItem> = dlq
            .messages
            .iter()
            .map(|m| {
                let age = m
                    .sent_at
                    .map(|t| humanize_age(now.signed_duration_since(t)))
                    .unwrap_or_else(|| "—".into());
                // Char-safe truncation: the body is arbitrary producer
                // data, and a byte slice at 80 panics mid-draw when a
                // multi-byte char straddles the boundary.
                let preview = truncate_for_display(m.body.lines().next().unwrap_or(""), 80);
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!(" {:<20} ", m.id),
                        Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("recv:{:<3} ", m.receive_count),
                        Style::default().fg(theme.health_yellow),
                    ),
                    Span::styled(format!("{:>5} ", age), Style::default().fg(theme.muted)),
                    Span::raw(preview),
                ]))
            })
            .collect();
        let list = List::new(items)
            .block(block)
            .highlight_style(
                Style::default()
                    .bg(theme.row_selected_bg)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(cursor_marker(&theme));
        f.render_stateful_widget(list, chunks[1], &mut dlq.list_state);
    }

    // Footer / confirm
    if dlq.confirm_purge {
        // Typed text turns green once it exactly matches the env name.
        let typed_style = Style::default()
            .fg(if dlq.purge_typed.text() == dlq.env_name.as_str() {
                theme.health_green
            } else {
                theme.text
            })
            .add_modifier(Modifier::BOLD);
        let mut spans = vec![
            Span::styled(
                " PURGE DLQ — type ",
                Style::default()
                    .fg(theme.health_red)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                dlq.env_name.clone(),
                Style::default()
                    .fg(theme.health_yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " to confirm: ",
                Style::default()
                    .fg(theme.health_red)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        spans.extend(input_caret_spans(
            dlq.purge_typed.text(),
            dlq.purge_typed.cursor_col(),
            typed_style,
            Style::default()
                .fg(theme.health_yellow)
                .add_modifier(Modifier::SLOW_BLINK),
            &theme,
        ));
        let line = Paragraph::new(Line::from(spans));
        f.render_widget(line, chunks[2]);
    } else if let Some(input) = &dlq.replay_input {
        let mut spans = vec![
            Span::styled(
                " REPLAY → main queue — ",
                Style::default()
                    .fg(theme.health_yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "all / count (20) / window (1h 24h 7d): ",
                Style::default().fg(theme.muted),
            ),
        ];
        spans.extend(input_caret_spans(
            input.text(),
            input.cursor_col(),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            Style::default()
                .fg(theme.health_yellow)
                .add_modifier(Modifier::SLOW_BLINK),
            &theme,
        ));
        let line = Paragraph::new(Line::from(spans));
        f.render_widget(line, chunks[2]);
    } else {
        let keys = match dlq.viewing {
            crate::app::QueueView::Main => {
                " MAIN  j/k move  enter view body  x delete  m → DLQ  ^R refresh  esc / q back"
            }
            crate::app::QueueView::Dlq => {
                " DLQ  j/k move  enter view body  r resend  R replay  x delete  p purge  m → MAIN  ^R refresh  esc / q back"
            }
        };
        let footer = Paragraph::new(vec![
            Line::from(match &dlq.error {
                Some(err) => Span::styled(format!(" {err}"), Style::default().fg(theme.health_red)),
                None => Span::raw(""),
            }),
            Line::from(Span::styled(keys, Style::default().fg(theme.muted))),
        ]);
        f.render_widget(footer, chunks[2]);
    }
}
