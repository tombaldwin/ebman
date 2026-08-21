//! Overlay / popup renderers: form, palette, toasts, saved-configs,
//! tails, about, why-red, describe/diff/history, picker — carved out of the 9,400-line `ui.rs` root (0.27
//! architecture pass, the same `app/` submodule pattern). Items are
//! `pub(super)`; the root glob-imports them so call sites and tests
//! are untouched. Shared chrome helpers (blocks, pills, glyphs,
//! `centered_overlay`) stay in the root and reach here via
//! `use super::*`.

use super::*;

pub(super) fn draw_form(f: &mut Frame, area: Rect, app: &mut App) {
    use crate::form::{FieldKind, FormState};
    let Some(form) = app.form.as_mut() else {
        return;
    };
    let popup = centered_overlay(OverlaySize::Text, area);
    f.render_widget(Clear, popup);
    let theme = &app.theme;
    let outer = titled_block(theme, &form.title, true, theme.title_alt);
    let inner = outer.inner(popup);
    f.render_widget(outer, popup);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // env target banner
            Constraint::Length(1), // separator
            Constraint::Min(1),    // fields
            Constraint::Length(1), // footer hint
        ])
        .split(inner);

    // LocalConfig forms (`:settings`) don't have an AWS target; show the
    // config file path instead so the operator knows where the submit
    // will land.
    let banner = if form.env_name.is_empty() {
        format!(" file: {}", crate::config::config_path().display())
    } else {
        format!(" target: {}", form.env_name)
    };
    f.render_widget(
        Paragraph::new(Span::styled(banner, Style::default().fg(theme.muted))),
        chunks[0],
    );
    f.render_widget(
        Paragraph::new(Span::styled(
            "─".repeat(inner.width as usize),
            Style::default().fg(theme.muted),
        )),
        chunks[1],
    );

    if form.state == FormState::Loading {
        f.render_widget(
            Paragraph::new(Span::styled(
                "  loading current values from AWS…",
                Style::default().fg(theme.muted),
            )),
            chunks[2],
        );
    } else {
        // Build the field rows. Each field takes 2-3 lines: label/value
        // row, optional help, optional error.
        let max_label = form
            .fields
            .iter()
            .map(|fld| fld.label.chars().count())
            .max()
            .unwrap_or(0)
            .clamp(8, 32);
        let mut lines: Vec<Line> = Vec::new();
        // Line index of the focused field's row (or focused MultiSelect
        // option row) — drives the cursor-follow below so a form taller
        // than the popup (9-field :asg-trigger on 80x24) scrolls
        // instead of letting Tab move focus below the fold.
        let mut cursor_line: Option<usize> = None;
        for (i, fld) in form.fields.iter().enumerate() {
            let is_cursor = i == form.cursor;
            if is_cursor {
                cursor_line = Some(lines.len());
            }
            let pointer = if is_cursor {
                glyph(theme.icons, "▶ ", "> ")
            } else {
                "  "
            };
            let label_style = if is_cursor {
                Style::default()
                    .fg(theme.title_alt)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text)
            };
            let value_style = if is_cursor {
                Style::default().fg(theme.text).bg(theme.row_selected_bg)
            } else {
                Style::default().fg(theme.text)
            };
            // Render the value per kind.
            let value_text: String = match &fld.kind {
                FieldKind::Text | FieldKind::Integer { .. } => {
                    if is_cursor {
                        format!("{}_", fld.value)
                    } else {
                        fld.value.clone()
                    }
                }
                FieldKind::Boolean => {
                    if fld.value == "true" {
                        "[x] true".to_string()
                    } else {
                        "[ ] false".to_string()
                    }
                }
                FieldKind::Select { options } => {
                    // ◀ value ▶ when focused; just value otherwise.
                    let _ = options; // currently unused; keeps the type
                    if is_cursor {
                        format!(
                            "{} {} {}",
                            glyph(theme.icons, "◀", "<"),
                            fld.value,
                            glyph(theme.icons, "▶", ">")
                        )
                    } else {
                        fld.value.clone()
                    }
                }
                FieldKind::MultiSelect { options } => {
                    // Value row shows a one-line summary; the full option
                    // list is rendered below on its own lines.
                    let n_selected = crate::form::parse_multi_value(&fld.value).len();
                    format!("({n_selected} / {} selected)", options.len())
                }
            };
            // Trailing in-line validation marker: a single ✗ glyph in
            // health_red next to the value when the field is invalid.
            // The full error message still renders on its own line below;
            // the marker is the eye-catcher that lets the operator scan
            // for the bad field without reading every help line.
            let mut row_spans = vec![
                Span::styled(pointer.to_string(), Style::default().fg(theme.accent)),
                Span::styled(
                    format!("{:<width$}  ", fld.label, width = max_label),
                    label_style,
                ),
                Span::styled(value_text, value_style),
            ];
            if fld.error.is_some() {
                row_spans.push(Span::styled(
                    glyph(theme.icons, "  ✗", "  x"),
                    Style::default()
                        .fg(theme.health_red)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            lines.push(Line::from(row_spans));
            // MultiSelect: render the full option list below the value
            // summary. Each row shows `[x] {opt}` or `[ ] {opt}`; if the
            // field carries `option_annotations`, the matching entry is
            // appended in muted text on the same line. The row at
            // `option_cursor` gets the same row_selected_bg treatment
            // the table uses for the focused row.
            if let FieldKind::MultiSelect { options } = &fld.kind {
                let annotations = fld.option_annotations.as_deref();
                for (idx, opt) in options.iter().enumerate() {
                    let selected = crate::form::is_multi_selected(&fld.value, opt);
                    let marker = if selected { "[x]" } else { "[ ]" };
                    let row_is_cursor = is_cursor && idx == fld.option_cursor;
                    if row_is_cursor {
                        cursor_line = Some(lines.len());
                    }
                    let row_style = if row_is_cursor {
                        Style::default().fg(theme.text).bg(theme.row_selected_bg)
                    } else if selected {
                        Style::default()
                            .fg(theme.title_alt)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme.text)
                    };
                    let annot = annotations
                        .and_then(|a| a.get(idx))
                        .map(|s| s.as_str())
                        .unwrap_or("");
                    let row_spans = if annot.is_empty() {
                        vec![Span::styled(format!("     {marker} {opt}"), row_style)]
                    } else {
                        vec![
                            Span::styled(format!("     {marker} {opt}  "), row_style),
                            Span::styled(annot.to_string(), Style::default().fg(theme.muted)),
                        ]
                    };
                    lines.push(Line::from(row_spans));
                }
            }
            if let Some(help) = &fld.help {
                lines.push(Line::from(Span::styled(
                    format!("     {help}"),
                    Style::default().fg(theme.muted),
                )));
            }
            if let Some(err) = &fld.error {
                lines.push(Line::from(Span::styled(
                    format!("     {}{err}", warn_glyph(theme.icons)),
                    Style::default().fg(theme.health_red),
                )));
            }
        }
        form.scroll = config_scroll_follow(
            form.scroll,
            cursor_line,
            chunks[2].height as usize,
            lines.len(),
        );
        // No .wrap: cursor-follow counts LOGICAL lines, and wrapped
        // rows above the cursor would make the follow undershoot
        // (long help text on a narrow popup put the focused field
        // below the fold again). Truncation matches the events panel.
        f.render_widget(Paragraph::new(lines).scroll((form.scroll, 0)), chunks[2]);
    }
    let footer = match form.state {
        FormState::Loading => " esc to cancel",
        FormState::Submitting => " submitting…",
        FormState::Ready => " tab field · ↓↑ field-or-option · type to edit · space toggle · ←→ cycle select · ^S submit · esc cancel",
    };
    f.render_widget(
        Paragraph::new(Span::styled(footer, Style::default().fg(theme.muted))),
        chunks[3],
    );
}

pub(super) fn draw_palette(f: &mut Frame, area: Rect, app: &App) {
    let popup = centered_overlay(OverlaySize::Picker, area);
    f.render_widget(Clear, popup);
    let theme = &app.theme;
    // Single frame around the whole palette (input + list + footer). The
    // inner layout splits the interior with no internal borders, so the
    // popup reads as one visually-unified widget rather than three stacked
    // boxes.
    let outer = titled_block(theme, "palette", true, theme.title_alt);
    let inner = outer.inner(popup);
    f.render_widget(outer, popup);
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // input
            Constraint::Length(1), // separator
            Constraint::Min(1),    // list
            Constraint::Length(1), // hint
        ])
        .split(inner);

    // Input bar (no border — drawn directly inside the outer frame).
    let mut input_spans = vec![Span::styled(
        " ❯ ",
        Style::default()
            .fg(theme.title_alt)
            .add_modifier(Modifier::BOLD),
    )];
    input_spans.extend(input_caret_spans(
        app.palette_input.text(),
        app.palette_input.cursor_col(),
        Style::default().fg(theme.text),
        Style::default()
            .fg(theme.title_alt)
            .add_modifier(Modifier::SLOW_BLINK),
        theme,
    ));
    let input = Paragraph::new(Line::from(input_spans));
    f.render_widget(input, layout[0]);

    // Thin horizontal rule between input and list.
    f.render_widget(
        Paragraph::new(Span::styled(
            "─".repeat(inner.width as usize),
            Style::default().fg(theme.muted),
        )),
        layout[1],
    );

    // Item list
    let items: Vec<ListItem> = app
        .palette_filtered
        .iter()
        .filter_map(|i| app.palette_items.get(*i))
        .map(|it| {
            let tag_color = match it.kind_tag {
                "cmd" => theme.title,
                "env" => theme.text,
                "view" => theme.title_alt,
                "plugin" => theme.accent,
                _ => theme.muted,
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {:<6}", it.kind_tag),
                    Style::default().fg(tag_color).add_modifier(Modifier::DIM),
                ),
                Span::styled(
                    format!("{:<32}", it.label),
                    Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                ),
                Span::styled(it.detail.clone(), Style::default().fg(theme.muted)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(theme.row_selected_bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(cursor_marker(theme));
    let mut state = app.palette_state.clone();
    f.render_stateful_widget(list, layout[2], &mut state);

    // Hint footer
    let hint_count = app.palette_filtered.len();
    let total = app.palette_items.len();
    let hint = Paragraph::new(Span::styled(
        format!(
            " {}/{} matches   ↑/↓ move   ⏎ run   esc cancel",
            hint_count, total,
        ),
        Style::default().fg(theme.muted),
    ));
    f.render_widget(hint, layout[3]);
}

pub(super) fn draw_toasts(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let width: u16 = 50;
    let toast_h: u16 = 3;
    // Stack from bottom-right; newest at the bottom.
    let n = app.toasts.len() as u16;
    let total_h = n * toast_h;
    if area.height < total_h + 2 || area.width < width + 2 {
        return;
    }
    let x = area.x + area.width.saturating_sub(width + 2);
    let mut y = area.y + area.height.saturating_sub(total_h + 2);
    for t in &app.toasts {
        let rect = Rect {
            x,
            y,
            width,
            height: toast_h,
        };
        // Severity drives all three of: glyph, border colour, title text.
        // Glyph picks vary by icon style so the toast stays readable when
        // the user's font doesn't have Nerd / Powerline glyphs.
        let (border_color, label, glyph) = match (t.kind, theme.icons) {
            (ToastKind::Info, IconStyle::Powerline) => (theme.title, "info", "\u{f05a}"),
            (ToastKind::Success, IconStyle::Powerline) => (theme.health_green, "ok", "\u{f058}"),
            (ToastKind::Error, IconStyle::Powerline) => (theme.health_red, "error", "\u{f057}"),
            (ToastKind::Info, IconStyle::Unicode) => (theme.title, "info", "ⓘ"),
            (ToastKind::Success, IconStyle::Unicode) => (theme.health_green, "ok", "✓"),
            (ToastKind::Error, IconStyle::Unicode) => (theme.health_red, "error", "✗"),
            (ToastKind::Info, IconStyle::Ascii) => (theme.title, "info", "i"),
            (ToastKind::Success, IconStyle::Ascii) => (theme.health_green, "ok", "+"),
            (ToastKind::Error, IconStyle::Ascii) => (theme.health_red, "error", "!"),
        };
        let block = rounded_block(theme, true)
            .border_style(Style::default().fg(border_color))
            .title(Span::styled(
                format!(" {glyph} {label} "),
                Style::default()
                    .fg(border_color)
                    .add_modifier(Modifier::BOLD),
            ));
        let mut text = t.text.clone();
        // Truncate so it fits one line inside the box. Leave room for the
        // left-edge severity stripe (▎) + leading glyph + space.
        let max = (width as usize).saturating_sub(7);
        if text.chars().count() > max {
            text = text.chars().take(max.saturating_sub(1)).collect::<String>();
            text.push('…');
        }
        // Chunky severity stripe on the left edge of the body. Reads as a
        // notification-card accent bar the way Slack / VS Code toasts look,
        // and keeps the severity signal even at the periphery of vision.
        let para = Paragraph::new(Line::from(vec![
            Span::styled(
                stripe_glyph(theme.icons),
                Style::default()
                    .fg(border_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {glyph} "),
                Style::default()
                    .fg(border_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(text, Style::default().fg(theme.text)),
        ]))
        .block(block);
        f.render_widget(Clear, rect);
        f.render_widget(para, rect);
        y += toast_h;
    }
}

pub(super) fn draw_saved_configs_interactive(
    f: &mut Frame,
    area: Rect,
    app: &App,
    items: &[(String, String)],
    cursor: usize,
    confirm_delete: bool,
) {
    let popup = centered_overlay(OverlaySize::Picker, area);
    f.render_widget(Clear, popup);
    let theme = &app.theme;
    let target = app
        .selected_env()
        .map(|e| e.name.clone())
        .unwrap_or_else(|| "—".into());
    // App of the apply-target env. Templates from a different app can't be
    // applied (EB rejects cross-app), so we dim those rows + add a marker
    // so the operator knows before pressing enter.
    let target_app = app.selected_env().map(|e| e.application.clone());
    // popup.height includes the title row + border. Subtract those + uniform
    // padding (1) + the 2 banner lines + the footer line. The remainder is
    // how many item rows we can show before clipping; if items overflow,
    // window them around the cursor.
    let row_budget = popup.height.saturating_sub(8) as usize;
    let (mut visible_start, mut visible_end) = visible_window(cursor, items.len(), row_budget);
    // Header rows (one per app-name group inside the window) and the
    // "more below" trailer consume rows the item budget didn't count —
    // a window spanning several apps used to clip the cursor row and
    // footer off the unscrolled popup. Count and re-window once; a
    // second pass would change the header count by at most one row.
    let count_headers = |start: usize, end: usize| -> usize {
        let mut prev = if start > 0 {
            items.get(start - 1).map(|(a, _)| a.as_str())
        } else {
            None
        };
        let mut n = 0;
        for (a, _) in &items[start..end] {
            if Some(a.as_str()) != prev {
                n += 1;
                prev = Some(a.as_str());
            }
        }
        n
    };
    let headers = count_headers(visible_start, visible_end);
    let reduced = row_budget.saturating_sub(headers + 1).max(1);
    if reduced < visible_end - visible_start {
        let (s, e) = visible_window(cursor, items.len(), reduced);
        visible_start = s;
        visible_end = e;
    }
    let mut lines: Vec<Line> = Vec::with_capacity(row_budget + 6);
    let banner = if confirm_delete {
        let cur_label = items
            .get(cursor)
            .map(|(a, t)| format!("{a}/{t}"))
            .unwrap_or_else(|| "?".into());
        Line::from(Span::styled(
            format!(" delete {cur_label}?  (Y/N)"),
            Style::default()
                .fg(theme.health_red)
                .add_modifier(Modifier::BOLD),
        ))
    } else {
        Line::from(Span::styled(
            format!(" apply target: {target}"),
            Style::default().fg(theme.muted),
        ))
    };
    lines.push(banner);
    if visible_start > 0 {
        lines.push(Line::from(Span::styled(
            format!(" ↑ {visible_start} more above"),
            Style::default().fg(theme.muted),
        )));
    } else {
        lines.push(Line::from(""));
    }
    // Group rows under app-name headers as the cursor walks the visible
    // window. Header lines aren't selectable so the cursor index still
    // maps 1:1 to `items`.
    let mut prev_app: Option<&str> = None;
    // If the first visible item isn't index 0, look back to figure out
    // whether to print its app header. We always emit a header when the
    // current item's app differs from the previous *visible* row.
    if visible_start > 0 {
        prev_app = items.get(visible_start - 1).map(|(a, _)| a.as_str());
    }
    for (i, (app_name, tmpl)) in items
        .iter()
        .enumerate()
        .skip(visible_start)
        .take(visible_end - visible_start)
    {
        if Some(app_name.as_str()) != prev_app {
            lines.push(Line::from(Span::styled(
                app_name.clone(),
                Style::default()
                    .fg(theme.title_alt)
                    .add_modifier(Modifier::BOLD),
            )));
            prev_app = Some(app_name.as_str());
        }
        let cross_app = target_app
            .as_ref()
            .map(|ta| ta != app_name)
            .unwrap_or(false);
        let marker = if i == cursor {
            glyph(theme.icons, " ▶ ", " > ")
        } else {
            "   "
        };
        let style = if i == cursor {
            let bg = if confirm_delete {
                theme.row_red_bg
            } else {
                theme.row_selected_bg
            };
            Style::default()
                .fg(theme.text)
                .bg(bg)
                .add_modifier(Modifier::BOLD)
        } else if cross_app {
            // Cross-app templates dimmed — EB rejects applying a template
            // from a different application, so the operator should see
            // before pressing enter that this row isn't a valid apply.
            Style::default().fg(theme.muted)
        } else {
            Style::default().fg(theme.text)
        };
        let suffix = if cross_app {
            "  (different app — apply will fail)"
        } else {
            ""
        };
        let line = Line::from(vec![
            Span::styled(marker.to_string(), Style::default().fg(theme.accent)),
            Span::styled(tmpl.clone(), style),
            Span::styled(suffix.to_string(), Style::default().fg(theme.health_yellow)),
        ]);
        lines.push(line);
    }
    if visible_end < items.len() {
        let more = items.len() - visible_end;
        lines.push(Line::from(Span::styled(
            format!(" ↓ {more} more below"),
            Style::default().fg(theme.muted),
        )));
    }
    lines.push(Line::from(""));
    let footer = if confirm_delete {
        " Y confirm • N / esc cancel "
    } else {
        " j/k move • enter/a apply • i inspect • c create • x delete • ? help • esc close "
    };
    lines.push(Line::from(Span::styled(
        footer,
        Style::default().fg(theme.muted),
    )));
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(&app.theme, "saved configurations", true, app.theme.title)
            .padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

/// Shared chrome for the two streaming tail overlays (`:logs-tail` /
/// `:event-tail`): window the pre-formatted, pre-filtered lines to
/// the Wide popup, append the footer (filter-entry echo, last error,
/// or the overlay's key hints), and render the titled paragraph.
/// The callers keep only what genuinely differs: line formatting,
/// filter predicate, and title text.
pub(super) struct TailChrome<'a> {
    title: &'a str,
    key_hints: &'a str,
    last_err: Option<&'a String>,
}

pub(super) fn draw_tail_overlay_chrome(
    f: &mut Frame,
    area: Rect,
    app: &App,
    view: &crate::app::TailView,
    lines: Vec<Line>,
    chrome: TailChrome<'_>,
) {
    let popup = centered_overlay(OverlaySize::Wide, area);
    f.render_widget(Clear, popup);
    let theme = &app.theme;
    // popup.height minus borders/padding/title/footer (≈6). Slice the tail
    // when following; otherwise honour the view's scroll.
    let body_rows = popup.height.saturating_sub(6) as usize;
    let start = crate::app::tail_window_start(lines.len(), body_rows, view);
    let mut paragraph_lines: Vec<Line> = lines.into_iter().skip(start).take(body_rows).collect();
    let footer_text = if view.filter_active {
        format!(" filter: {}_ (esc cancel)", view.filter_input.text())
    } else if let Some(err) = chrome.last_err {
        format!(" {}{err}", warn_glyph(theme.icons))
    } else {
        chrome.key_hints.to_string()
    };
    paragraph_lines.push(Line::raw(""));
    paragraph_lines.push(Line::from(Span::styled(
        footer_text,
        Style::default().fg(theme.muted),
    )));
    let p = Paragraph::new(paragraph_lines)
        .wrap(Wrap { trim: false })
        .block(
            titled_block(&app.theme, chrome.title, true, app.theme.title)
                .padding(Padding::uniform(1)),
        );
    f.render_widget(p, popup);
}

/// Suffix for a tail overlay's title reflecting the follow state.
pub(super) fn tail_follow_suffix(following: bool) -> &'static str {
    if following {
        " · following"
    } else {
        " · paused (G to follow)"
    }
}

pub(super) fn draw_log_tail_overlay(f: &mut Frame, area: Rect, app: &App) {
    let Some(crate::app::Overlay::LogTail {
        log_group,
        env_name,
        events,
        view,
        last_err,
        ..
    }) = app.current_overlay.as_ref()
    else {
        return;
    };
    let theme = &app.theme;
    // Format each event as `HH:MM:SS  STREAM_TAIL  message`. Stream names
    // are EB instance ids — keep just the last 8 chars so the line stays
    // scannable.
    let mut lines: Vec<Line> = Vec::with_capacity(events.len());
    for ev in events.iter() {
        if let Some(pat) = view.filter_pattern.as_ref() {
            if !pat.is_match(&ev.message) {
                continue;
            }
        }
        let dt = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ev.timestamp_ms)
            .unwrap_or_else(chrono::Utc::now);
        let stream_tail: String = ev
            .stream
            .chars()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let ts_style = Style::default().fg(theme.muted);
        let stream_style = Style::default().fg(theme.title_alt);
        let msg_style = Style::default().fg(theme.text);
        lines.push(Line::from(vec![
            Span::styled(format!("{}  ", dt.format("%H:%M:%S")), ts_style),
            Span::styled(format!("{stream_tail}  "), stream_style),
            Span::styled(ev.message.clone(), msg_style),
        ]));
    }
    let title_text = format!(
        "logs-tail — {env_name} · {} · {} lines{}",
        log_group.rsplit('/').next().unwrap_or(log_group.as_str()),
        events.len(),
        tail_follow_suffix(view.following)
    );
    draw_tail_overlay_chrome(
        f,
        area,
        app,
        view,
        lines,
        TailChrome {
            title: &title_text,
            key_hints: " j/k scroll · g/G top/follow · / filter · n clear-filter · Tab change group · esc / q close",
            last_err: last_err.as_ref(),
        },
    );
}

pub(super) fn draw_event_tail_overlay(f: &mut Frame, area: Rect, app: &App) {
    let Some(crate::app::Overlay::EventTail {
        events,
        view,
        last_err,
        ..
    }) = app.current_overlay.as_ref()
    else {
        return;
    };
    let theme = &app.theme;
    // Format each event as `HH:MM:SS  SEV  env  message`. Env names are
    // left un-truncated (they're the routing key in a fleet stream);
    // the message is capped so one verbose event can't wrap into a
    // whole page.
    let mut lines: Vec<Line> = Vec::with_capacity(events.len());
    for ev in events.iter() {
        if let Some(pat) = view.filter_pattern.as_ref() {
            if !crate::app::event_tail_matches(pat, ev) {
                continue;
            }
        }
        let ts = ev
            .at
            .map(|at| at.format("%H:%M:%S").to_string())
            .unwrap_or_else(|| "--:--:--".into());
        let sev_style = crate::ui::event_severity_style(&ev.severity, theme);
        lines.push(Line::from(vec![
            Span::styled(format!("{ts}  "), Style::default().fg(theme.muted)),
            Span::styled(format!("{:<5} ", ev.severity), sev_style),
            Span::styled(
                format!("{}  ", ev.env),
                Style::default().fg(theme.title_alt),
            ),
            Span::styled(
                truncate_for_display(&ev.message, 200),
                Style::default().fg(theme.text),
            ),
        ]));
    }
    // The line count is post-filter; show `shown/held` while a
    // filter is active so the title doesn't overstate what's visible.
    let title_text = format!(
        "event-tail — fleet · {} events{}",
        if view.filter_pattern.is_some() {
            format!("{}/{}", lines.len(), events.len())
        } else {
            events.len().to_string()
        },
        tail_follow_suffix(view.following)
    );
    draw_tail_overlay_chrome(
        f,
        area,
        app,
        view,
        lines,
        TailChrome {
            title: &title_text,
            key_hints: " j/k scroll · g/G top/follow · / filter · n clear-filter · esc / q close",
            last_err: last_err.as_ref(),
        },
    );
}

pub(super) fn draw_text_dump_overlay(
    f: &mut Frame,
    area: Rect,
    app: &App,
    title: &str,
    text: &str,
) {
    let popup = centered_overlay(OverlaySize::Text, area);
    f.render_widget(Clear, popup);
    let theme = &app.theme;
    let lines: Vec<Line> = text
        .lines()
        .map(|l| Line::from(Span::styled(l.to_string(), Style::default().fg(theme.text))))
        .collect();
    // Pin the close-hint to the bottom row of the popup so it stays
    // visible even when the body overflows. Body region + 1-row footer
    // both render inside the same titled block.
    let outer = titled_block(&app.theme, title, true, app.theme.title);
    let inner = outer.inner(popup);
    f.render_widget(outer, popup);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), chunks[0]);
    f.render_widget(
        Paragraph::new(Span::styled(
            " esc / q to close",
            Style::default().fg(theme.muted),
        )),
        chunks[1],
    );
}

/// Rendered width of the splash scene (20 art pixels × 2 cells).
const ABOUT_SCENE_W: u16 = 40;
/// Width budget for the `:about` project-text block.
const ABOUT_TEXT_W: u16 = 58;

/// Which of the three `:about` layouts to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AboutLayout {
    /// Scene above the text — roomy terminal.
    Stacked,
    /// Scene left, text right — wide but short.
    SideBySide,
    /// No scene — small terminal.
    TextOnly,
}

/// Pure: pick the `:about` layout for a `w`×`h` terminal given the
/// project-text block height `text_h`. The `+6` / `+8` budgets
/// cover the bordered block, padding, and ~2 rows/cols of slack so
/// content never butts against the card edge.
pub(super) fn about_layout(w: u16, h: u16, text_h: u16) -> AboutLayout {
    let scene_h = crate::splash::SPLASH_SCENE_ROWS as u16;
    // Stacked uses the wider of scene-vs-text as its popup width so
    // the text lines don't wrap mid-word. Only pick Stacked if the
    // terminal can actually accommodate that width — otherwise fall
    // through to TextOnly (which is always narrower).
    let stacked_w = ABOUT_SCENE_W.max(ABOUT_TEXT_W) + 6;
    if w >= stacked_w && h >= scene_h + text_h + 6 {
        AboutLayout::Stacked
    } else if w >= ABOUT_SCENE_W + ABOUT_TEXT_W + 8 && h >= scene_h + 4 {
        AboutLayout::SideBySide
    } else {
        AboutLayout::TextOnly
    }
}

/// `:about` overlay — the project card with the animated 8-bit
/// angry-giant-eats-the-beanstalk scene. The animation frame is
/// derived from `opened.elapsed()` (the `anim` ticker wakes the
/// draw loop while this overlay is up).
///
/// Three responsive layouts pick themselves from the terminal size:
/// **stacked** (scene above text, roomy terminal), **side-by-side**
/// (scene left, text right — wide but short), or **text-only** (no
/// scene, small terminal). The popup is sized to the layout chosen.
pub(super) fn draw_about(f: &mut Frame, area: Rect, app: &App, opened: std::time::Instant) {
    let theme = &app.theme;
    let frame = (opened.elapsed().as_millis() / 30) as u64;

    // Project text block.
    let title_style = Style::default()
        .fg(theme.title)
        .add_modifier(Modifier::BOLD);
    let muted = Style::default().fg(theme.muted);
    let text = Style::default().fg(theme.text);
    let accent = Style::default().fg(theme.title_alt);
    let centered = |span: Span<'static>| Line::from(span).alignment(Alignment::Center);
    let mut text_lines: Vec<Line> = Vec::new();
    text_lines.push(centered(Span::styled(
        format!("ebman {}", env!("CARGO_PKG_VERSION")),
        title_style,
    )));
    text_lines.push(centered(Span::styled(
        "k9s-style TUI for AWS Elastic Beanstalk".to_string(),
        muted,
    )));
    text_lines.push(Line::from(""));
    text_lines.push(centered(Span::styled(
        "Built by Tom Baldwin · Polymorphism Ltd".to_string(),
        accent,
    )));
    text_lines.push(centered(Span::styled(
        "https://polymorphism.co.uk".to_string(),
        muted,
    )));
    text_lines.push(Line::from(""));
    for row in [
        "Source:   https://github.com/tombaldwin/ebman",
        "License:  MIT OR Apache-2.0",
        "Crates:   https://crates.io/crates/ebman",
    ] {
        text_lines.push(centered(Span::styled(row.to_string(), text)));
    }
    text_lines.push(Line::from(""));
    for row in [
        "Polymorphism Ltd builds operations tools for teams",
        "running EB / ECS / Lambda at scale. Hire us, fork",
        "the code, or tell us what's missing — happy either way.",
    ] {
        text_lines.push(centered(Span::styled(row.to_string(), muted)));
    }
    text_lines.push(Line::from(""));
    text_lines.push(centered(Span::styled(
        "esc / q to close".to_string(),
        muted,
    )));

    // Pick a layout for the terminal, then size the popup to match.
    let scene_h = crate::splash::SPLASH_SCENE_ROWS as u16;
    let text_h = text_lines.len() as u16;
    let layout = about_layout(area.width, area.height, text_h);
    let (pw, ph) = match layout {
        // Stacked uses the wider of scene-vs-text as the popup width.
        // Without this, the text lines (designed for ABOUT_TEXT_W
        // ≈ 58 cols) wrap mid-word inside an ABOUT_SCENE_W ≈ 40-col
        // frame — "operations tools f / Hire u / what's missing —
        // happ" with everything truncated.
        AboutLayout::Stacked => (ABOUT_SCENE_W.max(ABOUT_TEXT_W) + 6, scene_h + text_h + 6),
        AboutLayout::SideBySide => (ABOUT_SCENE_W + ABOUT_TEXT_W + 8, scene_h + 4),
        AboutLayout::TextOnly => (ABOUT_TEXT_W + 6, text_h + 4),
    };
    let pw = pw.min(area.width);
    let ph = ph.min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(pw)) / 2,
        y: area.y + (area.height.saturating_sub(ph)) / 2,
        width: pw,
        height: ph,
    };
    f.render_widget(Clear, popup);
    let outer = titled_block(&app.theme, "about ebman", true, app.theme.title)
        .padding(Padding::horizontal(1));
    let inner = outer.inner(popup);
    f.render_widget(outer, popup);

    match layout {
        AboutLayout::Stacked => {
            let mut all: Vec<Line> = vec![Line::from("")];
            all.extend(crate::splash::splash_scene_lines(frame));
            all.push(Line::from(""));
            all.extend(text_lines);
            f.render_widget(Paragraph::new(all), inner);
        }
        AboutLayout::SideBySide => {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Length(ABOUT_SCENE_W),
                    Constraint::Length(2),
                    Constraint::Min(0),
                ])
                .split(inner);
            // Scene with a one-row margin above; the column is sized
            // one row taller than the scene, so a row also falls below.
            let mut scene = vec![Line::from("")];
            scene.extend(crate::splash::splash_scene_lines(frame));
            f.render_widget(Paragraph::new(scene), cols[0]);
            // Text vertically centred in the same column height.
            let mut col_text: Vec<Line> = Vec::new();
            let pad = (cols[2].height.saturating_sub(text_h) / 2) as usize;
            col_text.extend(std::iter::repeat_with(|| Line::from("")).take(pad));
            col_text.extend(text_lines);
            f.render_widget(Paragraph::new(col_text), cols[2]);
        }
        AboutLayout::TextOnly => {
            let mut all: Vec<Line> = vec![Line::from("")];
            all.extend(text_lines);
            f.render_widget(Paragraph::new(all), inner);
        }
    }
}

pub(super) fn draw_saved_configs_overlay(f: &mut Frame, area: Rect, app: &App, text: &str) {
    let popup = centered_overlay(OverlaySize::Picker, area);
    f.render_widget(Clear, popup);
    let theme = &app.theme;
    let mut lines: Vec<Line> = text
        .lines()
        .map(|l| {
            let style = if l.starts_with("Application:") {
                Style::default()
                    .fg(theme.title_alt)
                    .add_modifier(Modifier::BOLD)
            } else if l.trim_start().starts_with('▸') {
                Style::default().fg(theme.text)
            } else if l.starts_with("─") {
                Style::default().fg(theme.muted)
            } else {
                Style::default().fg(theme.text)
            };
            Line::from(Span::styled(l.to_string(), style))
        })
        .collect();
    push_close_hint(&mut lines, &app.theme);
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(&app.theme, "saved configurations", true, app.theme.title)
            .padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

/// Append a one-line `esc / q to close` hint to an overlay's body so the
/// title bar can stay clean. Pushes a blank separator first.
pub(super) fn push_close_hint(lines: &mut Vec<Line<'static>>, theme: &Theme) {
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " esc / q to close",
        Style::default().fg(theme.muted),
    )));
}

pub(super) fn draw_diff_overlay(f: &mut Frame, area: Rect, app: &App, text: &str) {
    let popup = centered_overlay(OverlaySize::Wide, area);
    f.render_widget(Clear, popup);
    let theme = &app.theme;
    let mut lines: Vec<Line> = text
        .lines()
        .map(|l| {
            let style = if l.starts_with('≠') {
                Style::default()
                    .fg(theme.health_yellow)
                    .add_modifier(Modifier::BOLD)
            } else if l.starts_with("─") {
                Style::default().fg(theme.muted)
            } else {
                Style::default().fg(theme.text)
            };
            Line::from(Span::styled(l.to_string(), style))
        })
        .collect();
    push_close_hint(&mut lines, &app.theme);
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(&app.theme, "diff", true, app.theme.title).padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

pub(super) fn draw_alarms_overlay(f: &mut Frame, area: Rect, app: &App, text: &str) {
    let popup = centered_overlay(OverlaySize::Text, area);
    f.render_widget(Clear, popup);
    let theme = &app.theme;
    let mut lines: Vec<Line> = text
        .lines()
        .map(|l| {
            // Highlight alarm state at the start of each line.
            let style = if l.starts_with("ALARM") {
                Style::default()
                    .fg(theme.health_red)
                    .add_modifier(Modifier::BOLD)
            } else if l.starts_with("OK") {
                Style::default().fg(theme.health_green)
            } else if l.starts_with("INSUFFICIENT") || l.trim_start().starts_with("↳") {
                Style::default().fg(theme.muted)
            } else {
                Style::default().fg(theme.text)
            };
            Line::from(Span::styled(l.to_string(), style))
        })
        .collect();
    push_close_hint(&mut lines, &app.theme);
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(&app.theme, "alarms", true, app.theme.title).padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

/// Title for the `:why` triage overlay, framed by the env's current
/// health. The overlay surfaces the same sections regardless of colour
/// (recent events / alarms / instances / deploys / queues), but a green
/// env shouldn't be asked "why is X red?" — that misreads as alarm.
pub(super) fn why_overlay_title(env_name: &str, health: &str) -> String {
    if health.eq_ignore_ascii_case("Red") || health.eq_ignore_ascii_case("Severe") {
        format!("why is {env_name} red?")
    } else if health.eq_ignore_ascii_case("Yellow")
        || health.eq_ignore_ascii_case("Warning")
        || health.eq_ignore_ascii_case("Degraded")
    {
        format!("why is {env_name} amber?")
    } else {
        format!("{env_name} — recent activity")
    }
}

/// Formatted detail text for a `:why` event drill (Enter on an event).
pub(super) fn format_why_event(e: &crate::aws::Event) -> String {
    let when =
        e.at.map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S %Z")
                .to_string()
        })
        .unwrap_or_else(|| "(unknown time)".into());
    format!("event @ {when}\nseverity: {}\n\n{}", e.severity, e.message)
}

/// Formatted detail text for a `:why` alarm drill.
pub(super) fn format_why_alarm(a: &crate::aws::CwAlarm) -> String {
    let mut out = format!(
        "alarm: {}\nstate:  {}\nmetric: {}/{}\n",
        a.name, a.state, a.namespace, a.metric_name
    );
    if !a.state_reason.is_empty() {
        out.push_str(&format!("\nreason:\n{}\n", a.state_reason));
    }
    out
}

/// Formatted detail text for a `:why` instance drill.
pub(super) fn format_why_instance(i: &crate::aws::Instance) -> String {
    let mut out = format!(
        "instance: {}\nhealth:   {}  ({})\ntype:     {}\nAZ:       {}\n",
        i.id, i.health, i.color, i.instance_type, i.availability_zone
    );
    if !i.causes.is_empty() {
        out.push_str("\ncauses:\n");
        for c in &i.causes {
            out.push_str(&format!("  - {c}\n"));
        }
    }
    out
}

/// Formatted detail text for a `:why` deploy drill.
pub(super) fn format_why_deploy(v: &crate::aws::AppVersion) -> String {
    let when = v
        .created
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S %Z")
                .to_string()
        })
        .unwrap_or_else(|| "(unknown time)".into());
    let mut out = format!("version:     {}\ndeployed at: {when}\n", v.label);
    if !v.description.is_empty() {
        out.push_str(&format!("\n{}\n", v.description));
    }
    out
}

pub(super) fn draw_why_red_overlay(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(crate::app::Overlay::WhyRed {
        env_name,
        tier,
        events,
        alarms,
        instances,
        deploys,
        queues,
        dlq_messages,
        cursor,
        ..
    }) = app.current_overlay.as_ref()
    else {
        return;
    };
    let cursor = *cursor;
    let is_worker = tier.eq_ignore_ascii_case("Worker");
    // Drillable items tracked alongside lines so the post-render highlight
    // pass can mark items[cursor]'s line, and the key handler reading
    // App.why_items knows what to drill into on Enter.
    let mut items: Vec<(crate::app::WhyItem, usize)> = Vec::new();
    let popup = centered_overlay(OverlaySize::Wide, area);
    f.render_widget(Clear, popup);
    let theme = &app.theme;
    let now = chrono::Utc::now();
    let mut lines: Vec<Line> = Vec::new();
    let section = |title: &str| -> Line<'static> {
        Line::from(Span::styled(
            format!("─── {title} "),
            Style::default()
                .fg(theme.title_alt)
                .add_modifier(Modifier::BOLD),
        ))
    };
    let blank = || Line::raw("");
    let muted = |s: String| -> Line<'static> {
        Line::from(Span::styled(s, Style::default().fg(theme.muted)))
    };

    // Operator-configured runbook for this env (config.toml `runbooks.ENV`),
    // surfaced at the top of the triage overlay so it's the first thing
    // the responder sees.
    if let Some(url) = app.cfg.runbooks.get(env_name) {
        lines.push(Line::from(vec![
            Span::styled(
                " runbook  ",
                Style::default()
                    .fg(theme.title_alt)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(url.clone(), Style::default().fg(theme.accent)),
        ]));
        lines.push(blank());
    }

    // Cost (when `:cost on` has populated `app.costs`). Two reasons cost
    // belongs at the top: (a) "are we paying for this red env to be
    // wrong" is a question that *should* take a quarter-second to
    // answer during triage, and (b) the bucket-tint (green / muted /
    // red, identical to the COST column's bucket rules) gives the
    // responder a coarse signal alongside health. Only rendered when
    // cost data is loaded — keeps the overlay shape stable for
    // operators who haven't enabled cost tracking.
    if let Some(cost) = app.costs.get(env_name).copied() {
        let bucket_fg = if cost >= 500.0 {
            theme.health_red
        } else if cost >= 50.0 {
            theme.text
        } else {
            theme.health_green
        };
        lines.push(Line::from(vec![
            Span::styled(
                " cost     ",
                Style::default()
                    .fg(theme.title_alt)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("${cost:.0}"),
                Style::default().fg(bucket_fg).add_modifier(Modifier::BOLD),
            ),
            Span::styled("/mo  ", Style::default().fg(theme.muted)),
            Span::styled(
                "(Cost Explorer, last 30d)",
                Style::default().fg(theme.muted),
            ),
        ]));
        lines.push(blank());
    }

    // 1. RECENT EVENTS (last 30 minutes — the window where "what went
    // wrong" usually shows up; older events are noise during triage).
    lines.push(section("recent events (last 30 min)"));
    match events {
        None => lines.push(muted(" fetching events…".into())),
        Some(Err(e)) => lines.push(Line::from(Span::styled(
            format!(" error: {e}"),
            Style::default().fg(theme.health_red),
        ))),
        Some(Ok(evs)) => {
            let cutoff = now - chrono::Duration::minutes(30);
            let recent: Vec<&crate::aws::Event> = evs
                .iter()
                .filter(|e| e.at.map(|t| t >= cutoff).unwrap_or(true))
                .take(15)
                .collect();
            if recent.is_empty() {
                lines.push(muted(" (no events in the last 30 min)".into()));
            } else {
                for e in recent {
                    let when =
                        e.at.map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
                            .unwrap_or_else(|| "??:??".into());
                    let sev_style = crate::ui::event_severity_style(&e.severity, theme);
                    items.push((
                        crate::app::WhyItem::Describe(format_why_event(e)),
                        lines.len(),
                    ));
                    lines.push(Line::from(vec![
                        Span::styled(format!(" {when}  "), Style::default().fg(theme.muted)),
                        Span::styled(format!("{:<5}", e.severity), sev_style),
                        Span::raw("  "),
                        Span::styled(e.message.clone(), Style::default().fg(theme.text)),
                    ]));
                }
            }
        }
    }
    lines.push(blank());

    // 2. ALARMS — ALARM-state ones first (red), then INSUFFICIENT_DATA
    // (yellow), then OK (green/muted). Operator wants to scan for active
    // alarms; OK alarms confirm what *isn't* the problem.
    lines.push(section("alarms"));
    match alarms {
        None => lines.push(muted(" fetching alarms…".into())),
        Some(Err(e)) => lines.push(Line::from(Span::styled(
            format!(" error: {e}"),
            Style::default().fg(theme.health_red),
        ))),
        Some(Ok(als)) => {
            if als.is_empty() {
                lines.push(muted(" (no CloudWatch alarms attached to this env)".into()));
            } else {
                // Active first
                let mut sorted: Vec<&crate::aws::CwAlarm> = als.iter().collect();
                sorted.sort_by_key(|a| match a.state.as_str() {
                    "ALARM" => 0,
                    "INSUFFICIENT_DATA" => 1,
                    _ => 2,
                });
                for a in sorted.iter().take(10) {
                    let (tag, style) = match a.state.as_str() {
                        "ALARM" => (
                            "ALARM",
                            Style::default()
                                .fg(theme.health_red)
                                .add_modifier(Modifier::BOLD),
                        ),
                        "OK" => ("OK   ", Style::default().fg(theme.health_green)),
                        _ => ("INS  ", Style::default().fg(theme.muted)),
                    };
                    items.push((
                        crate::app::WhyItem::Describe(format_why_alarm(a)),
                        lines.len(),
                    ));
                    lines.push(Line::from(vec![
                        Span::raw(" "),
                        Span::styled(tag.to_string(), style),
                        Span::raw("  "),
                        Span::styled(
                            a.name.clone(),
                            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("  ({}/{})", a.namespace, a.metric_name),
                            Style::default().fg(theme.muted),
                        ),
                    ]));
                    if !a.state_reason.is_empty() && a.state == "ALARM" {
                        lines.push(Line::from(Span::styled(
                            format!("   ↳ {}", a.state_reason),
                            Style::default().fg(theme.muted),
                        )));
                    }
                }
            }
        }
    }
    lines.push(blank());

    // 2.5 WORKER QUEUES — only rendered for Worker-tier envs. Surfaces
    // main + DLQ depths and a peek of DLQ message bodies so the operator
    // sees why the row went Red without leaving the overlay. Hidden
    // entirely for Web envs.
    if is_worker {
        lines.push(section("worker queues"));
        match queues {
            None => lines.push(muted(" fetching queue depths…".into())),
            Some(Err(e)) => lines.push(Line::from(Span::styled(
                format!(" error: {e}"),
                Style::default().fg(theme.health_red),
            ))),
            Some(Ok(q)) => {
                let main_line = match q.main_stats.as_ref() {
                    Some(s) => format!(
                        " main:  visible={}  in-flight={}  delayed={}",
                        s.visible, s.in_flight, s.delayed
                    ),
                    None => " main:  (queue URL not resolved)".to_string(),
                };
                let main_style = match q.main_stats.as_ref().map(|s| s.visible).unwrap_or(0) {
                    n if n > 100 => Style::default().fg(theme.health_yellow),
                    _ => Style::default().fg(theme.text),
                };
                lines.push(Line::from(Span::styled(main_line, main_style)));
                let dlq_visible = q.dlq_stats.as_ref().map(|s| s.visible).unwrap_or(0);
                let dlq_line = match q.dlq_stats.as_ref() {
                    Some(s) => format!(
                        " dlq:   visible={}  in-flight={}  delayed={}",
                        s.visible, s.in_flight, s.delayed
                    ),
                    None => " dlq:   (queue URL not resolved)".to_string(),
                };
                let dlq_style = if dlq_visible > 0 {
                    Style::default()
                        .fg(theme.health_red)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.text)
                };
                if q.dlq_url.is_some() {
                    items.push((crate::app::WhyItem::OpenDlq, lines.len()));
                }
                lines.push(Line::from(Span::styled(dlq_line, dlq_style)));
                // DLQ peek — only renders when there's something to peek
                // at. Empty result = "DLQ is clean" (no header line);
                // non-empty = bodies truncated to one screen-line each.
                if dlq_visible > 0 {
                    match dlq_messages {
                        None => lines.push(muted(" peeking dlq messages…".into())),
                        Some(Err(e)) => lines.push(Line::from(Span::styled(
                            format!(" dlq peek error: {e}"),
                            Style::default().fg(theme.health_red),
                        ))),
                        Some(Ok(msgs)) if msgs.is_empty() => {
                            // DLQ has visible messages but the peek
                            // returned empty — likely the messages are
                            // mid-visibility-timeout from another peek.
                            lines.push(muted(
                                " dlq peek returned no bodies (try again in a few seconds)".into(),
                            ));
                        }
                        Some(Ok(msgs)) => {
                            lines.push(Line::from(Span::styled(
                                format!(" dlq message peek ({} of {dlq_visible}):", msgs.len()),
                                Style::default().fg(theme.muted),
                            )));
                            for (i, m) in msgs.iter().enumerate() {
                                let when = m
                                    .sent_at
                                    .map(|t| humanize_age(now.signed_duration_since(t)))
                                    .unwrap_or_else(|| "—".into());
                                items.push((crate::app::WhyItem::OpenDlq, lines.len()));
                                lines.push(Line::from(vec![
                                    Span::styled(
                                        format!("   {}.", i + 1),
                                        Style::default().fg(theme.muted),
                                    ),
                                    Span::styled(
                                        format!(" sent {when} ago"),
                                        Style::default().fg(theme.muted),
                                    ),
                                    Span::styled(
                                        format!("  · received {}×", m.receive_count),
                                        Style::default().fg(theme.muted),
                                    ),
                                ]));
                                lines.push(Line::from(Span::styled(
                                    format!("      {}", truncate_for_display(&m.body, 100)),
                                    Style::default().fg(theme.text),
                                )));
                            }
                        }
                    }
                }
            }
        }
        lines.push(blank());
    }

    // 3. INSTANCE HEALTH — list each instance with its health colour +
    // causes. Severe / Warning rows pull the operator's eye first.
    lines.push(section("instance health"));
    match instances {
        None => lines.push(muted(" fetching instance health…".into())),
        Some(Err(e)) => lines.push(Line::from(Span::styled(
            format!(" error: {e}"),
            Style::default().fg(theme.health_red),
        ))),
        Some(Ok(insts)) => {
            if insts.is_empty() {
                lines.push(muted(" (no instances reported)".into()));
            } else {
                for i in insts {
                    let style = match i.color.as_str() {
                        "Red" => Style::default().fg(theme.health_red),
                        "Yellow" => Style::default().fg(theme.health_yellow),
                        "Green" => Style::default().fg(theme.health_green),
                        _ => Style::default().fg(theme.muted),
                    };
                    items.push((
                        crate::app::WhyItem::Describe(format_why_instance(i)),
                        lines.len(),
                    ));
                    lines.push(Line::from(vec![
                        Span::raw(" "),
                        Span::styled(i.id.clone(), Style::default().fg(theme.text)),
                        Span::raw("  "),
                        Span::styled(format!("{:<8}", i.health), style),
                        Span::styled(
                            format!("  {}  {}", i.instance_type, i.availability_zone),
                            Style::default().fg(theme.muted),
                        ),
                    ]));
                    for cause in i.causes.iter().take(3) {
                        lines.push(Line::from(Span::styled(
                            format!("   ↳ {cause}"),
                            Style::default().fg(theme.muted),
                        )));
                    }
                }
            }
        }
    }
    lines.push(blank());

    // 4. RECENT DEPLOYS — top 3 versions, newest first. The most-recent
    // deploy is the prime suspect when health flips Red right after.
    lines.push(section("recent deploys"));
    match deploys {
        None => lines.push(muted(" fetching deploys…".into())),
        Some(Err(e)) => lines.push(Line::from(Span::styled(
            format!(" error: {e}"),
            Style::default().fg(theme.health_red),
        ))),
        Some(Ok(vers)) => {
            if vers.is_empty() {
                lines.push(muted(" (no versions registered yet)".into()));
            } else {
                for v in vers.iter().take(5) {
                    let when = v
                        .created
                        .map(|t| humanize_age(now.signed_duration_since(t)))
                        .unwrap_or_else(|| "—".into());
                    let when_style = Style::default().fg(age_color(v.created, now, theme));
                    let mut spans = vec![
                        Span::raw(" "),
                        Span::styled(
                            v.label.clone(),
                            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(format!("  {when} ago"), when_style),
                    ];
                    if !v.description.is_empty() {
                        spans.push(Span::styled(
                            format!("  — {}", truncate_for_display(&v.description, 60)),
                            Style::default().fg(theme.muted),
                        ));
                    }
                    items.push((
                        crate::app::WhyItem::Describe(format_why_deploy(v)),
                        lines.len(),
                    ));
                    lines.push(Line::from(spans));
                }
            }
        }
    }
    // Cursor highlight: prepend a ▶ glyph to the active item's line.
    // Out-of-range cursor (e.g. items shrank under it) clamps to the last
    // item; an empty items list skips highlighting altogether.
    if !items.is_empty() {
        let active = cursor.min(items.len() - 1);
        let (_, line_idx) = items[active];
        if let Some(line) = lines.get_mut(line_idx) {
            let original = std::mem::take(line);
            let mut spans: Vec<Span<'static>> = Vec::with_capacity(original.spans.len() + 1);
            spans.push(Span::styled(
                glyph(theme.icons, "▶", ">"),
                Style::default()
                    .fg(theme.title_alt)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.extend(original.spans);
            *line = Line::from(spans);
        }
    }

    // Drill-in / navigation hint.
    let dlq_available = is_worker
        && matches!(
            queues,
            Some(Ok(qs)) if qs.dlq_url.is_some()
        );
    lines.push(Line::from(""));
    let mut hint = String::from(" ↑↓ / j k  navigate    ↵ Enter  drill in");
    if dlq_available {
        hint.push_str("    d  open DLQ viewer");
    }
    lines.push(Line::from(Span::styled(
        hint,
        Style::default().fg(theme.muted),
    )));
    push_close_hint(&mut lines, theme);

    let health = app
        .environments
        .iter()
        .find(|e| &e.name == env_name)
        .map(|e| e.health.as_str())
        .unwrap_or("");
    let title = why_overlay_title(env_name, health);
    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(titled_block(theme, &title, true, theme.title).padding(Padding::uniform(1)));
    f.render_widget(p, popup);

    // Hand the items list to the key handler now that the overlay borrow
    // (env_name / events / alarms / ... destructured at the top) is no
    // longer needed.
    app.why_items = items.into_iter().map(|(item, _)| item).collect();
}

/// Apps-scope action overlay. Small centred popup with one row per
/// `AppsActionItem`. Cursor row gets the title-alt accent (matches the
/// SavedConfigsInteractive cursor styling). Footer hint enumerates the
/// keys so the operator doesn't have to read help.
pub(super) fn draw_apps_action_menu(
    f: &mut Frame,
    area: Rect,
    app: &App,
    app_name: &str,
    env_names: &[String],
    cursor: usize,
) {
    let theme = &app.theme;
    let popup = centered_overlay(OverlaySize::Small, area);
    f.render_widget(Clear, popup);
    let n_envs = env_names.len();
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!("application: {app_name}  ·  {n_envs} env(s)"),
        Style::default().fg(theme.muted),
    )));
    lines.push(Line::from(""));
    for (i, item) in crate::app::APPS_ACTION_ITEMS.iter().enumerate() {
        let active = i == cursor;
        let cursor_glyph = if active { cursor_marker(theme) } else { "  " };
        // Inline the env count so the operator sees the blast radius
        // for the destructive batch entries without flipping screens.
        let label = match item {
            crate::app::AppsActionItem::BatchRebuild => {
                format!("Rebuild all {n_envs} env(s)")
            }
            crate::app::AppsActionItem::BatchRestart => {
                format!("Restart all {n_envs} env(s)")
            }
            crate::app::AppsActionItem::BatchDeploy => {
                format!("Deploy version label to all {n_envs} env(s)")
            }
            _ => item.label().to_string(),
        };
        let style = if active {
            Style::default()
                .fg(theme.title_alt)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };
        lines.push(Line::from(vec![
            Span::styled(cursor_glyph.to_string(), style),
            Span::styled(label, style),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " j/k move · enter dispatch · esc / q cancel",
        Style::default().fg(theme.muted),
    )));
    let title = format!("apps action — {app_name}");
    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(titled_block(theme, &title, true, theme.title).padding(Padding::uniform(1)));
    f.render_widget(p, popup);
}

/// Bug-report overlay. Renders the scrubbed payload as a scrollable
/// text dump + a footer key strip advertising the y / b / esc
/// keybinds the operator picks among. Wide popup so long log lines
/// don't reflow into unreadable wrap.
pub(super) fn draw_report_bug_overlay(f: &mut Frame, area: Rect, app: &App, body: &str) {
    let theme = &app.theme;
    let popup = centered_overlay(OverlaySize::Wide, area);
    f.render_widget(Clear, popup);
    let mut lines: Vec<Line<'static>> = body
        .lines()
        .map(|l| {
            // Distinguish section headers (### …) and code-fence rows
            // for at-a-glance scanning. Pure text overlay otherwise.
            if l.starts_with("### ") || l.starts_with("## ") {
                Line::from(Span::styled(
                    l.to_string(),
                    Style::default()
                        .fg(theme.title)
                        .add_modifier(Modifier::BOLD),
                ))
            } else if l.starts_with("```") {
                Line::from(Span::styled(
                    l.to_string(),
                    Style::default().fg(theme.muted),
                ))
            } else if l.starts_with("<!--") {
                Line::from(Span::styled(
                    l.to_string(),
                    Style::default()
                        .fg(theme.muted)
                        .add_modifier(Modifier::ITALIC),
                ))
            } else {
                Line::from(Span::styled(l.to_string(), Style::default().fg(theme.text)))
            }
        })
        .collect();
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(
            "  y",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" copy to clipboard   ", Style::default().fg(theme.muted)),
        Span::styled(
            "b",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " open GitHub issue in browser   ",
            Style::default().fg(theme.muted),
        ),
        Span::styled(
            "esc / q",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" cancel", Style::default().fg(theme.muted)),
    ]));
    let title = "bug report (scrubbed — review before sending)";
    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(titled_block(theme, title, true, theme.title).padding(Padding::uniform(1)));
    f.render_widget(p, popup);
}

pub(super) fn draw_history_overlay(f: &mut Frame, area: Rect, app: &App, text: &str) {
    let popup = centered_overlay(OverlaySize::Text, area);
    f.render_widget(Clear, popup);
    let mut lines: Vec<Line> = text
        .lines()
        .map(|l| {
            Line::from(Span::styled(
                l.to_string(),
                Style::default().fg(app.theme.text),
            ))
        })
        .collect();
    push_close_hint(&mut lines, &app.theme);
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(&app.theme, "history", true, app.theme.title).padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

pub(super) fn draw_whatsnew(f: &mut Frame, area: Rect, app: &App, text: &str) {
    let popup = centered_overlay(OverlaySize::Text, area);
    f.render_widget(Clear, popup);
    let lines: Vec<Line> = text
        .lines()
        .map(|l| {
            Line::from(Span::styled(
                l.to_string(),
                Style::default().fg(app.theme.text),
            ))
        })
        .collect();
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(
            &app.theme,
            "what's new — esc / w / q to close",
            true,
            app.theme.title_alt,
        )
        .padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

pub(super) fn draw_describe(f: &mut Frame, area: Rect, app: &App, text: &str) {
    let popup = centered_overlay(OverlaySize::Text, area);
    f.render_widget(Clear, popup);
    let lines: Vec<Line> = text
        .lines()
        .map(|l| {
            Line::from(Span::styled(
                l.to_string(),
                Style::default().fg(app.theme.text),
            ))
        })
        .collect();
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(
            &app.theme,
            "describe — esc / D / q to close",
            true,
            app.theme.title_alt,
        )
        .padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

pub(super) fn draw_picker(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = app.theme.clone();
    let Some(picker) = app.picker.as_mut() else {
        return;
    };
    let popup = centered_overlay(OverlaySize::Picker, area);
    f.render_widget(Clear, popup);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(popup);

    let filter_block = titled_block(&theme, picker.title().trim(), true, theme.title_alt);
    let mut filter_spans = vec![
        Span::styled(
            " /",
            Style::default()
                .fg(theme.health_yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ];
    filter_spans.extend(input_caret_spans(
        picker.filter.text(),
        picker.filter.cursor_col(),
        Style::default().fg(theme.text),
        Style::default()
            .fg(theme.health_yellow)
            .add_modifier(Modifier::SLOW_BLINK),
        &theme,
    ));
    let filter_inner = Paragraph::new(Line::from(filter_spans)).block(filter_block);
    f.render_widget(filter_inner, layout[0]);

    let filtered = picker.filtered();
    let items: Vec<ListItem> = filtered
        .iter()
        .map(|i| {
            let name = picker.items[*i].clone();
            ListItem::new(Line::from(Span::styled(
                format!(" {name}"),
                Style::default().fg(theme.text),
            )))
        })
        .collect();

    let list_block = rounded_block(&theme, true);
    let list = List::new(items).block(list_block).highlight_style(
        Style::default()
            .bg(theme.row_selected_bg)
            .add_modifier(Modifier::BOLD),
    );

    // List widget uses absolute indexes into its items vec, which is `filtered`.
    // Map the picker's "real" selection to its filtered position for rendering.
    let mut visible_state = ratatui::widgets::ListState::default();
    if let Some(real) = picker.list_state.selected() {
        visible_state.select(filtered.iter().position(|i| *i == real));
    }
    f.render_stateful_widget(list, layout[1], &mut visible_state);

    let hint = Paragraph::new(Span::styled(
        " j/k move  type to filter  enter select  esc cancel",
        Style::default().fg(theme.muted),
    ));
    f.render_widget(hint, layout[2]);
}
