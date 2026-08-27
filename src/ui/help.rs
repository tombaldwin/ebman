//! The `?` help overlay and its per-mode topic pages — carved out of the 9,400-line `ui.rs` root (0.27
//! architecture pass, the same `app/` submodule pattern). Items are
//! `pub(super)`; the root glob-imports them so call sites and tests
//! are untouched. Shared chrome helpers (blocks, pills, glyphs,
//! `centered_overlay`) stay in the root and reach here via
//! `use super::*`.

use super::*;

pub(super) fn draw_help(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = &app.theme;
    let popup = overlay_rect(OverlaySize::Text, area);
    f.render_widget(Clear, popup);
    // Per-context help: when the user pressed `?` inside Detail / DLQ /
    // Action / Shell, show only the keys relevant to that screen. The
    // global keymap is still available via `?` from Normal mode.
    match app.help.topic {
        crate::app::HelpTopic::Detail => return draw_help_detail(f, popup, app),
        crate::app::HelpTopic::Dlq => return draw_help_dlq(f, popup, app),
        crate::app::HelpTopic::Action => return draw_help_action(f, popup, app),
        crate::app::HelpTopic::Shell => return draw_help_shell(f, popup, app),
        crate::app::HelpTopic::SavedConfigs => {
            return draw_help_saved_configs(f, popup, app);
        }
        crate::app::HelpTopic::Global => {}
    }

    let interval_secs = app.refresh_interval.as_secs();
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            "ebman — keybindings",
            Style::default()
                .fg(app.theme.title)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("j / ↓ / wheel", "move selection down", theme),
        help_line("k / ↑ / wheel", "move selection up", theme),
        help_line("g / G", "jump to top / bottom", theme),
        help_line("enter", "open drill-down view for the selected env", theme),
        help_line(
            "a",
            "open actions menu (rebuild / restart / swap / terminate)",
            theme,
        ),
        help_line("b", "open selected env in the AWS console", theme),
        help_line("D", "describe overlay (raw env dump as JSON)", theme),
        help_line(
            "!",
            "diagnose selected env (events + alarms + instances + recent deploys)",
            theme,
        ),
        help_line(
            "U",
            "undo a pending action dispatch during its 5s cancel window",
            theme,
        ),
        help_line("f", "freeze / unfreeze auto-refresh", theme),
        help_line(
            "1 - 9",
            "jump to env at position 1-9 in the current view",
            theme,
        ),
        help_line("'", "name-jump: type a prefix to move selection", theme),
        help_line(
            "Ctrl-W",
            "yank equivalent `aws elasticbeanstalk describe-environments` command",
            theme,
        ),
        help_line("tab / shift-tab", "cycle scope (envs ↔ apps); Apps scope shows per-app rollup + has its own `a` / `b` / Enter", theme),
        help_line("click", "select row", theme),
        help_line("/", "filter rows (name, app, status, health)", theme),
        help_line("s / S", "cycle sort key / toggle ascending", theme),
        help_line("Ctrl-G", "toggle group-by-application", theme),
        help_line("Ctrl-E", "toggle events panel", theme),
        help_line(
            "T",
            "cycle event timestamp format (UTC → local → age)",
            theme,
        ),
        help_line("y / Y", "yank CNAME / name to clipboard", theme),
        help_line("Ctrl-Y", "export filtered table as TSV to clipboard", theme),
        help_line("r", "switch AWS region", theme),
        help_line("p", "switch AWS profile", theme),
        help_line(
            "Ctrl-K",
            "command palette: fuzzy search across commands / envs / views / plugins",
            theme,
        ),
        help_line("Ctrl-R / F5", "refresh now", theme),
        help_line(
            "Ctrl-X",
            "toggle redact mode (account id, ARN, CNAMEs)",
            theme,
        ),
        help_line("?", "toggle this help", theme),
        help_line("q / Ctrl-C", "quit", theme),
        Line::from(""),
        // Built from `HelpTopic::names()` rather than spelled out: a
        // hardcoded list here would drift the moment a topic is added,
        // and the whole point of this line is that every topic is
        // reachable.
        help_line(
            ":help <topic>",
            &format!("open one topic by name: {}", crate::app::HelpTopic::names()),
            theme,
        ),
        Line::from(Span::styled(
            "`:help shell` is the only way to read the embedded-shell keys — \
             once attached, every keystroke belongs to the subprocess.",
            Style::default().fg(theme.muted),
        )),
    ];
    // Apps-scope keys — pressed when Tab has swapped the main table to
    // the Applications view. Distinct from Envs-scope behaviour so the
    // operator knows what `a` / `b` / Enter do over there.
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Apps-scope keys (tab to enter)",
        Style::default()
            .fg(app.theme.title)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(help_line(
        "enter",
        "drill into envs (filters the envs table to this application)",
        theme,
    ));
    lines.push(help_line(
        "a",
        "open per-app action menu (Rebuild / Restart / Deploy / Open in console)",
        theme,
    ));
    lines.push(help_line(
        "b",
        "open application's AWS console page in the browser",
        theme,
    ));
    lines.push(help_line("j / k / g / G", "navigate the apps table", theme));
    lines.push(help_line(
        "space",
        "multi-select an app (persistent until esc clears)",
        theme,
    ));
    lines.push(help_line(
        "*",
        "pin / unpin selected app (sticks to top of apps table; persists in state.toml)",
        theme,
    ));
    // Command-bar reference — driven by `crate::commands::COMMANDS` so
    // adding a built-in only touches one file. Sections render in
    // `Category::ORDER`. Plugins land in their own footer block below.
    for category in crate::commands::Category::ORDER {
        let entries: Vec<&crate::commands::CommandSpec> = crate::commands::COMMANDS
            .iter()
            .filter(|c| c.category == *category)
            .collect();
        if entries.is_empty() {
            continue;
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            category.label(),
            Style::default()
                .fg(app.theme.title)
                .add_modifier(Modifier::BOLD),
        )));
        for c in entries {
            // Label is `:name` plus a `/ :alias` chain when aliases
            // exist — matches the existing help convention where
            // `:q / :quit` was on one row.
            let mut label = format!(":{}", c.name);
            for alias in c.aliases {
                label.push_str(&format!(" / :{alias}"));
            }
            lines.push(help_line(&label, c.help, theme));
        }
    }
    // Plugin commands (user-defined in commands.toml). Listed last so
    // they don't interleave with built-ins in the built-in sections.
    if !app.plugins.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "User plugin commands (commands.toml)",
            Style::default()
                .fg(app.theme.title)
                .add_modifier(Modifier::BOLD),
        )));
        for (name, plugin) in &app.plugins {
            let desc = plugin
                .description
                .clone()
                .unwrap_or_else(|| "plugin command".to_string());
            lines.push(help_line(&format!(":{name}"), &desc, theme));
        }
    }
    // Detail-view per-tab keys — these aren't `:commands` so they
    // don't fit the registry; render manually under their own header.
    let mut detail_lines: Vec<Line> = vec![
        Line::from(""),
        Line::from(Span::styled(
            "Detail-view per-tab keys",
            Style::default()
                .fg(app.theme.title)
                .add_modifier(Modifier::BOLD),
        )),
        help_line(
            "[ / ] (Metrics tab)",
            "decrease / increase metric range (15m → 24h)",
            theme,
        ),
        help_line(
            "(Logs tab) ^R",
            "request tail logs (takes ~10–20s while EB samples instances)",
            theme,
        ),
        help_line(
            "(Logs tab) s",
            "open CW Logs streaming overlay (live tail; needs `:logs-stream on`)",
            theme,
        ),
        help_line("(Logs tab) /", "regex-filter the visible log lines", theme),
        help_line(
            "(Logs overlay) Tab",
            "switch tailed log group via picker (over the env's discovered groups)",
            theme,
        ),
        Line::from(""),
    ];
    lines.append(&mut detail_lines);
    lines.push(Line::from(Span::styled(
        format!(
            "Refresh runs automatically every {interval_secs}s. Theme: {}. Configurable in ~/.config/ebman/config.toml.",
            app.theme.name
        ),
        Style::default().fg(app.theme.muted),
    )));
    lines.push(Line::from(Span::styled(
        "Region/profile come from the standard AWS env (AWS_REGION, AWS_PROFILE).",
        Style::default().fg(app.theme.muted),
    )));
    // Split the popup into a scrollable body + a sticky 1-row byline at
    // the bottom inside the border. The body is the popup minus the
    // border (top/bottom) and minus the padding (uniform(1) — top/bottom).
    // That gives the visible row budget for line-count clamping.
    let total_lines = lines.len() as u16;
    let inner_height = popup.height.saturating_sub(4); // top border + top pad + bottom pad + bottom border
                                                       // Reserve the bottommost inner row for the sticky byline; the body
                                                       // proper gets one less than that.
    let body_height = inner_height.saturating_sub(1);
    // Maximum scroll = where the last line is pinned to the body's
    // bottom. Below that, scrolling further would reveal blank space.
    let max_scroll = total_lines.saturating_sub(body_height);
    app.help.max_scroll = max_scroll;
    let effective_scroll = app.help.scroll.min(max_scroll);

    // Scroll indicators: emit "↑ N more above" on the top inner row and
    // "↓ N more below" on the row just above the byline when there's
    // content past the viewport. Rendered AFTER the body so they overlay
    // its first / last visible row.
    let footer_row = Rect {
        x: popup.x + 2,
        y: popup.y + popup.height.saturating_sub(2),
        width: popup.width.saturating_sub(4),
        height: 1,
    };
    let above_row = Rect {
        x: popup.x + 2,
        y: popup.y + 2, // skip border + top pad
        width: popup.width.saturating_sub(4),
        height: 1,
    };
    let below_row = Rect {
        x: popup.x + 2,
        y: popup.y + popup.height.saturating_sub(3),
        width: popup.width.saturating_sub(4),
        height: 1,
    };
    let help = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((effective_scroll, 0))
        .block(
            titled_block(&app.theme, "help", true, app.theme.title_alt)
                .padding(Padding::uniform(1)),
        );
    f.render_widget(help, popup);
    let muted_hint = Style::default()
        .fg(app.theme.accent)
        .add_modifier(Modifier::BOLD);
    if effective_scroll > 0 {
        let n = effective_scroll;
        // Clear blanks the row in the back-buffer; without it the
        // indicator overlays the body's visible line and leaves ghost
        // characters past the indicator text.
        f.render_widget(Clear, above_row);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("↑ {n} more above"),
                muted_hint,
            ))),
            above_row,
        );
    }
    if effective_scroll < max_scroll {
        let n = max_scroll - effective_scroll;
        f.render_widget(Clear, below_row);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("↓ {n} more below"),
                muted_hint,
            ))),
            below_row,
        );
    }
    // Sticky byline row at the bottom of the popup. Clear first for the
    // same reason as the indicators above — without it, longer help
    // body lines bleed past the byline's text.
    f.render_widget(Clear, footer_row);
    let credit = Paragraph::new(Line::from(Span::styled(
        format!(
            "ebman {} · built by Tom Baldwin / Polymorphism Ltd · :about",
            env!("CARGO_PKG_VERSION")
        ),
        Style::default()
            .fg(app.theme.muted)
            .add_modifier(Modifier::ITALIC),
    )));
    f.render_widget(credit, footer_row);
}

pub(super) fn draw_help_detail(f: &mut Frame, popup: Rect, app: &App) {
    let theme = &app.theme;
    let lines = vec![
        Line::from(Span::styled(
            "Detail view — keybindings",
            Style::default()
                .fg(app.theme.title)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line(
            "tab / l",
            "next tab (Events → Instances → Metrics → Queue → Logs → Config)",
            theme,
        ),
        help_line("shift-tab / h", "previous tab", theme),
        help_line(
            "j / k",
            "scroll within active tab (cursor on Instances / Queue tabs)",
            theme,
        ),
        help_line("^R", "re-fetch active tab's data", theme),
        help_line("R", "toggle per-tab auto-refresh", theme),
        help_line("a", "actions menu (rebuild / restart / deploy / …)", theme),
        help_line("b", "open env in AWS console", theme),
        help_line("D", "describe overlay (raw env dump as JSON)", theme),
        help_line("d", "open DLQ for this env (Worker tier only)", theme),
        help_line("*", "pin / unpin", theme),
        Line::from(""),
        Line::from(Span::styled(
            "Events tab",
            Style::default().fg(app.theme.title),
        )),
        help_line("/", "regex filter event messages", theme),
        help_line("n / N", "jump next / previous match", theme),
        help_line("L", "cycle min severity (all → info → warn → error)", theme),
        help_line("w", "cycle time window (all → 1h → 6h → 24h → 7d)", theme),
        help_line("T", "cycle timestamp format (UTC → local → age)", theme),
        Line::from(""),
        Line::from(Span::styled(
            "Metrics tab",
            Style::default().fg(app.theme.title),
        )),
        help_line(
            "[ / ]",
            "decrease / increase metric range (15m → 24h)",
            theme,
        ),
        help_line(
            "mouse hover",
            "show metric value at cursor x-position",
            theme,
        ),
        Line::from(""),
        Line::from(Span::styled(
            "Instances tab",
            Style::default().fg(app.theme.title),
        )),
        help_line(
            "enter / i",
            "open instance info overlay (id, type, AZ, health, causes)",
            theme,
        ),
        help_line("b", "open instance in EC2 console (browser)", theme),
        help_line("s", "embedded SSM shell into selected instance", theme),
        help_line("y", "yank instance ID", theme),
        help_line(
            "x",
            "terminate selected instance (Y/N; ASG replaces)",
            theme,
        ),
        Line::from(""),
        Line::from(Span::styled(
            "Queue tab",
            Style::default().fg(app.theme.title),
        )),
        help_line("j / k", "pick Main / DLQ", theme),
        help_line("enter", "open queue viewer", theme),
        help_line("d", "quick-open DLQ", theme),
        Line::from(""),
        Line::from(Span::styled(
            "Logs tab",
            Style::default().fg(app.theme.title),
        )),
        help_line(
            "^R",
            "request tail logs (10-20s wait for instance samples)",
            theme,
        ),
        help_line(
            "s",
            "open CW Logs streaming overlay (requires `:logs-stream on`)",
            theme,
        ),
        help_line("/", "regex filter visible lines", theme),
        Line::from(""),
        Line::from(Span::styled(
            "Config tab",
            Style::default().fg(app.theme.title),
        )),
        help_line(
            "j / k",
            "move cursor over editable rows (tags + env vars)",
            theme,
        ),
        help_line(
            "enter",
            "edit selected value in place (enter saves, esc cancels)",
            theme,
        ),
        help_line(
            "n",
            "add a new row (KEY=VALUE; kind from cursor section)",
            theme,
        ),
        help_line("r", "rename the selected row's key", theme),
        help_line("x", "delete the selected row (y confirms)", theme),
        Line::from(""),
        Line::from(Span::styled(
            "esc / q  to close help; from Normal mode `?` shows the full keymap",
            Style::default().fg(theme.muted),
        )),
    ];
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(&app.theme, "help — Detail", true, app.theme.title_alt)
            .padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

pub(super) fn draw_help_dlq(f: &mut Frame, popup: Rect, app: &App) {
    let theme = &app.theme;
    let lines = vec![
        Line::from(Span::styled(
            "Queue viewer — keybindings",
            Style::default()
                .fg(app.theme.title)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("j / k", "move cursor", theme),
        help_line("enter", "view full message body", theme),
        help_line("r", "resend selected (DLQ → main) — DLQ view only", theme),
        help_line(
            "R",
            "replay batch: all / count / window (1h 24h 7d) — DLQ view only",
            theme,
        ),
        help_line("x", "delete selected message (Y/N confirm)", theme),
        help_line(
            "p",
            "purge queue (strict typed-name confirm) — DLQ view only",
            theme,
        ),
        help_line("m", "toggle Main ↔ DLQ", theme),
        help_line(
            "^R",
            "refetch messages (deeper peek with long-polling)",
            theme,
        ),
        help_line("esc / q", "close viewer", theme),
        Line::from(""),
        Line::from(Span::styled(
            "Resend and purge are disabled in Main view — too dangerous on a live queue.",
            Style::default().fg(theme.muted),
        )),
    ];
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(&app.theme, "help — Queue", true, app.theme.title_alt)
            .padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

pub(super) fn draw_help_action(f: &mut Frame, popup: Rect, app: &App) {
    let theme = &app.theme;
    let lines = vec![
        Line::from(Span::styled(
            "Action menu — keybindings",
            Style::default()
                .fg(app.theme.title)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("j / k", "move cursor between actions", theme),
        help_line(
            "enter",
            "select; opens confirm modal (or picker for Swap)",
            theme,
        ),
        help_line("esc", "close menu", theme),
        Line::from(""),
        Line::from(Span::styled(
            "Confirm modal",
            Style::default().fg(app.theme.title),
        )),
        help_line("y / enter", "confirm and dispatch", theme),
        help_line("n / esc", "cancel", theme),
        help_line(
            "(typing)",
            "TypeName confirm (Terminate) — must match env name exactly",
            theme,
        ),
        Line::from(""),
        Line::from(Span::styled(
            "Parameterised actions (Deploy / Upgrade / Clone / Scale) close the menu",
            Style::default().fg(theme.muted),
        )),
        Line::from(Span::styled(
            "and prefill the command bar; type the arg and Enter to run.",
            Style::default().fg(theme.muted),
        )),
    ];
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(&app.theme, "help — Action", true, app.theme.title_alt)
            .padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

pub(super) fn draw_help_saved_configs(f: &mut Frame, popup: Rect, app: &App) {
    let theme = &app.theme;
    let lines = vec![
        Line::from(Span::styled(
            "Saved configurations — keybindings",
            Style::default()
                .fg(app.theme.title)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("j / k / arrows", "move cursor up / down", theme),
        help_line("g / G", "jump to top / bottom", theme),
        help_line(
            "enter / a",
            "apply selected template to the currently-selected env",
            theme,
        ),
        help_line(
            "i",
            "inspect template — open its option settings as a sorted text dump",
            theme,
        ),
        help_line(
            "c",
            "close overlay + prefill `:config-save ` to save current env as a new template",
            theme,
        ),
        help_line(
            "x",
            "delete selected template (Y/N confirm — config templates are recreatable)",
            theme,
        ),
        help_line("?", "this help", theme),
        help_line("esc / q", "close overlay", theme),
        Line::from(""),
        Line::from(Span::styled(
            "Apply target = whichever env the table cursor is on.",
            Style::default().fg(theme.muted),
        )),
    ];
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(
            &app.theme,
            "help — Saved Configs",
            true,
            app.theme.title_alt,
        )
        .padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

pub(super) fn draw_help_shell(f: &mut Frame, popup: Rect, app: &App) {
    let theme = &app.theme;
    let lines = vec![
        Line::from(Span::styled(
            "Embedded shell — keybindings",
            Style::default()
                .fg(app.theme.title)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Almost every key is forwarded to the subprocess. Exceptions:",
            Style::default().fg(theme.muted),
        )),
        Line::from(""),
        help_line(
            "F12",
            "detach back to ebman (subprocess keeps running)",
            theme,
        ),
        help_line("^D / exit", "close the session", theme),
        Line::from(""),
        Line::from(Span::styled(
            "Open from Instances tab → s on a selected instance.",
            Style::default().fg(theme.muted),
        )),
    ];
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(&app.theme, "help — Shell", true, app.theme.title_alt)
            .padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

pub(super) fn help_line(key: &str, desc: &str, theme: &Theme) -> Line<'static> {
    // Pad short keys to a 16-char column so descriptions line up, but if the
    // key itself is wider than the column always emit at least 2 spaces of
    // separator so it can't glue against the description.
    //
    // Returns Line<'static> by cloning into owned Spans so callers can
    // pass non-'static labels (e.g. the registry-driven loop builds
    // `format!(":{name}")` per row). Cheap — the help screen renders
    // once per `?` press, not per frame.
    let key_col = 16usize;
    let formatted = if key.chars().count() < key_col {
        format!(" {key:<width$}", width = key_col)
    } else {
        format!(" {key}  ")
    };
    Line::from(vec![
        Span::styled(
            formatted,
            Style::default()
                .fg(theme.health_yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(desc.to_string(), Style::default().fg(theme.text)),
    ])
}
