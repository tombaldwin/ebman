//! `:why` overlay background fetches. Six `spawn_why_red_*` helpers,
//! each fanning a single AWS read against the env's app / region:
//! events (last 50), alarms, instances, recent deploys, worker
//! queues (worker tier only), and (second-stage) DLQ message peek.
//!
//! All six follow the same pattern: snapshot a `session_id` from
//! the `Overlay::WhyRed` overlay state at spawn time, route the
//! result through a typed `AppMsg::WhyRed*` variant, and let the
//! handler in `msg.rs` write it into the overlay's slot. The
//! session_id discriminates between rapid re-opens (operator
//! closes `:why` on env A and immediately opens it on env B —
//! without the session_id check, A's fetcher could land on B's
//! overlay).
//!
//! Demo-mode short-circuits live next to each helper so VHS
//! recordings show synthetic fixture data instead of stub AWS
//! errors. The fixture flow uses `msg_tx.send` directly (no
//! `spawn_aws` indirection) since there's nothing to await.
//!
//! 0.21 lift: cluster moved out of `src/app.rs` as part of the
//! `spawn_*` clusters refactor that was deferred from 0.15/0.16/
//! 0.17/0.18/0.19/0.20.

use super::{App, AppMsg};

impl App {
    pub(crate) fn spawn_why_red_queues(&self, app_name: String, env_name: String, session_id: u64) {
        if self.demo_mode {
            let result = Ok(crate::demo_fixture::worker_queues_for_env(&env_name));
            let gen = self.generation;
            let _ = self.msg_tx.send(AppMsg::WhyRedQueues {
                gen,
                session_id,
                result,
            });
            return;
        }
        self.spawn_aws(
            "describe_worker_queues",
            move |aws| async move { aws.describe_worker_queues(&app_name, &env_name).await },
            move |gen, result| AppMsg::WhyRedQueues {
                gen,
                session_id,
                result,
            },
        );
    }

    /// Second-stage worker-queues fetch: once the queue stats land and
    /// the DLQ has visible messages, peek a few bodies so the operator
    /// sees what's failing without leaving the overlay. Uses the same
    /// `peek_messages` (visibility_timeout=5s) as the DLQ overlay — the
    /// brief invisibility is acceptable since the DLQ isn't being
    /// consumed by anyone in normal operation.
    pub(crate) fn spawn_why_red_dlq_peek(&self, dlq_url: String, session_id: u64) {
        self.spawn_aws(
            "peek_messages",
            move |aws| async move { aws.peek_messages(&dlq_url, 3).await },
            move |gen, result| AppMsg::WhyRedDlqMessages {
                gen,
                session_id,
                result,
            },
        );
    }

    pub(crate) fn spawn_why_red_events(&self, env_name: String, session_id: u64) {
        if self.demo_mode {
            let result = Ok(crate::demo_fixture::events_for_env(&env_name));
            let gen = self.generation;
            let _ = self.msg_tx.send(AppMsg::WhyRedEvents {
                gen,
                session_id,
                result,
            });
            return;
        }
        self.spawn_aws(
            "list_events_for_env",
            move |aws| async move { aws.list_events_for_env(&env_name, 50).await },
            move |gen, result| AppMsg::WhyRedEvents {
                gen,
                session_id,
                result,
            },
        );
    }

    pub(crate) fn spawn_why_red_alarms(&self, env_name: String, session_id: u64) {
        if self.demo_mode {
            let result = Ok(crate::demo_fixture::alarms_for_env(&env_name));
            let gen = self.generation;
            let _ = self.msg_tx.send(AppMsg::WhyRedAlarms {
                gen,
                session_id,
                result,
            });
            return;
        }
        self.spawn_aws(
            "list_alarms_for_env",
            move |aws| async move { aws.list_alarms_for_env(&env_name).await },
            move |gen, result| AppMsg::WhyRedAlarms {
                gen,
                session_id,
                result,
            },
        );
    }

    pub(crate) fn spawn_why_red_instances(&self, env_name: String, session_id: u64) {
        if self.demo_mode {
            let result = Ok(crate::demo_fixture::instances_for(&env_name));
            let gen = self.generation;
            let _ = self.msg_tx.send(AppMsg::WhyRedInstances {
                gen,
                session_id,
                result,
            });
            return;
        }
        self.spawn_aws(
            "list_instances",
            move |aws| async move { aws.list_instances(&env_name).await },
            move |gen, result| AppMsg::WhyRedInstances {
                gen,
                session_id,
                result,
            },
        );
    }

    pub(crate) fn spawn_why_red_deploys(&self, app_name: String, session_id: u64) {
        if self.demo_mode {
            let result = Ok(crate::demo_fixture::deploys_for_app(&app_name));
            let gen = self.generation;
            let _ = self.msg_tx.send(AppMsg::WhyRedDeploys {
                gen,
                session_id,
                result,
            });
            return;
        }
        self.spawn_aws(
            "list_application_versions",
            move |aws| async move { aws.list_application_versions(&app_name).await },
            move |gen, result| AppMsg::WhyRedDeploys {
                gen,
                session_id,
                result,
            },
        );
    }
}
