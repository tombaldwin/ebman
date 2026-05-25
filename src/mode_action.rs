//! Modal action flow — the `:a` actions menu and everything it spawns.
//!
//! This module owns the state-machine types for the action-menu screen
//! that walks an operator through Rebuild / Restart / Swap / Terminate /
//! Deploy / Upgrade / Clone / Scale / Abort. The actual *dispatch* logic
//! — the `spawn_action` / `advance_action_flow` / `handle_action_key`
//! methods — still lives on [`crate::app::App`] because every step
//! reaches into shared App state (AwsClient, pending-actions panel,
//! audit log, status toasts). The split here is "data + helpers" vs
//! "App-coupled control flow": the move that fits in one commit
//! without pulling half of `app.rs` along.
//!
//! Types exported via `pub use crate::mode_action::*` from `app.rs` so
//! `ui.rs` and the rest of the crate keep their existing import paths.
//! When a future commit splits the App-coupled methods, the move target
//! is `impl ActionMode { fn handle_key(app: &mut App, …) }`-style
//! wrappers in this file.

use ratatui::widgets::ListState;

use crate::app::Picker;
use crate::aws::Event as EbEvent;

/// The flat action list the menu shows when the operator presses `a`.
/// Ordered by daily-operator frequency: rebuild / restart land at the
/// top, terminate at the bottom so it can't be invoked by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Rebuild,
    RestartAppServer,
    SwapCnames,
    Terminate,
    /// Deploy a specific application-version label to the env.
    Deploy,
    /// Migrate the env to a different platform ARN (rolling).
    UpgradePlatform,
    /// Clone the env into a new one with a chosen name.
    Clone,
    /// Set ASG min/max so the env scales to a fixed count.
    Scale,
    /// Open the modal form to edit MinSize / MaxSize / InstanceType /
    /// Cooldown in one shot. Richer surface than `Scale` for the cases
    /// where the operator wants more than just min=max=N.
    Capacity,
    /// Cancel an in-flight environment update.
    AbortUpdate,
    /// `CreateConfigurationTemplate` from the selected env.
    ConfigSave,
    /// `DeleteConfigurationTemplate`.
    ConfigDelete,
    /// `UpdateEnvironment(template_name)` — apply a saved template.
    ConfigApply,
    /// `ec2:TerminateInstances` against a single instance picked from the
    /// Instances tab. ASG replaces it; not env-level.
    TerminateInstance,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Self::Rebuild => "Rebuild env",
            Self::RestartAppServer => "Restart app server",
            Self::SwapCnames => "Swap CNAMEs with another env",
            Self::Terminate => "Terminate env",
            Self::Deploy => "Deploy application version",
            Self::UpgradePlatform => "Upgrade platform",
            Self::Clone => "Clone env",
            Self::Scale => "Scale (min/max)",
            Self::Capacity => "Capacity (min/max/instance/cooldown)",
            Self::AbortUpdate => "Abort current update",
            Self::ConfigSave => "Save configuration template",
            Self::ConfigDelete => "Delete configuration template",
            Self::ConfigApply => "Apply configuration template",
            Self::TerminateInstance => "Terminate instance",
        }
    }
    pub fn destructive(self) -> bool {
        matches!(self, Self::Terminate)
    }

    /// Whether the confirm modal should fetch `DescribeInstancesHealth`
    /// (instance / AZ impact preview) and the last 3 EB events before
    /// authorising. Actions that touch instances or roll capacity opt in;
    /// actions that don't (AbortUpdate, swap CNAMEs at the LB layer,
    /// ConfigDelete on a template definition) opt out.
    ///
    /// This is the single source of truth — every `ConfirmModal`
    /// construction site reads it instead of hand-rolling its own
    /// allow-list.
    pub fn wants_preflight(self) -> bool {
        matches!(
            self,
            Self::Deploy
                | Self::UpgradePlatform
                | Self::Scale
                | Self::Clone
                | Self::Rebuild
                | Self::RestartAppServer
                | Self::SwapCnames
                | Self::Terminate
                | Self::ConfigApply
        )
    }

    /// Per-icon-style glyph for the action menu. Powerline mode draws
    /// recognisable Nerd Font icons (refresh / power / swap / trash / …)
    /// next to the label; unicode falls back to short emoji-ish glyphs;
    /// ASCII gets a single-letter tag so the column stays aligned.
    pub fn glyph(self, icons: crate::theme::IconStyle) -> &'static str {
        use crate::theme::IconStyle;
        match (icons, self) {
            // Powerline / Nerd Font Material Design glyphs.
            (IconStyle::Powerline, Self::Rebuild) => "\u{f0450}", // refresh
            (IconStyle::Powerline, Self::RestartAppServer) => "\u{f0709}", // restart
            (IconStyle::Powerline, Self::SwapCnames) => "\u{f0521}", // swap-horizontal
            (IconStyle::Powerline, Self::Terminate) => "\u{f01b4}", // delete / trash
            (IconStyle::Powerline, Self::Deploy) => "\u{f01da}",  // upload
            (IconStyle::Powerline, Self::UpgradePlatform) => "\u{f0140}", // upgrade arrow
            (IconStyle::Powerline, Self::Clone) => "\u{f018f}",   // content-copy
            (IconStyle::Powerline, Self::Scale) => "\u{f0566}",   // resize / arrow-expand
            (IconStyle::Powerline, Self::Capacity) => "\u{f0493}", // tune / sliders
            (IconStyle::Powerline, Self::AbortUpdate) => "\u{f0156}", // cancel
            (IconStyle::Powerline, Self::ConfigSave) => "\u{f0193}", // content-save
            (IconStyle::Powerline, Self::ConfigDelete) => "\u{f01b4}", // delete
            (IconStyle::Powerline, Self::ConfigApply) => "\u{f00e2}", // check
            (IconStyle::Powerline, Self::TerminateInstance) => "\u{f01b4}", // delete
            // Unicode fallbacks — common monospaced glyphs that render in
            // most modern terminals without a patched font.
            (IconStyle::Unicode, Self::Rebuild) => "↻",
            (IconStyle::Unicode, Self::RestartAppServer) => "⟳",
            (IconStyle::Unicode, Self::SwapCnames) => "⇄",
            (IconStyle::Unicode, Self::Terminate) => "✗",
            (IconStyle::Unicode, Self::Deploy) => "↑",
            (IconStyle::Unicode, Self::UpgradePlatform) => "▲",
            (IconStyle::Unicode, Self::Clone) => "❐",
            (IconStyle::Unicode, Self::Scale) => "↔",
            (IconStyle::Unicode, Self::Capacity) => "⚙",
            (IconStyle::Unicode, Self::AbortUpdate) => "■",
            (IconStyle::Unicode, Self::ConfigSave) => "✎",
            (IconStyle::Unicode, Self::ConfigDelete) => "✗",
            (IconStyle::Unicode, Self::ConfigApply) => "✓",
            (IconStyle::Unicode, Self::TerminateInstance) => "✗",
            // ASCII: single-letter tags so the menu column stays fixed-width.
            (IconStyle::Ascii, Self::Rebuild) => "R",
            (IconStyle::Ascii, Self::RestartAppServer) => "r",
            (IconStyle::Ascii, Self::SwapCnames) => "S",
            (IconStyle::Ascii, Self::Terminate) => "X",
            (IconStyle::Ascii, Self::Deploy) => "D",
            (IconStyle::Ascii, Self::UpgradePlatform) => "U",
            (IconStyle::Ascii, Self::Clone) => "C",
            (IconStyle::Ascii, Self::Scale) => "N",
            (IconStyle::Ascii, Self::Capacity) => "M",
            (IconStyle::Ascii, Self::AbortUpdate) => "A",
            (IconStyle::Ascii, Self::ConfigSave) => "s",
            (IconStyle::Ascii, Self::ConfigDelete) => "d",
            (IconStyle::Ascii, Self::ConfigApply) => "a",
            (IconStyle::Ascii, Self::TerminateInstance) => "x",
        }
    }
}

/// Order the action menu renders rows in. `Rebuild` / `Restart` are at
/// the top because they're the daily-driver actions; `Terminate` lives
/// at the bottom so the operator can't ⌘+enter it by accident.
pub const ACTIONS: &[Action] = &[
    Action::Rebuild,
    Action::RestartAppServer,
    Action::Deploy,
    Action::UpgradePlatform,
    Action::Scale,
    Action::Capacity,
    Action::Clone,
    Action::SwapCnames,
    Action::AbortUpdate,
    Action::Terminate,
];

/// One step of the action flow. Owned by `App.action_flow` while the
/// `:a` menu / confirm screens are up; replaced on each step
/// transition and cleared when the operator backs out.
///
/// Note: the `Running` variant used to exist for the brief
/// "dispatching…" modal between Y-confirm and the SDK call landing.
/// The cancel-window design (5s undo via `U` after a confirm) closes
/// the action flow immediately on Y-confirm — the pending-dispatch
/// pill in the header carries the same signal, so the central modal
/// became dead weight.
pub enum ActionFlow {
    Menu { list_state: ListState },
    SwapTarget { source: String, picker: Picker },
    Confirm(ConfirmModal),
}

#[derive(Clone)]
pub struct ConfirmModal {
    pub action: Action,
    pub target_env: String,
    pub swap_with: Option<String>,
    pub typed: String,
    pub kind: ConfirmKind,
    pub dryrun: Option<DryRunInfo>,
    pub loading_dryrun: bool,
    pub recent_events: Option<Vec<EbEvent>>,
    pub loading_events: bool,
    /// One-line pre-flight warning derived from the env's current state when
    /// the modal opened — e.g. "ACTIVE DEPLOY: status=Updating" or "RECENT
    /// CHANGE: updated 4m ago". Surfaced above the Y/N row so the operator
    /// sees mid-flight changes before authorising another action.
    pub traffic_warning: Option<String>,
    /// Version label to deploy when `action == Deploy`. None for other actions.
    pub deploy_version: Option<String>,
    /// Platform ARN to migrate to when `action == UpgradePlatform`.
    pub upgrade_platform_arn: Option<String>,
    /// Human-readable label for the upgrade target — shown in the modal.
    pub upgrade_platform_label: Option<String>,
    /// New env name when `action == Clone`.
    pub clone_target: Option<String>,
    /// Desired min/max instance counts when `action == Scale`. Both set to
    /// the same value for a "scale to N" operation; differ for explicit
    /// min/max overrides.
    pub scale_min: Option<i32>,
    pub scale_max: Option<i32>,
    /// Auto-rollback watchdog deadline in seconds (Deploy only). When
    /// `Some(N)`, a background task fires N seconds after dispatch
    /// and — if the env hasn't reached Green by then — automatically
    /// redeploys the captured `DeploySnapshot`'s previous version.
    /// `None` is the default; the operator opts in with
    /// `:deploy LABEL --auto-rollback 5m`.
    pub auto_rollback_secs: Option<u64>,
    /// Wait-for-green deadline in seconds (Deploy only). When
    /// `Some(N)`, a watcher is armed at dispatch and `apply_refresh`
    /// emits a pinned status (success on Green, error on timeout)
    /// the next time the env's health is observed. Orthogonal to
    /// `auto_rollback_secs` — both flags can be set on the same
    /// deploy. The operator opts in with
    /// `:deploy LABEL --wait-for-green 5m`.
    pub wait_for_green_secs: Option<u64>,
}

/// Helper carrying the optional parameters needed by the new parameterised
/// actions. Avoids passing seven `Option<…>` args to `open_parameterised_action`.
#[derive(Default, Clone, Debug)]
pub struct ParameterisedAction {
    pub deploy_version: Option<String>,
    pub upgrade_platform_arn: Option<String>,
    pub upgrade_platform_label: Option<String>,
    pub clone_target: Option<String>,
    pub scale_min: Option<i32>,
    pub scale_max: Option<i32>,
    /// CNAME-swap target. Only meaningful for `Action::SwapCnames`.
    pub swap_with: Option<String>,
    /// Deploy-only auto-rollback deadline in seconds. See
    /// `ConfirmModal::auto_rollback_secs` for the contract.
    pub auto_rollback_secs: Option<u64>,
    /// Deploy-only wait-for-green deadline in seconds. See
    /// `ConfirmModal::wait_for_green_secs` for the contract.
    pub wait_for_green_secs: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct DryRunInfo {
    pub instance_count: usize,
    pub az_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKind {
    YesNo,
    TypeName, // user must type the env name exactly
}
