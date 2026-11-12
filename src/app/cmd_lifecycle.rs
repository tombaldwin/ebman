//! `:version-lifecycle` — an application's version retention policy:
//! show it, set a max count or age, turn it off, or undo this session's
//! last change. The application is the selected env's.

use super::*;

/// The role EB deletes versions under when the application names none:
/// the one the console creates.
const DEFAULT_SERVICE_ROLE: &str = "aws-elasticbeanstalk-service-role";

/// What `:version-lifecycle ARGS` asks for. Pure.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LifecycleCommand {
    Show,
    MaxCount { count: i32, delete_source: bool },
    MaxAge { days: i32, delete_source: bool },
    Off,
    Undo,
}

const LIFECYCLE_USAGE: &str =
    "usage: :version-lifecycle [max-count N | max-age DAYS] [--delete-source] | off | undo";

/// Parse `:version-lifecycle`'s arguments. Counts are 1–1000 and ages
/// 1–180 days, EB's own ranges.
pub(crate) fn parse_lifecycle_command(rest: &[&str]) -> Result<LifecycleCommand, String> {
    let delete_source = rest.contains(&"--delete-source");
    let args: Vec<&str> = rest
        .iter()
        .copied()
        .filter(|a| *a != "--delete-source")
        .collect();
    let number = |v: Option<&&str>, what: &str, max: i32| -> Result<i32, String> {
        let n = v
            .and_then(|s| s.parse::<i32>().ok())
            .ok_or_else(|| format!("{what} expects a whole number — {LIFECYCLE_USAGE}"))?;
        if !(1..=max).contains(&n) {
            return Err(format!("{what} must be 1–{max} (got {n})"));
        }
        Ok(n)
    };
    match args.first().copied() {
        None => Ok(LifecycleCommand::Show),
        Some("max-count") => Ok(LifecycleCommand::MaxCount {
            count: number(args.get(1), "max-count", 1000)?,
            delete_source,
        }),
        Some("max-age") => Ok(LifecycleCommand::MaxAge {
            days: number(args.get(1), "max-age", 180)?,
            delete_source,
        }),
        Some("off") => Ok(LifecycleCommand::Off),
        Some("undo") => Ok(LifecycleCommand::Undo),
        Some(other) => Err(format!("unknown '{other}' — {LIFECYCLE_USAGE}")),
    }
}

/// The policy `command` sets, given the one in force: a rule keeps the
/// app's own service role (EB's default when it has none, since EB needs
/// one to delete under), `off` keeps the role and turns both rules off,
/// and an undo restores `undo_to` whole. `None` for `Show`, which sets
/// nothing, and for an undo with nothing to restore. Pure.
pub(crate) fn next_policy(
    command: &LifecycleCommand,
    undo_to: Option<crate::aws::VersionLifecycle>,
    current: &crate::aws::VersionLifecycle,
) -> Option<crate::aws::VersionLifecycle> {
    let role = || {
        current
            .service_role
            .clone()
            .or_else(|| Some(DEFAULT_SERVICE_ROLE.into()))
    };
    match command {
        LifecycleCommand::Undo => undo_to,
        LifecycleCommand::Show => None,
        LifecycleCommand::Off => Some(crate::aws::VersionLifecycle {
            service_role: current.service_role.clone(),
            ..Default::default()
        }),
        LifecycleCommand::MaxCount {
            count,
            delete_source,
        } => Some(crate::aws::VersionLifecycle {
            max_count: Some(*count),
            max_age_days: None,
            delete_source: *delete_source,
            service_role: role(),
        }),
        LifecycleCommand::MaxAge {
            days,
            delete_source,
        } => Some(crate::aws::VersionLifecycle {
            max_count: None,
            max_age_days: Some(*days),
            delete_source: *delete_source,
            service_role: role(),
        }),
    }
}

impl App {
    /// `:version-lifecycle [...]` on the selected env's application.
    pub(crate) fn cmd_version_lifecycle(&mut self, rest: &[&str]) {
        let command = match parse_lifecycle_command(rest) {
            Ok(c) => c,
            Err(e) => {
                self.error_message = Some(e);
                return;
            }
        };
        let Some(env) = self.pointed_env() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        if command == LifecycleCommand::Show {
            self.show_version_lifecycle(&env);
            return;
        }
        // The policy is the application's, and `--delete-source` reaches
        // every env's rollback targets: a pin on any env of the app in
        // this region refuses it, not only one on the env selected.
        let key = self.env_key(&env);
        let mut app_envs: Vec<EnvKey> = self
            .environments
            .iter()
            .filter(|e| e.application == env.application)
            .map(|e| self.env_key(e))
            .filter(|k| k.region == key.region && *k != key)
            .collect();
        app_envs.insert(0, key.clone());
        if self.deny_write_batch(&app_envs, "version-lifecycle") {
            return;
        }
        let EnvKey { region, .. } = key;
        let app = env.application.clone();
        // What to set, given the policy in force: a rule keeps the app's
        // own service role, and an undo restores the one this session
        // replaced.
        let undo_to = match &command {
            LifecycleCommand::Undo => match self
                .caches
                .lifecycle_undo
                .get(&(region.clone(), app.clone()))
                .cloned()
            {
                Some(previous) => Some(previous),
                None => {
                    self.error_message = Some(format!(
                        "no retention change to {app} in this session to undo"
                    ));
                    return;
                }
            },
            _ => None,
        };
        let label = "VersionLifecycle";
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &region,
            label,
            &app,
            &[],
        );
        let pending = self.push_pending("Set version retention", app.clone());
        let client = self.client_for_region(&region);
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let account = self.context.account_id.clone();
        let profile = self.context.profile.clone();
        tokio::spawn(async move {
            let result: Result<
                (crate::aws::VersionLifecycle, crate::aws::VersionLifecycle),
                String,
            > = async {
                let aws = client
                    .resolve()
                    .await
                    .map_err(|e| flatten_err("cached_client", e))?;
                let current = aws
                    .list_version_retention()
                    .await
                    .map_err(|e| flatten_err("list_version_retention", e))?
                    .into_iter()
                    .find(|a| a.app == app)
                    .map(|a| a.policy)
                    .unwrap_or_default();
                let next = next_policy(&command, undo_to, &current)
                    .ok_or_else(|| "nothing to set".to_string())?;
                aws.set_version_lifecycle(&app, &next)
                    .await
                    .map_err(|e| flatten_err("set_version_lifecycle", e))?;
                Ok((current, next))
            }
            .await;
            let said = result
                .as_ref()
                .map(|(was, now)| (was.describe(), now.describe()));
            let extras: Vec<(&str, &str)> = match &said {
                Ok((was, now)) => vec![("policy", now.as_str()), ("previous", was.as_str())],
                Err(_) => Vec::new(),
            };
            crate::audit::append_action_completed(
                account.as_deref(),
                profile.as_deref(),
                &region,
                label,
                &app,
                result.as_ref().map(|_| ()).map_err(|e| e.as_str()),
                &extras,
            );
            let undoing = matches!(command, LifecycleCommand::Undo);
            let _ = tx.send(AppMsg::LifecycleSet {
                gen,
                pending,
                region,
                app,
                undoing,
                result,
            });
        });
    }

    pub(super) fn handle_lifecycle_set(
        &mut self,
        pending: PendingId,
        region: String,
        app: String,
        undoing: bool,
        result: Result<(crate::aws::VersionLifecycle, crate::aws::VersionLifecycle), String>,
    ) {
        self.complete_pending(pending, result.as_ref().map(|_| ()).map_err(Clone::clone));
        match result {
            Ok((was, now)) => {
                let key = (region, app.clone());
                if undoing {
                    self.caches.lifecycle_undo.remove(&key);
                    self.push_toast(
                        ToastKind::Info,
                        format!("{app} version retention restored: {}", now.describe()),
                    );
                } else {
                    self.caches.lifecycle_undo.insert(key, was.clone());
                    self.push_toast(
                        ToastKind::Info,
                        format!(
                            "{app} version retention: {} (was {}) — `:version-lifecycle undo` \
                             restores it",
                            now.describe(),
                            was.describe()
                        ),
                    );
                }
            }
            Err(e) => self.push_toast(
                ToastKind::Error,
                format!("{app} version retention not changed: {e}"),
            ),
        }
    }

    fn show_version_lifecycle(&mut self, env: &crate::aws::Environment) {
        let app = env.application.clone();
        let title = format!("version retention — {app}");
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        if self.demo_mode {
            let body = format_version_lifecycle(&app, &Ok(None), &Ok(None));
            let _ = tx.send(AppMsg::TextOverlay { gen, title, body });
            return;
        }
        let client = self.client_for(env);
        self.status_message = Some(format!("fetching {app}'s version retention…"));
        tokio::spawn(async move {
            let body = match client.resolve().await {
                Err(e) => format!("error: {}", flatten_err("cached_client", e)),
                Ok(aws) => {
                    let (apps, quota) =
                        tokio::join!(aws.list_version_retention(), aws.version_quota());
                    let total: Option<usize> = apps
                        .as_ref()
                        .ok()
                        .map(|a| a.iter().map(|x| x.versions).sum());
                    let this = apps
                        .map(|a| a.into_iter().find(|x| x.app == app))
                        .map_err(|e| flatten_err("list_version_retention", e));
                    let quota = quota
                        .map(|q| q.zip(total))
                        .map_err(|e| flatten_err("version_quota", e));
                    format_version_lifecycle(&app, &this, &quota)
                }
            };
            let _ = tx.send(AppMsg::TextOverlay { gen, title, body });
        });
    }
}

/// `:version-lifecycle`'s overlay text. Pure. `quota` is the region's
/// (quota, versions used) when known.
pub(crate) fn format_version_lifecycle(
    app: &str,
    this: &Result<Option<crate::aws::AppRetention>, String>,
    quota: &Result<Option<(i32, usize)>, String>,
) -> String {
    let mut out = String::new();
    match this {
        Err(e) => out.push_str(&format!("could not read {app}: {e}\n")),
        Ok(None) => out.push_str(&format!("{app}: not found in this region\n")),
        Ok(Some(a)) => {
            out.push_str(&format!("versions:  {}\n", a.versions));
            out.push_str(&format!("retention: {}\n", a.policy.describe()));
            if let Some(role) = &a.policy.service_role {
                out.push_str(&format!("role:      {role}\n"));
            }
        }
    }
    match quota {
        Err(e) => out.push_str(&format!("quota:     could not read: {e}\n")),
        Ok(None) => out.push_str("quota:     (not reported)\n"),
        Ok(Some((q, used))) => out.push_str(&format!(
            "quota:     {used} of {q} versions in this region\n"
        )),
    }
    out.push_str(&format!("\n{LIFECYCLE_USAGE}\n"));
    out
}
