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
use tui_common::TextInput;

use crate::app::Picker;
use crate::aws::Event as EbEvent;

/// The flat action list the menu shows when the operator presses `a`.
/// Ordered by daily-operator frequency: rebuild / restart land at the
/// top, terminate at the bottom so it can't be invoked by accident.
///
/// `#[non_exhaustive]` (added in 0.17.4 after the bug-hunt review
/// flagged the 0.17.3 `Action::SsmRun` addition as a SemVer-major
/// break): downstream crates matching on `Action` MUST include
/// `_ =>` so future variant additions in a patch release are
/// non-breaking. Internal match sites in this crate stay exhaustive
/// because they share the same crate and the compiler enforces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
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
    /// `:ssm-run "<shell-command>"` — fans a command across the env's
    /// instances via SSM Run Command. Routed through the standard
    /// confirm-modal flow (the command + resolved instance list ride
    /// on `ConfirmModal.ssm_run_*` / `ParameterisedAction.ssm_run_*`)
    /// so the operator gets a Y/N pre-confirm with the command and
    /// instance count before dispatch. NOT in `ACTIONS` — command-
    /// only, no menu entry. `spawn_action` short-circuits before its
    /// standard tokio::spawn body and calls `spawn_ssm_run_impl`,
    /// which surfaces results in a TextOverlay rather than the
    /// `ActionResult` toast every other variant funnels through.
    SsmRun,
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
            Self::SsmRun => "Run SSM shell command",
        }
    }
    pub fn destructive(self) -> bool {
        // SsmRun is flagged destructive so the confirm modal renders
        // in red — operator-explicit shell exec across instances is
        // treat-as-write. Operators using it for read-only probes
        // (`uptime`, `ls`) still see red; the visual prominence is
        // worth more than the false-positive on a probe.
        matches!(self, Self::Terminate | Self::SsmRun)
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
        // SsmRun deliberately opts out — no instance count / event
        // preview needed; the operator already chose the instance set
        // by opening Detail/Instances. Modal renders without dryrun /
        // events loading spinners.
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
            (IconStyle::Powerline, Self::SsmRun) => "\u{f018d}",  // console / terminal
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
            (IconStyle::Unicode, Self::SsmRun) => "▶",
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
            (IconStyle::Ascii, Self::SsmRun) => "$",
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
// ConfirmModal has grown along with the action surface (deploy
// preview + watchdogs + watching deploys + dry-run + recent
// events). Boxing the variant would require touching every
// pattern-match site for ~zero perceptible runtime benefit at
// the modal's once-per-action allocation cadence. Allow rather
// than absorb the churn.
#[allow(clippy::large_enum_variant)]
pub enum ActionFlow {
    Menu {
        list_state: ListState,
    },
    SwapTarget {
        source: String,
        picker: Picker,
    },
    Confirm(ConfirmModal),
    /// `:rollout LABEL --regions r1,r2,r3` opens this flow. It's
    /// a state-machine of its own (Planning → AwaitingConfirm →
    /// Dispatching → Done) — different shape from the standard
    /// per-env ConfirmModal because the rollout fans out across
    /// regions, with one row per region in the overlay and per-
    /// region async preflight + dispatch.
    Rollout(RolloutFlow),
}

/// Cross-region rollout state machine for the TUI surface.
/// Distinct from `RolloutFlow::AwaitingConfirm`'s
/// `current_version` data — the modal renders a row per region
/// throughout the lifecycle, just with different content per
/// state. Each state transition is driven by an `AppMsg::Rollout*`
/// variant.
#[derive(Clone, Debug)]
pub struct RolloutFlow {
    /// Stable correlation id written into every per-region
    /// audit-log line. Same shape `ebman action rollout` uses
    /// (`rollout-YYYYMMDDTHHMMSSZ`) so audit-log grep finds
    /// CLI + TUI rollouts identically.
    pub rollout_id: String,
    pub env_name: String,
    pub version_label: String,
    /// Per-region rows, in dispatch order (= the order the
    /// operator supplied via `--regions`).
    pub regions: Vec<RolloutRegion>,
    pub state: RolloutState,
    /// Operator-supplied flags carried through to the dispatch
    /// phase. `wait_for_green_secs` is per-region; the inner
    /// poll loop reuses `decide_poll` from main.rs's CLI path.
    pub wait_for_green_secs: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct RolloutRegion {
    pub region: String,
    /// `None` until pre-flight lands. `Some("")` means the env
    /// was found but has no version deployed yet; `Some(label)`
    /// means it's currently on that label.
    pub current_version: Option<String>,
    /// `None` while planning. `Some(true)` if the same-name env
    /// was found in this region's fleet; `Some(false)` if not
    /// (rollout halts before dispatching anywhere in that case
    /// — pre-flight gate).
    pub env_found: Option<bool>,
    /// Pre-flight error message, when one occurred (STS / list
    /// failed). Surfaces in the plan table so the operator can
    /// see which region needs investigation.
    pub preflight_error: Option<String>,
    /// Dispatch outcome (populated in `Dispatching` state).
    /// `None` until that region's turn; `Some(Ok(()))` on a
    /// successful UpdateEnvironment + (if asked) Green; `Some(Err(...))`
    /// on either dispatch failure or wait-for-green timeout.
    pub outcome: Option<Result<(), String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RolloutState {
    /// Pre-flight fetches in flight. Plan rows render with
    /// `…` placeholders. No keys do anything except `esc` /
    /// `q` (cancel before any region is touched).
    Planning,
    /// All pre-flights landed. At least one region passed; the
    /// operator can press `y` to dispatch. Failed pre-flights
    /// render with their error; the operator can `esc` and
    /// retry after fixing them.
    AwaitingConfirm,
    /// `y` was pressed; dispatching regions sequentially. The
    /// `next_index` tracks which region the dispatch loop is on.
    /// Regions before `next_index` have outcomes; regions at or
    /// after are pending. Outer halt fires if a region's outcome
    /// is `Err` (stop on first failure).
    Dispatching { next_index: usize },
    /// All regions either dispatched or skipped (post-halt). The
    /// overlay shows the final report; `esc` / `q` closes.
    Done,
}

#[derive(Clone)]
pub struct ConfirmModal {
    pub action: Action,
    pub target_env: String,
    pub swap_with: Option<String>,
    pub typed: TextInput,
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
    /// Pre-deploy preview body — formatted output of
    /// `format_deploy_preview` (label / age / description /
    /// rollback-warning when candidate is older than current).
    /// Populated by `spawn_version_preview` after the modal opens
    /// for `Action::Deploy`, so the operator sees what they're
    /// about to ship inline rather than having to dispatch
    /// `:deploy LABEL --preview` separately first. `None` for
    /// every other action.
    pub version_preview: Option<String>,
    /// True while `spawn_version_preview` is in flight — the UI
    /// shows a `fetching version metadata…` placeholder until the
    /// fetch lands.
    pub loading_version_preview: bool,
    /// Result of the pre-deploy health-check probe. Same shape as
    /// the other pre-flight fields — None while in flight or
    /// never run, Some(Ok(())) when the probe was 2xx (silence is
    /// golden; nothing rendered), Some(Err(reason)) when the
    /// probe failed (non-2xx, timeout, connection error) and the
    /// modal should render a yellow warning. The probe doesn't
    /// block the deploy either way; it's a heads-up that the
    /// footgun auto-rollback was built to rescue from is live.
    pub health_check_probe: Option<Result<(), String>>,
    /// True while `spawn_health_check_probe` is in flight.
    pub loading_health_check: bool,
    /// Pre-rendered "deploy plan: POLICY → max X/Y instances
    /// unavailable" line, plus a caution flag for colouring (true
    /// = render in yellow; false = green/muted — no capacity
    /// impact). Populated by `spawn_unavailability_estimate` from
    /// the env's deployment-policy + batch + ASG max-size option-
    /// settings. None while loading or for non-Deploy actions.
    pub unavailability_line: Option<(String, bool)>,
    /// True while `spawn_unavailability_estimate` is in flight.
    pub loading_unavailability: bool,
    /// Lint findings against the env's pre-write state, surfaced
    /// inline in the modal as additional warning lines.
    /// Generalises the health-check probe + unavailability pill
    /// pattern — anything the lint engine flags at `>= Warn` is
    /// rendered as a yellow caution line so the operator sees
    /// risk before confirming, not after the deploy goes bad.
    /// `None` while loading; populated by `spawn_confirm_lint` at
    /// modal-open time.
    pub lint_issues: Option<Vec<crate::lint::Issue>>,
    /// True while `spawn_confirm_lint` is in flight.
    pub loading_lint: bool,
    /// Shell command to run when `action == SsmRun`. The trimmed
    /// form (no surrounding quotes) — the original quote-handling
    /// happens in `cmd_ssm_run` before the modal opens.
    pub ssm_run_command: Option<String>,
    /// Resolved target instance IDs when `action == SsmRun`. Pre-
    /// resolved at modal-open time from `Detail.instances`; this
    /// way the operator sees the fan-out count in the confirm copy
    /// and the dispatch path is straight-forward.
    pub ssm_run_instances: Option<Vec<String>>,
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
    /// SSM-only: shell command (trimmed of surrounding quotes).
    /// Set by `cmd_ssm_run` before routing through
    /// `open_parameterised_action`. None for every other action.
    pub ssm_run_command: Option<String>,
    /// SSM-only: pre-resolved target instance IDs (from cached
    /// `Detail.instances`). Lets the confirm modal render the
    /// fan-out count without re-fetching. None for every other
    /// action.
    pub ssm_run_instances: Option<Vec<String>>,
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
