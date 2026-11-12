//! `:managed-updates` and `:managed-apply` — managed platform updates:
//! what EB has scheduled for an env, what it ran before and how that
//! went, and running a scheduled one now rather than in its window.
//! `:managed-window` (in `cmd_settings`) sets the window itself.

use super::*;

impl App {
    /// `:managed-updates` — the selected env's scheduled managed updates
    /// and its recent history, in a text overlay. Each half can fail on
    /// its own and says so; "could not read" is never "none".
    pub(crate) fn cmd_managed_updates(&mut self) {
        let Some(env) = self.pointed_env() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        let env_name = env.name.clone();
        let title = format!("managed updates — {env_name}");
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        if self.demo_mode {
            let (scheduled, history) = crate::demo_fixture::managed_actions_for(&env_name);
            let body = format_managed_updates(&Ok(scheduled), &Ok(history), chrono::Utc::now());
            let _ = tx.send(AppMsg::TextOverlay { gen, title, body });
            return;
        }
        let client = self.client_for(&env);
        self.status_message = Some(format!("fetching managed updates for {env_name}…"));
        tokio::spawn(async move {
            let body = match client.resolve().await {
                Err(e) => format!("error: {}", flatten_err("cached_client", e)),
                Ok(aws) => {
                    let (scheduled, history) = tokio::join!(
                        aws.list_managed_actions(&env_name),
                        aws.managed_action_history(&env_name, crate::aws::MANAGED_HISTORY_MAX),
                    );
                    format_managed_updates(
                        &scheduled.map_err(|e| flatten_err("list_managed_actions", e)),
                        &history.map_err(|e| flatten_err("managed_action_history", e)),
                        chrono::Utc::now(),
                    )
                }
            };
            let _ = tx.send(AppMsg::TextOverlay { gen, title, body });
        });
    }

    /// `:managed-apply ACTION_ID` — run a scheduled managed update on the
    /// selected env now. The id is the one `:managed-updates` lists, so
    /// the operator names exactly what runs. Gated, audited and tracked
    /// like every other write; there is no undo (a platform update is an
    /// immutable deployment), and `:abort` stops one in flight.
    pub(crate) fn cmd_managed_apply(&mut self, rest: &[&str]) {
        let Some(action_id) = rest.first().map(|s| s.to_string()) else {
            self.error_message = Some(
                "usage: :managed-apply ACTION_ID  (`:managed-updates` lists the scheduled ones)"
                    .into(),
            );
            return;
        };
        let Some(env) = self.pointed_env() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        let key = self.env_key(&env);
        if self.deny_write(&key, "managed-apply") {
            return;
        }
        let label = crate::verb::Verb::ApplyManagedAction.audit_label();
        let EnvKey {
            region,
            name: env_name,
        } = key;
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &region,
            label,
            &env_name,
            &[("action_id", &action_id)],
        );
        let pending = self.push_pending("Apply managed update", env_name.clone());
        let client = self.client_for_region(&region);
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let account = self.context.account_id.clone();
        let profile = self.context.profile.clone();
        // No demo branch: `deny_write` refuses every write in demo mode.
        tokio::spawn(async move {
            let result = match client.resolve().await {
                Ok(aws) => aws
                    .apply_managed_action(&env_name, &action_id)
                    .await
                    .map_err(|e| flatten_err("apply_managed_action", e)),
                Err(e) => Err(flatten_err("cached_client", e)),
            };
            crate::audit::append_action_completed(
                account.as_deref(),
                profile.as_deref(),
                &region,
                label,
                &env_name,
                result.as_ref().map(|_| ()).map_err(|e| e.as_str()),
                &[("action_id", &action_id)],
            );
            let _ = tx.send(AppMsg::ManagedApplied {
                gen,
                pending,
                env_name,
                action_id,
                result,
            });
        });
    }

    pub(super) fn handle_managed_applied(
        &mut self,
        pending: PendingId,
        env_name: String,
        action_id: String,
        result: Result<(), String>,
    ) {
        self.complete_pending(pending, result.clone());
        match result {
            Ok(()) => {
                self.push_toast(
                    ToastKind::Info,
                    format!("managed update {action_id} started on {env_name} — `:abort` stops it"),
                );
                // The env goes to Updating: show it.
                self.spawn_refresh();
            }
            Err(msg) => self.push_toast(
                ToastKind::Error,
                format!("managed update {action_id} on {env_name} failed: {msg}"),
            ),
        }
    }
}

/// The `:managed-updates` overlay's text: what is scheduled, then what
/// ran. Pure. A failed half says it failed, and an empty one says there
/// is nothing, which are different answers.
pub(crate) fn format_managed_updates(
    scheduled: &Result<Vec<crate::aws::ManagedAction>, String>,
    history: &Result<Vec<crate::aws::ManagedActionRecord>, String>,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let when = |t: Option<chrono::DateTime<chrono::Utc>>| {
        t.map(|t| {
            // A window is usually ahead; a finish is behind.
            let rel = if t > now {
                format!("in {}", crate::ui::humanize_age(t - now))
            } else {
                format!("{} ago", crate::ui::humanize_age(now - t))
            };
            format!("{} ({rel})", t.format("%Y-%m-%d %H:%M UTC"))
        })
        .unwrap_or_else(|| "—".into())
    };
    let mut out = String::from("SCHEDULED\n");
    match scheduled {
        Err(e) => out.push_str(&format!("  could not read: {e}\n")),
        Ok(a) if a.is_empty() => out.push_str(
            "  (nothing scheduled — managed updates may be off: :managed-window DAY HOUR)\n",
        ),
        Ok(actions) => {
            for a in actions {
                out.push_str(&format!(
                    "  {}  {}  {}  window {}\n    {}\n",
                    a.id,
                    a.action_type,
                    a.status,
                    when(a.window_start),
                    a.description
                ));
            }
            out.push_str("  :managed-apply ACTION_ID runs one now; :abort stops it once running\n");
        }
    }
    out.push_str("\nHISTORY (newest first)\n");
    match history {
        Err(e) => out.push_str(&format!("  could not read: {e}\n")),
        Ok(h) if h.is_empty() => out.push_str("  (no managed update has run)\n"),
        Ok(records) => {
            for r in records {
                out.push_str(&format!(
                    "  {}  {}  {}  finished {}\n    {}\n",
                    r.id,
                    r.action_type,
                    r.status,
                    when(r.finished),
                    r.description
                ));
                if !r.failure_type.is_empty() {
                    out.push_str(&format!(
                        "    FAILED: {} — {}\n",
                        r.failure_type, r.failure_description
                    ));
                }
            }
        }
    }
    out
}
