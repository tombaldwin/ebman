//! SSM Run Command: fan a shell command across an environment's
//! instances and aggregate the per-instance results.

use futures::StreamExt;

use super::*;

/// One per-instance result returned by [`AwsClient::run_shell_command`].
/// `status` is the SSM CommandInvocationStatus string verbatim
/// (`Success` / `Failed` / `Cancelled` / `TimedOut` / `Pending` /
/// `InProgress` / `Delayed`) so the renderer can colour by it. The
/// stdout / stderr buffers cap at SSM's per-invocation limit (~24 KiB
/// each by default) — anything larger is signalled via the
/// `*_url` fields on the API which we don't currently follow.
#[derive(Clone, Debug, PartialEq)]
pub struct SsmRunResult {
    pub instance_id: String,
    pub status: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl AwsClient {
    /// Send a shell command to `instance_ids` via SSM Run Command
    /// (`AWS-RunShellScript` document), poll per-instance results until
    /// every invocation reaches a terminal status, and return the
    /// aggregated rows. `wall_clock_secs` bounds total time and feeds
    /// into the SSM-side `TimeoutSeconds` parameter (the server kills
    /// commands that don't start running within that window).
    ///
    /// The poll loop sleeps 2s between cycles — the same cadence
    /// `run_insights_query` uses. SSM has no aggregate "all done" call
    /// the way Insights does, so we poll `GetCommandInvocation` per
    /// instance per cycle and drop instances out of the wait set once
    /// they're terminal. Returns once every instance is terminal *or*
    /// the wall-clock deadline has passed (operator gets best-effort
    /// partial results either way).
    pub async fn run_shell_command(
        &self,
        instance_ids: &[String],
        command: &str,
        wall_clock_secs: u64,
    ) -> Result<Vec<SsmRunResult>> {
        use aws_sdk_ssm::types::CommandInvocationStatus;
        if instance_ids.is_empty() {
            return Err(eyre!("run_shell_command: no instance ids"));
        }
        // Cap the per-command timeout at 600s (SSM's default upper
        // bound for non-MaintenanceWindow runs is 2880s; staying well
        // under it avoids surprising server-side rejections).
        let ssm_timeout = wall_clock_secs.min(600) as i32;
        let mut send = self
            .ssm()
            .send_command()
            .document_name("AWS-RunShellScript")
            .timeout_seconds(ssm_timeout)
            .parameters("commands", vec![command.to_string()]);
        for id in instance_ids {
            send = send.instance_ids(id);
        }
        let send_resp = send.send().await.wrap_err("SendCommand failed")?;
        let command_id = send_resp
            .command
            .and_then(|c| c.command_id)
            .ok_or_else(|| eyre!("SendCommand returned no command_id"))?;

        // Track which instances are still pending. SSM needs ~a second
        // before invocations become queryable; the loop's first iteration
        // tolerates an InvocationDoesNotExist error.
        let mut pending: std::collections::HashSet<String> = instance_ids.iter().cloned().collect();
        let mut completed: Vec<SsmRunResult> = Vec::new();
        // `tokio::time::Instant` (vs `std::time::Instant`) so the
        // deadline check advances with `tokio::time::pause + advance`
        // under `#[tokio::test(start_paused = true)]` — otherwise the
        // mocked clock advances tokio::sleep but not the real Instant
        // the deadline was derived from, so paused-time tests can't
        // exercise the timeout branch.
        let deadline =
            tokio::time::Instant::now() + std::time::Duration::from_secs(wall_clock_secs);
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            // Snapshot so we can mutate `pending` while walking results.
            let cycle: Vec<String> = pending.iter().cloned().collect();
            // Poll the cycle concurrently. Sequentially, one cycle cost
            // one round trip PER INSTANCE and the deadline was only
            // checked after all of them — so on a large env a single
            // cycle could outlast the operator's wall clock, and every
            // instance that hadn't happened to be polled late in that
            // cycle was written off as `TimedOut(local)` despite the
            // command still running fine. Bounded so a big fleet
            // doesn't hammer SSM's rate limit.
            const POLL_CONCURRENCY: usize = 10;
            let responses: Vec<(String, _)> = futures::stream::iter(cycle.into_iter().map(|id| {
                let command_id = command_id.clone();
                async move {
                    let resp = self
                        .ssm()
                        .get_command_invocation()
                        .command_id(&command_id)
                        .instance_id(&id)
                        .send()
                        .await;
                    (id, resp)
                }
            }))
            .buffer_unordered(POLL_CONCURRENCY)
            .collect()
            .await;
            for (id, resp) in responses {
                let invocation = match resp {
                    Ok(o) => o,
                    Err(e) => {
                        // InvocationDoesNotExist on the first cycle is
                        // expected (SSM registers invocations async) —
                        // skip and retry next cycle. Every OTHER error
                        // (AccessDenied, throttle, validation) used to
                        // fall through here too, spinning to the
                        // wall-clock deadline and reporting the
                        // permission gap as "TimedOut(local)" — the
                        // operator debugged the instances instead of
                        // IAM. Resolve those instances now with the
                        // real error.
                        let msg = format!("{e}");
                        let text = e
                            .as_service_error()
                            .map(|se| format!("{se:?}"))
                            .unwrap_or(msg);
                        if text.contains("InvocationDoesNotExist") {
                            continue;
                        }
                        pending.remove(&id);
                        completed.push(SsmRunResult {
                            instance_id: id,
                            status: "Error".into(),
                            exit_code: -1,
                            stdout: String::new(),
                            stderr: format!("GetCommandInvocation: {text}"),
                        });
                        continue;
                    }
                };
                let status = invocation.status.clone();
                let terminal = matches!(
                    status,
                    Some(CommandInvocationStatus::Success)
                        | Some(CommandInvocationStatus::Failed)
                        | Some(CommandInvocationStatus::Cancelled)
                        | Some(CommandInvocationStatus::TimedOut)
                );
                if !terminal {
                    continue;
                }
                pending.remove(&id);
                completed.push(SsmRunResult {
                    instance_id: id,
                    status: status
                        .map(|s| s.as_str().to_string())
                        .unwrap_or_else(|| "?".into()),
                    exit_code: invocation.response_code,
                    stdout: invocation.standard_output_content.unwrap_or_default(),
                    stderr: invocation.standard_error_content.unwrap_or_default(),
                });
            }
            if pending.is_empty() || tokio::time::Instant::now() >= deadline {
                break;
            }
        }
        // Anything left in `pending` after the deadline gets a synthetic
        // "TimedOut(local)" row so the operator sees which instances
        // didn't finish in the wall-clock window.
        for id in pending {
            completed.push(SsmRunResult {
                instance_id: id,
                status: "TimedOut(local)".into(),
                exit_code: -1,
                stdout: String::new(),
                stderr:
                    "ebman wall-clock timeout — instance didn't reach a terminal status in time"
                        .into(),
            });
        }
        // Sort by instance id so output is deterministic across runs.
        completed.sort_by(|a, b| a.instance_id.cmp(&b.instance_id));
        Ok(completed)
    }
}
