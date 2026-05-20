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
