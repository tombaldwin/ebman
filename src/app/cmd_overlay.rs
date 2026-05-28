//! Command handlers that open multi-account / read-only overlays
//! (`:accounts`, `:org-health`, `:find-env`). These are the heaviest
//! arms in `execute_command` — each spawns a fan-out across profiles
//! and AssumeRole accounts and lands the result as a TextOverlay. Same
//! `&mut self` pattern as the inline arms; just lifted out so
//! `execute_command` reads more like a dispatcher and the file size
//! comes down.
//!
//! Pulling overlay-only commands first (not state-mutating writes)
//! keeps the blast radius small — each method ends with a `tokio::spawn`
//! and the result lands via `AppMsg::TextOverlay`, so the refactor
//! doesn't change any sync state-transition behaviour.

use super::{flatten_err_to_string, format_org_accounts, App, AppMsg};

impl App {
    /// Handler for `:accounts` — fetches the AWS Organizations account
    /// list and renders it as a TextOverlay. AccessDenied surfaces a
    /// templated hint pointing at the `accounts.NAME` config workaround
    /// instead of the raw SDK error chain.
    pub(crate) fn cmd_accounts(&mut self) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let configured: std::collections::HashMap<String, String> = self
            .accounts
            .keys()
            .map(|n| (n.clone(), n.clone()))
            .collect();
        self.status_message = Some("fetching org accounts…".into());
        tokio::spawn(async move {
            let body = match aws.list_org_accounts().await {
                Ok(accounts) => format_org_accounts(&accounts, &configured),
                Err(e) => {
                    let s = flatten_err_to_string(&e);
                    if s.to_lowercase().contains("access") || s.to_lowercase().contains("denied") {
                        "no organizations:* access from this credential.\n\n\
                         `:accounts` needs to be run from the org's management account\n\
                         (or a delegated administrator). Configure child accounts manually\n\
                         via `accounts.NAME.role_arn = …` in config.toml and use `:account NAME`\n\
                         to switch into them.\n\nesc / q to close"
                            .to_string()
                    } else {
                        format!("organizations:ListAccounts failed:\n{s}\n\nesc / q to close")
                    }
                }
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: "org accounts".into(),
                body,
            });
        });
    }

    /// Handler for `:org-health` — fans out `list_environments` across
    /// every profile in `~/.aws/{config,credentials}` plus every
    /// `accounts.NAME` AssumeRole entry, aggregates per-source counts
    /// (env total + Red total) into a single overlay.
    pub(crate) fn cmd_org_health(&mut self) {
        let profiles = crate::profiles::load_profiles();
        let accounts: Vec<(String, crate::config::AccountSpec)> = self
            .accounts
            .iter()
            .map(|(n, s)| (n.clone(), s.clone()))
            .collect();
        let region = self.context.region.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        self.status_message = Some(format!(
            "scanning {} profile(s) + {} assume-role account(s) in {region}…",
            profiles.len(),
            accounts.len()
        ));
        tokio::spawn(async move {
            use futures::future::join_all;
            enum SourceKind {
                Profile,
                AssumeRole,
            }
            let profile_tasks: Vec<_> = profiles
                .iter()
                .cloned()
                .map(|p| {
                    let r = region.clone();
                    async move {
                        let res = crate::aws::list_environments_in_region(Some(p.clone()), r).await;
                        (p, SourceKind::Profile, res)
                    }
                })
                .collect();
            let account_tasks: Vec<_> = accounts
                .iter()
                .cloned()
                .map(|(name, spec)| {
                    let r = region.clone();
                    async move {
                        let res =
                            crate::aws::list_environments_for_account(&name, &spec, Some(r)).await;
                        (name, SourceKind::AssumeRole, res)
                    }
                })
                .collect();
            type OrgHealthRow = (
                String,
                SourceKind,
                color_eyre::Result<Vec<crate::aws::Environment>>,
            );
            type BoxedTask =
                std::pin::Pin<Box<dyn std::future::Future<Output = OrgHealthRow> + Send>>;
            let mut all_tasks: Vec<BoxedTask> =
                Vec::with_capacity(profile_tasks.len() + account_tasks.len());
            for t in profile_tasks {
                all_tasks.push(Box::pin(t));
            }
            for t in account_tasks {
                all_tasks.push(Box::pin(t));
            }
            let results = join_all(all_tasks).await;
            let mut lines: Vec<String> = vec![
                "Org-wide health (one row per profile / assume-role account)".into(),
                "─────────────────────────────────────".into(),
                String::new(),
            ];
            let mut total = 0usize;
            let mut total_red = 0usize;
            for (label, kind, r) in results {
                let suffix = match kind {
                    SourceKind::Profile => "",
                    SourceKind::AssumeRole => " (assume-role)",
                };
                let display = format!("{label}{suffix}");
                match r {
                    Ok(envs) => {
                        let n = envs.len();
                        total += n;
                        let red = envs
                            .iter()
                            .filter(|e| {
                                e.health.eq_ignore_ascii_case("Red")
                                    || e.health.eq_ignore_ascii_case("Severe")
                            })
                            .count();
                        total_red += red;
                        let warning = if red > 0 { " ⚠" } else { "" };
                        lines.push(format!("  {display:<34}  envs:{n:<4}  red:{red}{warning}"));
                    }
                    Err(e) => {
                        lines.push(format!("  {display:<34}  ERROR: {e}"));
                    }
                }
            }
            lines.push(String::new());
            lines.push(format!(
                "Total: {total} envs, {total_red} in Red across all sources"
            ));
            lines.push("esc / q to close".into());
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: "org health".into(),
                body: lines.join("\n"),
            });
        });
    }

    /// Handler for `:logs-insights [--window WINDOW] QUERY` — runs a
    /// CloudWatch Logs Insights query against the env's discovered log
    /// groups, polls until Complete / Failed / Cancelled / Timeout, and
    /// lands the formatted result rows as a TextOverlay. `--window`
    /// accepts the same grammar as the DLQ replay prompt: `30m` / `6h` /
    /// `24h` / `7d`. Default is the last 1 hour. Multi-group is supported
    /// by Insights natively — we pass every discovered group from
    /// `discover_env_log_groups` so the operator doesn't have to pick
    /// one. Use during post-incident analysis when `:logs-tail` regex
    /// isn't enough (e.g. "p99 latency for /checkout over the last 6h,
    /// grouped by instance").
    pub(crate) fn cmd_logs_insights(&mut self, args: &str) {
        // Parse the optional `--window WINDOW` prefix. The leading position
        // is the only one we look at — putting `--window` mid-query would
        // confuse parsing of the operator's actual Insights query syntax
        // (which may legitimately contain `--`-prefixed tokens via comments,
        // backslashes, etc.).
        let trimmed = args.trim_start();
        let (window_ms, query) = if let Some(rest) = trimmed.strip_prefix("--window ") {
            let rest = rest.trim_start();
            let (spec, query) = match rest.find(char::is_whitespace) {
                Some(pos) => (&rest[..pos], rest[pos..].trim_start()),
                None => (rest, ""),
            };
            match crate::aws::parse_window_ms(spec) {
                Some(ms) => (ms, query.to_string()),
                None => {
                    self.error_message = Some(format!(
                        "logs-insights: unrecognised --window value {spec:?} — accepts 30m / 1h / 6h / 24h / 7d"
                    ));
                    return;
                }
            }
        } else {
            (60 * 60 * 1000, trimmed.to_string())
        };
        if query.is_empty() {
            self.error_message =
                Some("usage: :logs-insights [--window 30m|1h|6h|24h|7d] <query>".into());
            return;
        }
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        let env_name = env.name.clone();
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let window_label = window_ms_label(window_ms);
        self.status_message = Some(format!(
            "running Insights query on {env_name} (last {window_label})… results land in an overlay when the query finishes (typically 2–15s)"
        ));
        tokio::spawn(async move {
            // Discover groups. Empty result is an actionable error
            // (operator hasn't streamed logs yet); SDK error is a hard fail.
            let groups = match aws.discover_env_log_groups(&env_name).await {
                Ok(g) if !g.is_empty() => g,
                Ok(_) => {
                    let _ = tx.send(AppMsg::TextOverlay {
                        gen,
                        title: format!("logs-insights — {env_name}"),
                        body: format!(
                            "no CW log groups under /aws/elasticbeanstalk/{env_name}/\n\n\
                             Enable streaming first with `:logs-stream on`, then re-run.\n\n\
                             esc / q to close"
                        ),
                    });
                    return;
                }
                Err(e) => {
                    let _ = tx.send(AppMsg::TextOverlay {
                        gen,
                        title: format!("logs-insights — {env_name}"),
                        body: format!(
                            "discover log groups failed: {}\n\nesc / q to close",
                            flatten_err_to_string(&e)
                        ),
                    });
                    return;
                }
            };
            // Time range from --window (default 1h). The Insights API
            // caps queries at 15 minutes server-side so even pathologically
            // expensive queries terminate.
            let end_ms = chrono::Utc::now().timestamp_millis();
            let start_ms = end_ms - window_ms;
            match aws
                .run_insights_query(&groups, start_ms, end_ms, &query)
                .await
            {
                Ok(results) => {
                    let mut body = crate::aws::format_insights_results(&results, &query, &groups);
                    body.push_str("\nesc / q to close");
                    let _ = tx.send(AppMsg::TextOverlay {
                        gen,
                        title: format!("logs-insights — {env_name}"),
                        body,
                    });
                }
                Err(e) => {
                    let _ = tx.send(AppMsg::TextOverlay {
                        gen,
                        title: format!("logs-insights — {env_name}"),
                        body: format!(
                            "query: {query}\nlog groups: {}\n\nQuery failed: {}\n\nesc / q to close",
                            groups.join(", "),
                            flatten_err_to_string(&e)
                        ),
                    });
                }
            }
        });
    }

    /// Handler for `:envs-by-version LABEL` — fans out the same
    /// profile + AssumeRole scan as `cmd_find_env`, but filters envs
    /// where `version_label == LABEL` (case-sensitive — labels are
    /// version identifiers, not search terms). Use during incidents
    /// when a bad build slipped to prod and the question is "where
    /// is this label deployed?". Emits one row per matching env with
    /// app / health and the source label so the operator can pivot
    /// to `:account NAME` / `:profile NAME`.
    pub(crate) fn cmd_envs_by_version(&mut self, label: &str) {
        let label = label.to_string();
        let profiles = crate::profiles::load_profiles();
        let accounts: Vec<(String, crate::config::AccountSpec)> = self
            .accounts
            .iter()
            .map(|(n, s)| (n.clone(), s.clone()))
            .collect();
        let region = self.context.region.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        self.status_message = Some(format!(
            "scanning '{label}' across {} profile(s) + {} assume-role account(s) in {region}…",
            profiles.len(),
            accounts.len(),
        ));
        tokio::spawn(async move {
            use futures::future::join_all;
            type Hit = Result<Vec<String>, String>;
            type RowFuture =
                std::pin::Pin<Box<dyn std::future::Future<Output = (String, Hit)> + Send>>;
            let mut tasks: Vec<RowFuture> = Vec::with_capacity(profiles.len() + accounts.len());
            for p in &profiles {
                let p = p.clone();
                let r = region.clone();
                let want = label.clone();
                tasks.push(Box::pin(async move {
                    let source = p.clone();
                    match crate::aws::list_environments_in_region(Some(p.clone()), r).await {
                        Ok(envs) => {
                            let hits: Vec<String> = envs
                                .into_iter()
                                .filter(|e| e.version_label == want)
                                .map(|e| {
                                    format!(
                                        "  • {p}  / {} ({}) — {} · {}",
                                        e.name, e.application, e.health, e.status
                                    )
                                })
                                .collect();
                            (source, Ok(hits))
                        }
                        Err(e) => (source, Err(format!("{e}"))),
                    }
                }));
            }
            for (name, spec) in &accounts {
                let name = name.clone();
                let spec = spec.clone();
                let r = region.clone();
                let want = label.clone();
                tasks.push(Box::pin(async move {
                    let source = format!("{name} (assume-role)");
                    match crate::aws::list_environments_for_account(&name, &spec, Some(r)).await {
                        Ok(envs) => {
                            let hits: Vec<String> = envs
                                .into_iter()
                                .filter(|e| e.version_label == want)
                                .map(|e| {
                                    format!(
                                        "  • {name} (assume-role)  / {} ({}) — {} · {}",
                                        e.name, e.application, e.health, e.status
                                    )
                                })
                                .collect();
                            (source, Ok(hits))
                        }
                        Err(e) => (source, Err(format!("{e}"))),
                    }
                }));
            }
            let results = join_all(tasks).await;
            let mut hits: Vec<String> = Vec::new();
            let mut errs: Vec<String> = Vec::new();
            for (source, r) in results {
                match r {
                    Ok(mut h) if !h.is_empty() => hits.append(&mut h),
                    Ok(_) => {}
                    Err(e) => errs.push(format!("  {source}: {e}")),
                }
            }
            let body = format!(
                "Envs running version '{label}'\n\
                 ─────────────────────────────\n\n\
                 {}\n\n\
                 {}\n\nesc / q to close",
                if hits.is_empty() {
                    "(no envs found running this version)".to_string()
                } else {
                    hits.join("\n")
                },
                if errs.is_empty() {
                    String::new()
                } else {
                    format!("Errors:\n{}", errs.join("\n"))
                },
            );
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("envs by version — {label}"),
                body,
            });
        });
    }

    /// Handler for `:find-env SUBSTRING` — same fan-out shape as
    /// `cmd_org_health` but the per-source closure filters envs by
    /// case-insensitive name / application match and emits one line
    /// per hit. Errors per source are collected separately so a
    /// failed AssumeRole on one account doesn't poison the others.
    pub(crate) fn cmd_find_env(&mut self, needle: &str) {
        let needle = needle.to_string();
        let profiles = crate::profiles::load_profiles();
        let accounts: Vec<(String, crate::config::AccountSpec)> = self
            .accounts
            .iter()
            .map(|(n, s)| (n.clone(), s.clone()))
            .collect();
        let region = self.context.region.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        self.status_message = Some(format!(
            "searching '{needle}' across {} profile(s) + {} assume-role account(s) in {region}…",
            profiles.len(),
            accounts.len(),
        ));
        tokio::spawn(async move {
            use futures::future::join_all;
            type Hit = Result<Vec<String>, String>;
            type RowFuture =
                std::pin::Pin<Box<dyn std::future::Future<Output = (String, Hit)> + Send>>;
            let needle_lc = needle.to_lowercase();
            let mut tasks: Vec<RowFuture> = Vec::with_capacity(profiles.len() + accounts.len());
            for p in &profiles {
                let p = p.clone();
                let r = region.clone();
                let n = needle_lc.clone();
                tasks.push(Box::pin(async move {
                    let label = p.clone();
                    match crate::aws::list_environments_in_region(Some(p.clone()), r).await {
                        Ok(envs) => {
                            let hits: Vec<String> = envs
                                .into_iter()
                                .filter(|e| {
                                    e.name.to_lowercase().contains(&n)
                                        || e.application.to_lowercase().contains(&n)
                                })
                                .map(|e| format!("  • {p}  / {} ({})", e.name, e.health))
                                .collect();
                            (label, Ok(hits))
                        }
                        Err(e) => (label, Err(format!("{e}"))),
                    }
                }));
            }
            for (name, spec) in &accounts {
                let name = name.clone();
                let spec = spec.clone();
                let r = region.clone();
                let n = needle_lc.clone();
                tasks.push(Box::pin(async move {
                    let label = format!("{name} (assume-role)");
                    match crate::aws::list_environments_for_account(&name, &spec, Some(r)).await {
                        Ok(envs) => {
                            let hits: Vec<String> = envs
                                .into_iter()
                                .filter(|e| {
                                    e.name.to_lowercase().contains(&n)
                                        || e.application.to_lowercase().contains(&n)
                                })
                                .map(|e| {
                                    format!("  • {name} (assume-role)  / {} ({})", e.name, e.health)
                                })
                                .collect();
                            (label, Ok(hits))
                        }
                        Err(e) => (label, Err(format!("{e}"))),
                    }
                }));
            }
            let results = join_all(tasks).await;
            let mut hits: Vec<String> = Vec::new();
            let mut errs: Vec<String> = Vec::new();
            for (label, r) in results {
                match r {
                    Ok(mut h) if !h.is_empty() => hits.append(&mut h),
                    Ok(_) => {}
                    Err(e) => errs.push(format!("  {label}: {e}")),
                }
            }
            let body = format!(
                "Cross-account search for '{needle}'\n\
                 ─────────────────────────────────\n\n\
                 {}\n\n\
                 {}\n\nesc / q to close",
                if hits.is_empty() {
                    "(no matches)".to_string()
                } else {
                    hits.join("\n")
                },
                if errs.is_empty() {
                    String::new()
                } else {
                    format!("Errors:\n{}", errs.join("\n"))
                },
            );
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("cross-account search — {needle}"),
                body,
            });
        });
    }
}

/// Pure: render a window length (milliseconds) back into the compact
/// `30m` / `6h` / `7d` form the operator typed. Picks the largest unit
/// that divides evenly; falls back to `<n>m` otherwise. Used in the
/// "running Insights query (last 6h)…" status line so the toast echoes
/// the operator's input rather than a flat ms count.
pub(crate) fn window_ms_label(ms: i64) -> String {
    let total_minutes = ms / 60_000;
    if total_minutes <= 0 {
        return "0m".into();
    }
    let days = total_minutes / (24 * 60);
    if days > 0 && total_minutes % (24 * 60) == 0 {
        return format!("{days}d");
    }
    let hours = total_minutes / 60;
    if hours > 0 && total_minutes % 60 == 0 {
        return format!("{hours}h");
    }
    format!("{total_minutes}m")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_ms_label_picks_largest_clean_unit() {
        assert_eq!(window_ms_label(30 * 60_000), "30m");
        assert_eq!(window_ms_label(60 * 60_000), "1h");
        assert_eq!(window_ms_label(6 * 60 * 60_000), "6h");
        assert_eq!(window_ms_label(24 * 60 * 60_000), "1d");
        assert_eq!(window_ms_label(7 * 24 * 60 * 60_000), "7d");
        // Falls back to minutes when no clean unit divides evenly.
        assert_eq!(window_ms_label(90 * 60_000), "90m");
        // Defensive: zero / negative render as "0m" rather than "-1d".
        assert_eq!(window_ms_label(0), "0m");
        assert_eq!(window_ms_label(-1), "0m");
    }
}
