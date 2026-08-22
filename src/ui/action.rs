//! The confirm modal — the largest single view, and the one every
//! destructive action passes through.
//!
//! Moved verbatim out of `src/ui.rs` (5,046 lines) in the 0.31 split;
//! visibility widened to `pub(crate)` where a sibling needs it, and
//! nothing else changed. `use super::*` picks up the shared vocabulary
//! the way `detail.rs` and `overlays.rs` already do.

use super::*;

pub(crate) fn draw_action(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = app.theme.clone();
    let Some(flow) = app.action_flow.as_mut() else {
        return;
    };
    match flow {
        ActionFlow::Menu { list_state } => {
            let popup = centered_overlay(OverlaySize::Small, area);
            f.render_widget(Clear, popup);
            let layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(3), Constraint::Length(1)])
                .split(popup);
            let items: Vec<ListItem> = ACTIONS
                .iter()
                .map(|a| {
                    let style = if a.destructive() {
                        Style::default()
                            .fg(theme.health_red)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme.text)
                    };
                    // Per-action glyph in muted (or red for destructive) so the
                    // shape carries the signal without competing with the label.
                    let glyph_style = if a.destructive() {
                        Style::default().fg(theme.health_red)
                    } else {
                        Style::default().fg(theme.title_alt)
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(format!(" {} ", a.glyph(theme.icons)), glyph_style),
                        Span::styled(format!("{} ", a.label()), style),
                    ]))
                })
                .collect();
            let list = List::new(items)
                .block(titled_block(&theme, "action", true, theme.title_alt))
                .highlight_style(
                    Style::default()
                        .bg(theme.row_selected_bg)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol(cursor_marker(&theme));
            f.render_stateful_widget(list, layout[0], list_state);
            f.render_widget(
                Paragraph::new(Span::styled(
                    " j/k move   [enter] select   [esc] cancel",
                    Style::default().fg(theme.muted),
                )),
                layout[1],
            );
        }
        ActionFlow::SwapTarget { source, picker } => {
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
            let title = format!("swap CNAMEs: {source} ↔ ?");
            let block = titled_block(&theme, &title, true, theme.title_alt);
            let mut prompt_spans = vec![
                Span::styled(
                    " /",
                    Style::default()
                        .fg(theme.health_yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ];
            prompt_spans.extend(input_caret_spans(
                picker.filter.text(),
                picker.filter.cursor_col(),
                Style::default().fg(theme.text),
                Style::default()
                    .fg(theme.health_yellow)
                    .add_modifier(Modifier::SLOW_BLINK),
                &theme,
            ));
            let prompt = Paragraph::new(Line::from(prompt_spans)).block(block);
            f.render_widget(prompt, layout[0]);
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
            let list = List::new(items)
                .block(rounded_block(&theme, true))
                .highlight_style(
                    Style::default()
                        .bg(theme.row_selected_bg)
                        .add_modifier(Modifier::BOLD),
                );
            let mut vs = ratatui::widgets::ListState::default();
            if let Some(real) = picker.list_state.selected() {
                vs.select(filtered.iter().position(|i| *i == real));
            }
            f.render_stateful_widget(list, layout[1], &mut vs);
            f.render_widget(
                Paragraph::new(Span::styled(
                    " j/k move   type to filter   [enter] confirm   [esc] cancel",
                    Style::default().fg(theme.muted),
                )),
                layout[2],
            );
        }
        ActionFlow::Confirm(modal) => {
            let popup = centered_overlay(OverlaySize::Small, area);
            f.render_widget(Clear, popup);
            // Treat scale-to-zero as destructive at the modal level even
            // though `Action::Scale.destructive() == false` — dropping
            // an env to 0 instances serves zero requests, which is
            // operator-visible as severe as Terminate. `Action::Scale`
            // stays non-destructive in the type system so non-zero
            // scales (which are routine) don't get the alarming red
            // accent. (0.17.4 fix; the 0.17.2 patch added the SCALE-TO-
            // ZERO body copy without the matching modal styling.)
            let scale_to_zero = modal.action == Action::Scale
                && modal.scale_min == Some(0)
                && modal.scale_max == Some(0);
            let render_destructive = modal.action.destructive() || scale_to_zero;
            let accent = if render_destructive {
                theme.health_red
            } else {
                theme.title_alt
            };
            let block = rounded_block(&theme, true)
                .border_style(Style::default().fg(accent))
                .title(Span::styled(
                    " confirm ",
                    Style::default().fg(accent).add_modifier(Modifier::BOLD),
                ));
            let mut lines: Vec<Line> = Vec::new();
            let summary = match modal.action {
                Action::Rebuild => format!(
                    "Rebuild env '{}'? (terminates and recreates all instances)",
                    modal.target_env
                ),
                Action::RestartAppServer => {
                    format!("Restart app server on env '{}'?", modal.target_env)
                }
                Action::SwapCnames => format!(
                    "Swap CNAMEs between '{}' and '{}'?",
                    modal.target_env,
                    modal.swap_with.as_deref().unwrap_or("?")
                ),
                Action::Terminate => format!(
                    "TERMINATE env '{}'. This cannot be undone.",
                    modal.target_env
                ),
                Action::Deploy => format!(
                    "Deploy version '{}' to env '{}'? (rolling, reversible)",
                    modal.deploy_version.as_deref().unwrap_or("?"),
                    modal.target_env
                ),
                Action::UpgradePlatform => format!(
                    "Upgrade '{}' to platform: {} (rolling, reversible)",
                    modal.target_env,
                    modal.upgrade_platform_label.as_deref().unwrap_or("?")
                ),
                Action::Clone => format!(
                    "Clone '{}' into a new env named '{}'? (creates a new env)",
                    modal.target_env,
                    modal.clone_target.as_deref().unwrap_or("?")
                ),
                Action::Scale => {
                    let min = modal.scale_min.unwrap_or(0);
                    let max = modal.scale_max.unwrap_or(0);
                    if min == 0 && max == 0 {
                        format!(
                            "SCALE TO ZERO: '{}' will serve 0 requests (`:start` to resume)",
                            modal.target_env
                        )
                    } else {
                        format!(
                            "Scale '{}' to min={min} / max={max}? (rolling)",
                            modal.target_env
                        )
                    }
                }
                Action::AbortUpdate => format!("Abort current update on '{}'?", modal.target_env),
                Action::SsmRun => {
                    let cmd = modal.ssm_run_command.as_deref().unwrap_or("?");
                    let n = modal
                        .ssm_run_instances
                        .as_ref()
                        .map(|v| v.len())
                        .unwrap_or(0);
                    format!(
                        "SSM-RUN: `{cmd}` on {n} instance{} of '{}' (treat as write)",
                        if n == 1 { "" } else { "s" },
                        modal.target_env
                    )
                }
                // These variants never reach the ConfirmModal — they're
                // dispatched directly from command paths (Capacity opens a
                // modal form; Config* and TerminateInstance have their own
                // spawn paths). Placeholder copy keeps the match
                // exhaustive without dead UI.
                Action::Capacity
                | Action::ConfigSave
                | Action::ConfigDelete
                | Action::ConfigApply
                | Action::TerminateInstance => {
                    format!("{} on '{}'", modal.action.label(), modal.target_env)
                }
            };
            lines.push(Line::from(""));
            // Render the env name in red+bold for destructive actions so
            // the operator can't miss what's about to be nuked even if
            // they scan the modal too fast to read the full sentence.
            let body_style = Style::default()
                .fg(if render_destructive {
                    theme.health_red
                } else {
                    theme.text
                })
                .add_modifier(Modifier::BOLD);
            let name_style = if render_destructive {
                Style::default()
                    .fg(theme.health_red)
                    .bg(theme.row_red_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(theme.title_alt)
                    .add_modifier(Modifier::BOLD)
            };
            lines.push(highlight_env_in_summary(
                &summary,
                &modal.target_env,
                body_style,
                name_style,
            ));
            // Pre-flight traffic-level warning if anything noteworthy is in
            // progress (mid-deploy, recent change, currently Red). Rendered
            // before the dry-run info so the operator sees state-level concerns
            // first.
            if let Some(w) = &modal.traffic_warning {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("  {}{w}", warn_glyph(theme.icons)),
                    Style::default()
                        .fg(theme.health_red)
                        .add_modifier(Modifier::BOLD),
                )));
            }
            // tf-managed env warning: if the target appears in the
            // cached tfstate, the operator's about to drift the tf
            // state by mutating EB directly. Yellow not red — this
            // is a heads-up, not a block. Rendered just below
            // traffic_warning so state-level concerns stay grouped.
            if app.tf_managed_envs.contains(&modal.target_env) {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!(
                        "  {}env is terraform-managed — changes will drift on next plan/apply",
                        warn_glyph(theme.icons)
                    ),
                    Style::default()
                        .fg(theme.health_yellow)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(Span::styled(
                    "    :drift to see what will diverge",
                    Style::default().fg(theme.muted),
                )));
            }
            // Dry-run preview: instance count + AZ spread, when available.
            if modal.loading_dryrun {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "  fetching impact…",
                    Style::default().fg(theme.muted),
                )));
            } else if let Some(dr) = &modal.dryrun {
                lines.push(Line::from(""));
                let inst_word = if dr.instance_count == 1 {
                    "instance"
                } else {
                    "instances"
                };
                let az_word = if dr.az_count == 1 { "AZ" } else { "AZs" };
                lines.push(Line::from(Span::styled(
                    format!(
                        "  impact: {} {inst_word} across {} {az_word}",
                        dr.instance_count, dr.az_count
                    ),
                    Style::default()
                        .fg(theme.health_yellow)
                        .add_modifier(Modifier::BOLD),
                )));
            }
            // Deploy-plan unavailability — capacity-impact derived
            // from deployment-policy + batch + ASG max. Yellow when
            // any instance unavailability, green/muted when zero.
            // Loading state stays silent rather than showing a
            // placeholder — keeps the modal compact when the fetch
            // hasn't landed yet.
            if let Some((body, caution)) = &modal.unavailability_line {
                let style = if *caution {
                    Style::default()
                        .fg(theme.health_yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.health_green)
                };
                lines.push(Line::from(Span::styled(format!("  {body}"), style)));
            }
            // Lint warnings — rule-keyed risks against the env's
            // pre-write state. Renders only Warn+ (Info issues
            // are noise in a confirm modal; the operator can
            // see them via `:lint` separately). Each issue gets
            // a header line `⚠ [EBL001] <title>` plus an
            // indented `→ <suggestion>` when one's available.
            // Skips the loading state silently — modal stays
            // compact while the spawn runs.
            if let Some(issues) = &modal.lint_issues {
                use crate::lint::Severity;
                let mut to_show: Vec<&crate::lint::Issue> = issues
                    .iter()
                    .filter(|i| i.severity >= Severity::Warn)
                    .collect();
                // Sort severity DESC then rule_id ASC so the operator
                // sees `Error` issues at the top of the modal — when
                // there are 3+ warnings, the worst one shouldn't be
                // buried at the bottom (0.19 review item).
                to_show.sort_by(|a, b| {
                    b.severity
                        .cmp(&a.severity)
                        .then_with(|| a.rule_id.cmp(&b.rule_id))
                });
                if !to_show.is_empty() {
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        "  lint warnings:",
                        Style::default().fg(theme.muted),
                    )));
                    for issue in to_show {
                        let sev_glyph = match issue.severity {
                            Severity::Error => glyph(theme.icons, "✗", "x"),
                            Severity::Warn => glyph(theme.icons, "⚠", "!"),
                            Severity::Info => glyph(theme.icons, "·", "-"),
                        };
                        let color = match issue.severity {
                            Severity::Error => theme.health_red,
                            _ => theme.health_yellow,
                        };
                        lines.push(Line::from(Span::styled(
                            format!("    {sev_glyph} [{}] {}", issue.rule_id, issue.title),
                            Style::default().fg(color).add_modifier(Modifier::BOLD),
                        )));
                        if let Some(suggestion) = &issue.suggestion {
                            lines.push(Line::from(Span::styled(
                                format!("      → {suggestion}"),
                                Style::default().fg(theme.muted),
                            )));
                        }
                    }
                }
            }
            // Pre-flight events: last 3 events on this env.
            if let Some(events) = &modal.recent_events {
                if !events.is_empty() {
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        "  recent events:",
                        Style::default().fg(theme.muted),
                    )));
                    let now = chrono::Utc::now();
                    for e in events.iter().take(3) {
                        let when = match e.at {
                            Some(t) => humanize_age(now.signed_duration_since(t)),
                            None => "—".into(),
                        };
                        // Full message — the modal wraps now, so we no
                        // longer truncate mid-word.
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("    {when:>4}  "),
                                Style::default().fg(theme.muted),
                            ),
                            Span::styled(
                                format!("{:<5}  ", e.severity),
                                severity_style(&e.severity, &theme),
                            ),
                            Span::styled(e.message.clone(), Style::default().fg(theme.text)),
                        ]));
                    }
                }
            } else if modal.loading_events {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "  fetching recent events…",
                    Style::default().fg(theme.muted),
                )));
            }
            // Inline version preview for deploys — saves the operator
            // a separate `:deploy LABEL --preview` round-trip. Only
            // populated for Action::Deploy; renders below the recent-
            // events block. Indented to match the other modal blocks.
            if modal.loading_version_preview {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "  fetching version metadata…",
                    Style::default().fg(theme.muted),
                )));
            } else if let Some(preview) = &modal.version_preview {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "  pre-deploy preview:",
                    Style::default().fg(theme.muted),
                )));
                for raw in preview.lines() {
                    // The formatter prefixes warning lines with `⚠` (older-
                    // candidate rollback hint, unknown-label refusal).
                    // Match on the glyph to colour just those lines.
                    let style = if raw.contains('⚠') {
                        Style::default()
                            .fg(theme.health_yellow)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme.text)
                    };
                    lines.push(Line::from(Span::styled(format!("    {raw}"), style)));
                }
            }
            // Pre-deploy health-check probe outcome. Silence is
            // golden — only render when the probe found a problem.
            // Failed probe doesn't block the deploy; auto-rollback
            // is the catch-net, the warning is the heads-up.
            if modal.loading_health_check {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "  probing health-check URL…",
                    Style::default().fg(theme.muted),
                )));
            } else if let Some(Err(reason)) = &modal.health_check_probe {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("  ⚠ health-check probe: {reason}"),
                    Style::default()
                        .fg(theme.health_yellow)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(Span::styled(
                    "    (deploy will proceed; consider --auto-rollback Nm if this matters)",
                    Style::default().fg(theme.muted),
                )));
            }
            lines.push(Line::from(""));
            match modal.kind {
                ConfirmKind::YesNo => {
                    lines.push(Line::from(Span::styled(
                        "  [y] yes / [enter]    [n] no / [esc]",
                        Style::default().fg(theme.muted),
                    )));
                }
                ConfirmKind::TypeName => {
                    lines.push(Line::from(vec![
                        Span::styled("  type ", Style::default().fg(theme.muted)),
                        Span::styled(
                            modal.target_env.clone(),
                            Style::default()
                                .fg(theme.health_yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" to confirm:", Style::default().fg(theme.muted)),
                    ]));
                    lines.push(Line::from(""));
                    let matches = modal.typed.text() == modal.target_env.as_str();
                    let typed_style = Style::default()
                        .fg(if matches {
                            theme.health_green
                        } else {
                            theme.text
                        })
                        .add_modifier(Modifier::BOLD);
                    let mut typed_spans = vec![Span::raw("  ")];
                    typed_spans.extend(input_caret_spans(
                        modal.typed.text(),
                        modal.typed.cursor_col(),
                        typed_style,
                        Style::default()
                            .fg(theme.health_yellow)
                            .add_modifier(Modifier::SLOW_BLINK),
                        &theme,
                    ));
                    lines.push(Line::from(typed_spans));
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        if matches {
                            "  [enter] terminate    [esc] cancel"
                        } else {
                            "  [esc] cancel"
                        },
                        Style::default().fg(theme.muted),
                    )));
                }
            }
            f.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .block(block),
                popup,
            );
        }
        ActionFlow::Rollout(flow) => {
            let popup = centered_overlay(OverlaySize::Wide, area);
            f.render_widget(Clear, popup);
            let title = format!(
                " rollout {} — {} → {} ",
                flow.rollout_id, flow.env_name, flow.version_label
            );
            let block = ratatui::widgets::Block::default()
                .borders(ratatui::widgets::Borders::ALL)
                .title(title)
                .border_style(Style::default().fg(theme.title));
            let mut lines: Vec<Line> = Vec::new();
            // Header note keyed to current state.
            let state_line = match &flow.state {
                crate::mode_action::RolloutState::Planning => {
                    "  pre-flighting regions…".to_string()
                }
                crate::mode_action::RolloutState::AwaitingConfirm => {
                    let n_ok = flow
                        .regions
                        .iter()
                        .filter(|r| r.env_found == Some(true))
                        .count();
                    let n_total = flow.regions.len();
                    format!(
                        "  pre-flight complete ({n_ok}/{n_total} ok) — press y to dispatch, n / esc to abort"
                    )
                }
                crate::mode_action::RolloutState::Dispatching { next_index } => {
                    format!(
                        "  dispatching region {}/{}…",
                        next_index + 1,
                        flow.regions.len()
                    )
                }
                crate::mode_action::RolloutState::Done => {
                    let n_ok = flow
                        .regions
                        .iter()
                        .filter(|r| matches!(r.outcome, Some(Ok(()))))
                        .count();
                    let n_err = flow
                        .regions
                        .iter()
                        .filter(|r| matches!(r.outcome, Some(Err(_))))
                        .count();
                    let n_skipped = flow
                        .regions
                        .iter()
                        .filter(|r| r.outcome.is_none() && r.env_found != Some(false))
                        .count();
                    format!(
                        "  done — {n_ok} ok, {n_err} failed, {n_skipped} skipped (esc / q to close)"
                    )
                }
            };
            lines.push(Line::from(Span::styled(
                state_line,
                Style::default().fg(theme.muted),
            )));
            lines.push(Line::from(""));
            // Column header.
            lines.push(Line::from(Span::styled(
                "  REGION                ENV               CURRENT           TARGET            STATUS",
                Style::default().fg(theme.muted),
            )));
            for row in &flow.regions {
                // status_text is borrowed `&str` for the static
                // cases and `String` for the error case. To unify,
                // build it as `String` always and slice as `&str`
                // in the Span.
                let (status_text, status_color): (String, _) =
                    match (&row.outcome, row.env_found, &row.preflight_error) {
                        (Some(Ok(())), _, _) => (
                            format!("{} deployed", glyph(theme.icons, "✓", "+")),
                            theme.health_green,
                        ),
                        (Some(Err(_)), _, _) => {
                            // Short label only; the full error
                            // surfaces as an indented "↳" line below
                            // the row.
                            (
                                format!("{} failed", glyph(theme.icons, "✗", "x")),
                                theme.health_red,
                            )
                        }
                        (None, Some(false), Some(_)) => (
                            format!("{} pre-flight fail", glyph(theme.icons, "✗", "x")),
                            theme.health_red,
                        ),
                        (None, Some(false), None) => (
                            format!("{} env not found", glyph(theme.icons, "✗", "x")),
                            theme.health_red,
                        ),
                        (None, Some(true), _) => {
                            if matches!(
                                flow.state,
                                crate::mode_action::RolloutState::Dispatching { .. }
                            ) {
                                ("pending".to_string(), theme.muted)
                            } else {
                                (
                                    format!("{} pre-flight ok", glyph(theme.icons, "✓", "+")),
                                    theme.health_green,
                                )
                            }
                        }
                        (None, None, _) => ("planning…".to_string(), theme.muted),
                    };
                let current = row.current_version.as_deref().unwrap_or("…");
                let region_col = pad_right(&row.region, 22);
                let env_col = pad_right(&flow.env_name, 18);
                let current_col = pad_right(current, 18);
                let target_col = pad_right(&flow.version_label, 18);
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::raw(region_col),
                    Span::raw(env_col),
                    Span::raw(current_col),
                    Span::raw(target_col),
                    Span::styled(
                        status_text.to_string(),
                        Style::default()
                            .fg(status_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
                if let Some(err) = &row.preflight_error {
                    lines.push(Line::from(Span::styled(
                        format!("        ↳ {err}"),
                        Style::default().fg(theme.muted),
                    )));
                }
                if let Some(Err(e)) = &row.outcome {
                    lines.push(Line::from(Span::styled(
                        format!("        ↳ {e}"),
                        Style::default().fg(theme.muted),
                    )));
                }
            }
            f.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .block(block),
                popup,
            );
        }
    }
}
