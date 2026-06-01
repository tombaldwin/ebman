//! Cross-region rollout background dispatch: pre-flight a region, then
//! dispatch the deploy to it. The `:rollout` state machine in `msg.rs`
//! drives these — `spawn_rollout_preflight` per region first (so the
//! operator sees which regions pass before pressing `y`), then
//! `spawn_rollout_dispatch` per region sequentially, advancing or
//! halting on each `AppMsg::RolloutDispatched` result.
//!
//! `spawn_rollout_dispatch` reuses the shared `deploy_poll::decide_poll`
//! state machine for the optional wait-for-green, the same one the
//! `ebman action deploy/rollout` CLI path uses.
//!
//! 0.21+ lift: cluster moved out of `src/app.rs` as part of the
//! `spawn_*` clusters refactor. Pure relocation; kept `pub(crate)`
//! since these are referenced by name from `msg.rs` / `cmd_action.rs`.
//! No behaviour change.

use super::{App, AppMsg};

impl App {
    /// Pre-flight one region of a rollout: construct an
    /// AwsClient with the region override, list the region's
    /// envs, check whether the target env exists, and emit
    /// `AppMsg::RolloutPreflight`. Failure modes (STS error,
    /// list_environments failure, env not found) all land as
    /// per-region row state — the operator sees which regions
    /// passed pre-flight and which need investigation before
    /// pressing `y` to dispatch.
    pub(crate) fn spawn_rollout_preflight(
        &self,
        profile: Option<String>,
        region: String,
        env_name: String,
    ) {
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            let result = match crate::aws::AwsClient::with(profile, Some(region.clone())).await {
                Ok(client) => match client.list_environments().await {
                    Ok(envs) => match envs.iter().find(|e| e.name == env_name) {
                        Some(e) => Ok(e.version_label.clone()),
                        None => Err(format!("env '{env_name}' not found in region '{region}'")),
                    },
                    Err(e) => Err(format!("list_environments: {e}")),
                },
                Err(e) => Err(format!("AwsClient::with({region}): {e}")),
            };
            let _ = tx.send(AppMsg::RolloutPreflight {
                gen,
                region,
                result,
            });
        });
    }

    /// Dispatch a single region of a rollout: construct an
    /// AwsClient with that region's override, fire
    /// `UpdateEnvironment(env, version_label)`, optionally poll
    /// for Green if `wait_for_green_secs` is set. Emits
    /// `AppMsg::RolloutDispatched` with the outcome. The handler
    /// advances the state machine (next region, or halt on
    /// failure).
    ///
    /// Reuses `deploy_settled_green` for the wait-for-green
    /// predicate. Polling cadence 5s; deadline `wait_for_green_secs`
    /// from the dispatch's start.
    pub(crate) fn spawn_rollout_dispatch(
        &self,
        profile: Option<String>,
        region: String,
        env_name: String,
        version_label: String,
        wait_for_green_secs: Option<u64>,
    ) {
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            let client = match crate::aws::AwsClient::with(profile, Some(region.clone())).await {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(AppMsg::RolloutDispatched {
                        gen,
                        region,
                        result: Err(format!("client: {e}")),
                    });
                    return;
                }
            };
            if let Err(e) = client.deploy_version(&env_name, &version_label).await {
                let _ = tx.send(AppMsg::RolloutDispatched {
                    gen,
                    region,
                    result: Err(format!("deploy_version: {e}")),
                });
                return;
            }
            if let Some(secs) = wait_for_green_secs {
                let start = tokio::time::Instant::now();
                // Always false in this path — the WaitForGreenTimeout
                // arm sends RolloutDispatched and returns
                // immediately, so the per-tick suppression never
                // fires. A future change wiring --auto-rollback for
                // TUI rollouts will need to promote this to `let mut`
                // so subsequent ticks suppress duplicate timeout
                // milestones.
                let wait_timeout_emitted = false;
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    let (status, health) = match client.list_environments().await {
                        Ok(envs) => envs
                            .iter()
                            .find(|e| e.name == env_name)
                            .map(|e| (e.status.clone(), e.health.clone()))
                            .unwrap_or_default(),
                        Err(e) => {
                            let _ = tx.send(AppMsg::RolloutDispatched {
                                gen,
                                region,
                                result: Err(format!("poll list_environments: {e}")),
                            });
                            return;
                        }
                    };
                    let elapsed = start.elapsed().as_secs();
                    match crate::deploy_poll::decide_poll(
                        &status,
                        &health,
                        elapsed,
                        Some(secs),
                        None,
                        wait_timeout_emitted,
                    ) {
                        crate::deploy_poll::PollDecision::KeepPolling => {}
                        crate::deploy_poll::PollDecision::Success => break,
                        crate::deploy_poll::PollDecision::WaitForGreenTimeout => {
                            let _ = tx.send(AppMsg::RolloutDispatched {
                                gen,
                                region,
                                result: Err(format!(
                                    "did not reach Green within {secs}s (status={status}, health={health})"
                                )),
                            });
                            return;
                        }
                        crate::deploy_poll::PollDecision::DispatchRollback => {
                            // --auto-rollback isn't wired into the
                            // TUI rollout path yet; defensive break
                            // in case a future change wires it.
                            break;
                        }
                    }
                }
            }
            let _ = tx.send(AppMsg::RolloutDispatched {
                gen,
                region,
                result: Ok(()),
            });
        });
    }
}
