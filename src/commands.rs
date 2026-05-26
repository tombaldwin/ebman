//! Single source of truth for `:command` metadata.
//!
//! Each entry in [`COMMANDS`] carries the name + aliases, usage hint,
//! one-line help, category bucket, and palette behaviour for one
//! command. Three consumers read this registry:
//!
//!   1. `app::BUILTIN_COMMANDS` — flat name+aliases list used by plugin
//!      loading to detect collisions with built-in commands.
//!   2. `build_palette_items` (`Ctrl-K` fuzzy finder) — turns each
//!      [`CommandKind::ZeroArg`] into a `RunCommand` palette item and
//!      each [`CommandKind::Prefill`] into a `PrefillCommand` one.
//!   3. `draw_help` (global `?` screen) — groups by [`Category`] to
//!      render the command reference.
//!
//! Adding a command means adding one entry here, wiring the dispatch in
//! `app::execute_command` (or the relevant `app/cmd_*.rs` sub-module),
//! and nothing else. The CI test
//! `commands_registry_covers_every_dispatch_arm` fails if the registry
//! and the dispatch site drift apart.

/// Logical grouping for the global help screen. Order here matches the
/// section order rendered in `draw_help`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    /// View / filter / sort / persistence / cosmetic toggles.
    View,
    /// Region / profile / account switchers + cross-account scan.
    Navigation,
    /// Per-env inspection overlays (`:why`, `:diff`, `:alarms`, ...).
    Inspection,
    /// Lifecycle actions (deploy / rebuild / restart / terminate / ...).
    Lifecycle,
    /// Env configuration writes (env vars, tags, option-settings).
    EnvConfig,
    /// Versions / saved-config templates / alarms / custom platforms.
    VersionsConfigsAlarms,
    /// `space`-multi-select-driven batch ops.
    BulkOps,
    /// Setup, discovery, and bookkeeping.
    Setup,
}

impl Category {
    /// Section title rendered in the global help. Kept short so the
    /// header line doesn't wrap on narrow terminals.
    pub fn label(self) -> &'static str {
        match self {
            Self::View => "View / filter / sort",
            Self::Navigation => "Multi-account / multi-region",
            Self::Inspection => "Per-env inspection",
            Self::Lifecycle => "Lifecycle actions",
            Self::EnvConfig => "Env config",
            Self::VersionsConfigsAlarms => "Versions / configs / alarms / platforms",
            Self::BulkOps => "Bulk ops (space to multi-select first)",
            Self::Setup => "Setup / discovery",
        }
    }

    /// Iteration order used by `draw_help` — most-frequent surfaces first.
    pub const ORDER: &'static [Self] = &[
        Self::Navigation,
        Self::Inspection,
        Self::Lifecycle,
        Self::EnvConfig,
        Self::VersionsConfigsAlarms,
        Self::BulkOps,
        Self::View,
        Self::Setup,
    ];
}

/// How the palette (`Ctrl-K`) should treat this command on Enter.
/// Two variants today — `Hidden` was speculative and removed to satisfy
/// the dead-code lint; re-add when there's a concrete plugin / overlay
/// command that wants registry metadata without palette surfacing.
#[derive(Debug, Clone, Copy)]
pub enum CommandKind {
    /// No arguments — palette Enter dispatches immediately.
    ZeroArg,
    /// Takes arguments — palette Enter switches to command-bar mode
    /// pre-filled with the given prefix (usually `"name "`). The user
    /// types the rest and presses Enter to run.
    Prefill(&'static str),
}

/// Static metadata for one built-in command.
#[derive(Debug, Clone, Copy)]
pub struct CommandSpec {
    /// Canonical name (without the leading `:`).
    pub name: &'static str,
    /// Alternate names that dispatch to the same handler. Used by
    /// `BUILTIN_COMMANDS` collision detection.
    pub aliases: &'static [&'static str],
    /// One-line description shown in the help screen and the palette.
    pub help: &'static str,
    /// Logical grouping for `draw_help`.
    pub category: Category,
    /// Palette behaviour on Enter.
    pub kind: CommandKind,
}

/// Static helper for declaring entries without the per-line ceremony.
const fn cmd(
    name: &'static str,
    help: &'static str,
    category: Category,
    kind: CommandKind,
) -> CommandSpec {
    CommandSpec {
        name,
        aliases: &[],
        help,
        category,
        kind,
    }
}

const fn cmd_with_aliases(
    name: &'static str,
    aliases: &'static [&'static str],
    help: &'static str,
    category: Category,
    kind: CommandKind,
) -> CommandSpec {
    CommandSpec {
        name,
        aliases,
        help,
        category,
        kind,
    }
}

/// The whole registry. Adding / removing a command means changing one
/// entry here; the palette, the help screen, and the plugin-collision
/// detector all read from this slice.
pub const COMMANDS: &[CommandSpec] = &[
    // ── Navigation / multi-account ───────────────────────────────────────
    cmd_with_aliases(
        "region",
        &["r"],
        ":region NAME / :region all — switch region, or fan across configured regions",
        Category::Navigation,
        CommandKind::Prefill("region "),
    ),
    cmd_with_aliases(
        "profile",
        &["p"],
        ":profile NAME — switch AWS profile",
        Category::Navigation,
        CommandKind::Prefill("profile "),
    ),
    cmd(
        "account",
        ":account NAME — switch to AssumeRole account (config.toml accounts.NAME); falls back to :profile aliasing",
        Category::Navigation,
        CommandKind::Prefill("account "),
    ),
    cmd(
        "accounts",
        ":accounts — list AWS-Organizations child accounts (annotated with :account hints)",
        Category::Navigation,
        CommandKind::ZeroArg,
    ),
    cmd(
        "find-env",
        ":find-env SUBSTRING — scan every ~/.aws profile + AssumeRole account",
        Category::Navigation,
        CommandKind::Prefill("find-env "),
    ),
    cmd(
        "envs-by-version",
        ":envs-by-version LABEL — fleet-wide blast-radius for a bad build",
        Category::Navigation,
        CommandKind::Prefill("envs-by-version "),
    ),
    cmd(
        "logs-insights",
        ":logs-insights [--window 30m|1h|6h|24h|7d] QUERY — run a CW Logs Insights query against the env's log groups",
        Category::Inspection,
        CommandKind::Prefill("logs-insights "),
    ),
    cmd(
        "org-health",
        ":org-health — aggregate env / red counts per profile + AssumeRole account",
        Category::Navigation,
        CommandKind::ZeroArg,
    ),
    // ── Inspection ───────────────────────────────────────────────────────
    cmd_with_aliases(
        "why",
        &["diagnose"],
        ":why — diagnostic overlay (events + alarms + instances + recent deploys)",
        Category::Inspection,
        CommandKind::ZeroArg,
    ),
    cmd(
        "diff",
        ":diff NAME — side-by-side env comparison vs selected env;  :diff ENV-A ENV-B names both explicitly",
        Category::Inspection,
        CommandKind::Prefill("diff "),
    ),
    cmd_with_aliases(
        "resources",
        &["res"],
        ":resources / :res — DescribeEnvironmentResources dump for selected env",
        Category::Inspection,
        CommandKind::ZeroArg,
    ),
    cmd(
        "alarms",
        ":alarms — CloudWatch alarms attached to selected env",
        Category::Inspection,
        CommandKind::ZeroArg,
    ),
    cmd(
        "versions",
        ":versions — application versions for selected env's app (deploy hint included)",
        Category::Inspection,
        CommandKind::ZeroArg,
    ),
    cmd_with_aliases(
        "saved-configs",
        &["configs"],
        ":saved-configs / :configs — EB saved configuration templates per application",
        Category::Inspection,
        CommandKind::ZeroArg,
    ),
    cmd_with_aliases(
        "custom-platforms",
        &["platforms"],
        ":custom-platforms / :platforms — custom EB platforms in this account/region",
        Category::Inspection,
        CommandKind::ZeroArg,
    ),
    cmd(
        "plugins",
        ":plugins — list user plugin commands defined in commands.toml",
        Category::Inspection,
        CommandKind::ZeroArg,
    ),
    cmd(
        "apps-info",
        ":apps-info — application metadata overlay (description / dates / templates / envs)",
        Category::Inspection,
        CommandKind::ZeroArg,
    ),
    cmd(
        "listeners",
        ":listeners — ALB listener config (per-port: proto / cert / SSL policy / default rule). Web-tier only.",
        Category::Inspection,
        CommandKind::ZeroArg,
    ),
    cmd(
        "listener-edit",
        ":listener-edit PORT — modal cert picker for an ALB listener: pick from the region's ACM certificates (loaded live), pre-selected with the listener's current certs. PORT = 443 / numeric / default.",
        Category::Lifecycle,
        CommandKind::Prefill("listener-edit "),
    ),
    cmd(
        "rds",
        ":rds — RDS instance config attached to the env (engine / class / credentials). Password is redacted.",
        Category::Inspection,
        CommandKind::ZeroArg,
    ),
    cmd(
        "rds-attach",
        ":rds-attach — modal form: couple an RDS instance to the env (engine / class / storage / credentials / deletion policy / Multi-AZ). Pre-fills if one is already attached.",
        Category::Lifecycle,
        CommandKind::ZeroArg,
    ),
    cmd(
        "rds-detach",
        ":rds-detach ENV — safe-ify the coupled RDS: sets DBDeletionPolicy=Snapshot so the DB survives env termination. Repeat the env name to confirm. Does not decouple the DB (EB has no detach op).",
        Category::Lifecycle,
        CommandKind::Prefill("rds-detach "),
    ),
    cmd(
        "options",
        ":options [NAMESPACE] — full settable-option vocabulary for the env's platform (current value + default + type + constraints). Slow.",
        Category::Inspection,
        CommandKind::Prefill("options "),
    ),
    cmd(
        "config-diff",
        ":config-diff ENV — compare the selected env's option-settings against ENV's; shows every setting that differs",
        Category::Inspection,
        CommandKind::Prefill("config-diff "),
    ),
    cmd(
        "config-diff-local",
        ":config-diff-local [NAME] — diff the deployed env against a local EB CLI saved config under `.elasticbeanstalk/saved_configs/`. No arg auto-picks the lone file; with multiple, name one.",
        Category::Inspection,
        CommandKind::Prefill("config-diff-local "),
    ),
    cmd(
        "changes",
        ":changes — deploy + config-change timeline for the selected env (from its event history, newest first)",
        Category::Inspection,
        CommandKind::ZeroArg,
    ),
    cmd(
        "lineage",
        ":lineage — deploy-only timeline for the selected env: one row per version label, newest first, with Δ between consecutive deploys",
        Category::Inspection,
        CommandKind::ZeroArg,
    ),
    cmd(
        "ssh",
        ":ssh [i-abc] — open an SSM Session Manager session into an env instance. With an instance ID, targets it directly; without, opens a picker over cached Detail/Instances. Needs `aws` CLI + session-manager-plugin on PATH.",
        Category::Inspection,
        CommandKind::Prefill("ssh "),
    ),
    cmd(
        "ssm-run",
        ":ssm-run \"<shell-command>\" — fan a shell command across the env's instances via SSM Run Command (AWS-RunShellScript). Targets are sourced from cached Detail/Instances. 60s wall-clock cap. Gated by read-only / per-env safety pin.",
        Category::Inspection,
        CommandKind::Prefill("ssm-run "),
    ),
    cmd(
        "explain",
        ":explain — diagnose the last IAM AccessDenied via iam:SimulatePrincipalPolicy.  :explain ARN ACTION evaluates explicit pairs.",
        Category::Inspection,
        CommandKind::Prefill("explain "),
    ),
    cmd(
        "env-edit",
        ":env-edit — bulk env-var editor: opens current env vars in $EDITOR (KEY=VALUE), diffs + dispatches on save",
        Category::EnvConfig,
        CommandKind::ZeroArg,
    ),
    cmd(
        "secrets",
        ":secrets [FILTER] — browse Secrets Manager (region-scoped). Metadata only — values stay hidden until :secret NAME.",
        Category::Inspection,
        CommandKind::Prefill("secrets "),
    ),
    cmd(
        "secret",
        ":secret NAME — fetch one secret's value (CloudTrail-audited). Respects :redact. Use :secrets to find the name.",
        Category::Inspection,
        CommandKind::Prefill("secret "),
    ),
    cmd(
        "report-bug",
        ":report-bug — scrubbed bug-report overlay. y = copy, b = open GitHub issue with body pre-filled. No outbound HTTP from ebman.",
        Category::Setup,
        CommandKind::ZeroArg,
    ),
    cmd(
        "cost",
        ":cost on | off | status — toggle the COST column ($/month per env via Cost Explorer; 24h cache)",
        Category::View,
        CommandKind::Prefill("cost "),
    ),
    cmd(
        "history",
        ":history — show recent info/error messages",
        Category::Inspection,
        CommandKind::ZeroArg,
    ),
    cmd_with_aliases(
        "pending",
        &["in-flight", "inflight"],
        ":pending / :in-flight — in-flight + recently-completed actions",
        Category::Inspection,
        CommandKind::ZeroArg,
    ),
    cmd_with_aliases(
        "rollbacks-armed",
        &["rb-armed"],
        ":rollbacks-armed / :rb-armed — currently-armed `--auto-rollback` watchdogs (env, rollback target, time-to-deadline)",
        Category::Inspection,
        CommandKind::ZeroArg,
    ),
    cmd(
        "abort-rollback",
        ":abort-rollback [ENV] — disarm an armed `--auto-rollback` watchdog (no arg drains all in the current context)",
        Category::Lifecycle,
        CommandKind::Prefill("abort-rollback "),
    ),
    cmd(
        "freeze-deploys",
        ":freeze-deploys [REASON…] — session-scoped fleet-wide write-lock; every destructive op refuses while frozen. Useful during incident triage to prevent accidental deploys. Cleared by :thaw-deploys or by exiting ebman. Re-issue to update the reason in place.",
        Category::Lifecycle,
        CommandKind::Prefill("freeze-deploys "),
    ),
    cmd(
        "thaw-deploys",
        ":thaw-deploys — clear the session-scoped freeze set by :freeze-deploys",
        Category::Lifecycle,
        CommandKind::ZeroArg,
    ),
    cmd(
        "undo",
        ":undo — reverse the most-recent option-settings write (10-entry ring buffer; :undo of :undo redoes the original)",
        Category::EnvConfig,
        CommandKind::ZeroArg,
    ),
    cmd(
        "lint",
        ":lint [ENV] — run the diagnostic rule engine against the selected env (or named env). Surfaces AllAtOnce-on-multi-instance, missing health-check URL, env Red >4h, batch-size > max-size, single-instance prod, low cooldown. Operator-tunable via lint.disable in config.toml.",
        Category::Inspection,
        CommandKind::Prefill("lint "),
    ),
    // ── Lifecycle actions ────────────────────────────────────────────────
    cmd(
        "rebuild",
        ":rebuild — terminate + recreate every instance (Y/N confirm)",
        Category::Lifecycle,
        CommandKind::ZeroArg,
    ),
    cmd(
        "restart",
        ":restart — restart app server on every instance (Y/N confirm)",
        Category::Lifecycle,
        CommandKind::ZeroArg,
    ),
    cmd(
        "terminate",
        ":terminate — TERMINATE env (typed-name confirm; irreversible)",
        Category::Lifecycle,
        CommandKind::ZeroArg,
    ),
    cmd(
        "deploy",
        ":deploy LABEL [--preview] [--auto-rollback Nm] [--wait-for-green Nm]  |  :deploy --from PATH [--label L] [--describe D] [--no-deploy]  — auto-rollback arms a watchdog that redeploys the captured pre-deploy snapshot if the env doesn't reach Green within N minutes; wait-for-green pins a success/timeout status when the deploy resolves",
        Category::Lifecycle,
        CommandKind::Prefill("deploy "),
    ),
    cmd(
        "promote-env",
        ":promote-env SOURCE TARGET [--auto-rollback Nm] [--wait-for-green Nm] — ship SOURCE's currently-deployed version label to TARGET in one dispatch; opens the deploy confirm on TARGET with the same watchdog/wait flags as :deploy. Refuses if SOURCE has no version, or if SOURCE's version is already on TARGET.",
        Category::Lifecycle,
        CommandKind::Prefill("promote-env "),
    ),
    cmd(
        "rollback",
        ":rollback [--to LABEL] [--auto-rollback Nm] — redeploy the previous version. No arg uses the captured pre-deploy snapshot (falls back to event-history scan). --to LABEL targets a specific version. --auto-rollback arms a roll-forward watchdog.",
        Category::Lifecycle,
        CommandKind::Prefill("rollback "),
    ),
    cmd(
        "upgrade",
        ":upgrade [ARN] — no-arg: list compatible platforms; with ARN: migrate to it",
        Category::Lifecycle,
        CommandKind::Prefill("upgrade "),
    ),
    cmd(
        "clone",
        ":clone NEW-NAME — clone selected env",
        Category::Lifecycle,
        CommandKind::Prefill("clone "),
    ),
    cmd(
        "scale",
        ":scale N — set ASG min=max=N (use :stop for 0, :start for 1)",
        Category::Lifecycle,
        CommandKind::Prefill("scale "),
    ),
    cmd(
        "stop",
        ":stop — ASG min=max=0 (Y/N confirm)",
        Category::Lifecycle,
        CommandKind::ZeroArg,
    ),
    cmd(
        "start",
        ":start — ASG min=max=1 (Y/N confirm)",
        Category::Lifecycle,
        CommandKind::ZeroArg,
    ),
    cmd(
        "capacity",
        ":capacity — modal form: Min / Max / Instance type / Cooldown in one shot",
        Category::Lifecycle,
        CommandKind::ZeroArg,
    ),
    cmd(
        "scaling-triggers",
        ":scaling-triggers — modal form for the metric-based autoscaling trigger (metric / statistic / period / breach duration / thresholds / scale increments). Pre-fills the current trigger.",
        Category::Lifecycle,
        CommandKind::ZeroArg,
    ),
    cmd(
        "swap",
        ":swap TARGET — swap CNAMEs (Y/N confirm; same preflight as a → Swap)",
        Category::Lifecycle,
        CommandKind::Prefill("swap "),
    ),
    cmd(
        "abort",
        ":abort — cancel an in-flight env update",
        Category::Lifecycle,
        CommandKind::ZeroArg,
    ),
    // ── Env config ───────────────────────────────────────────────────────
    cmd(
        "env",
        ":env list | set KEY VAL | unset KEY — application env-var editor (triggers app-server restart)",
        Category::EnvConfig,
        CommandKind::Prefill("env "),
    ),
    cmd(
        "tag",
        ":tag KEY VALUE — env tag editor",
        Category::EnvConfig,
        CommandKind::Prefill("tag "),
    ),
    cmd(
        "untag",
        ":untag KEY — remove env tag",
        Category::EnvConfig,
        CommandKind::Prefill("untag "),
    ),
    cmd(
        "set-option",
        ":set-option NS OPT VALUE — generic option-settings escape hatch",
        Category::EnvConfig,
        CommandKind::Prefill("set-option "),
    ),
    cmd(
        "unset-option",
        ":unset-option NS OPT — clear an option setting",
        Category::EnvConfig,
        CommandKind::Prefill("unset-option "),
    ),
    cmd(
        "instance-type",
        ":instance-type TYPE — EC2 instance type (rolling launch-config replacement)",
        Category::EnvConfig,
        CommandKind::Prefill("instance-type "),
    ),
    cmd(
        "keypair",
        ":keypair NAME — set EC2 key pair NAME on the env's ASG",
        Category::EnvConfig,
        CommandKind::Prefill("keypair "),
    ),
    cmd(
        "service-role",
        ":service-role ARN — set EB service role ARN/name",
        Category::EnvConfig,
        CommandKind::Prefill("service-role "),
    ),
    cmd(
        "instance-profile",
        ":instance-profile NAME — set EC2 instance-profile NAME on the env's ASG",
        Category::EnvConfig,
        CommandKind::Prefill("instance-profile "),
    ),
    cmd(
        "public-ip",
        ":public-ip on|off — toggle EC2 public IP association",
        Category::EnvConfig,
        CommandKind::Prefill("public-ip "),
    ),
    cmd(
        "elb-scheme",
        ":elb-scheme public|internal — set ELB scheme (rolling)",
        Category::EnvConfig,
        CommandKind::Prefill("elb-scheme "),
    ),
    cmd(
        "subnets",
        ":subnets — modal MultiSelect picker for aws:ec2:vpc.Subnets",
        Category::EnvConfig,
        CommandKind::ZeroArg,
    ),
    cmd(
        "elb-subnets",
        ":elb-subnets — modal MultiSelect picker for aws:ec2:vpc.ELBSubnets (web-tier)",
        Category::EnvConfig,
        CommandKind::ZeroArg,
    ),
    cmd(
        "security-groups",
        ":security-groups — modal MultiSelect picker for instance SGs (launch-config)",
        Category::EnvConfig,
        CommandKind::ZeroArg,
    ),
    cmd(
        "deployment-policy",
        ":deployment-policy POLICY — AllAtOnce | Rolling | RollingWithAdditionalBatch | Immutable | TrafficSplitting",
        Category::EnvConfig,
        CommandKind::Prefill("deployment-policy "),
    ),
    cmd(
        "rolling-update",
        ":rolling-update on|off — ASG rolling-update policy",
        Category::EnvConfig,
        CommandKind::Prefill("rolling-update "),
    ),
    cmd(
        "health-check-url",
        ":health-check-url /path — HTTP health-check path",
        Category::EnvConfig,
        CommandKind::Prefill("health-check-url "),
    ),
    cmd(
        "logs-stream",
        ":logs-stream on|off [--retention DAYS] — toggle CW Logs streaming (default 7d)",
        Category::EnvConfig,
        CommandKind::Prefill("logs-stream "),
    ),
    cmd(
        "logs-tail",
        ":logs-tail [LOG_GROUP] — open live CW Logs tail overlay (picker if multiple groups)",
        Category::EnvConfig,
        CommandKind::Prefill("logs-tail "),
    ),
    cmd(
        "notify",
        ":notify EMAIL_OR_SNS_ARN | off — set notification endpoint",
        Category::EnvConfig,
        CommandKind::Prefill("notify "),
    ),
    cmd(
        "managed-window",
        ":managed-window DAY HOUR | off — managed-update window (Mon..Sun, 0..23)",
        Category::EnvConfig,
        CommandKind::Prefill("managed-window "),
    ),
    // ── Versions / configs / alarms / platforms ──────────────────────────
    cmd(
        "delete-version",
        ":delete-version LABEL [--force] — drop an app version (--force also nukes the S3 bundle)",
        Category::VersionsConfigsAlarms,
        CommandKind::Prefill("delete-version "),
    ),
    cmd(
        "config-save",
        ":config-save NAME — save current env as a config template",
        Category::VersionsConfigsAlarms,
        CommandKind::Prefill("config-save "),
    ),
    cmd(
        "config-apply",
        ":config-apply NAME — apply a saved template to selected env (Y/N confirm)",
        Category::VersionsConfigsAlarms,
        CommandKind::Prefill("config-apply "),
    ),
    cmd(
        "config-delete",
        ":config-delete APP NAME — delete a saved config template",
        Category::VersionsConfigsAlarms,
        CommandKind::Prefill("config-delete "),
    ),
    cmd(
        "config-inspect",
        ":config-inspect TEMPLATE — inspect a saved config template",
        Category::VersionsConfigsAlarms,
        CommandKind::Prefill("config-inspect "),
    ),
    cmd(
        "alarm-create",
        ":alarm-create NAME KIND THRESHOLD [OP] — CW alarm (KIND: health | 4xx | 5xx | latency)",
        Category::VersionsConfigsAlarms,
        CommandKind::Prefill("alarm-create "),
    ),
    cmd(
        "alarm-delete",
        ":alarm-delete NAME — remove a CW alarm",
        Category::VersionsConfigsAlarms,
        CommandKind::Prefill("alarm-delete "),
    ),
    cmd(
        "alarm-history",
        ":alarm-history NAME — recent CloudWatch alarm transition timeline (StateUpdate / ConfigurationUpdate / Action entries, newest first)",
        Category::VersionsConfigsAlarms,
        CommandKind::Prefill("alarm-history "),
    ),
    cmd(
        "custom-platform-delete",
        ":custom-platform-delete ARN — delete a custom EB platform (fails if any env uses it)",
        Category::VersionsConfigsAlarms,
        CommandKind::Prefill("custom-platform-delete "),
    ),
    cmd(
        "metric",
        ":metric add LABEL NS NAME [STAT] / :metric remove LABEL / :metric list — custom Metrics-tab charts",
        Category::VersionsConfigsAlarms,
        CommandKind::Prefill("metric "),
    ),
    // ── Bulk ops ─────────────────────────────────────────────────────────
    cmd(
        "batch-rebuild",
        ":batch-rebuild — fan rebuild across multi-selection",
        Category::BulkOps,
        CommandKind::ZeroArg,
    ),
    cmd(
        "batch-restart",
        ":batch-restart — fan restart across multi-selection",
        Category::BulkOps,
        CommandKind::ZeroArg,
    ),
    cmd(
        "batch-deploy",
        ":batch-deploy LABEL — deploy the same version to every selected env",
        Category::BulkOps,
        CommandKind::Prefill("batch-deploy "),
    ),
    cmd(
        "batch-tag",
        ":batch-tag KEY VAL — fan tag write across multi-selection",
        Category::BulkOps,
        CommandKind::Prefill("batch-tag "),
    ),
    cmd(
        "batch-untag",
        ":batch-untag KEY — fan tag remove across multi-selection",
        Category::BulkOps,
        CommandKind::Prefill("batch-untag "),
    ),
    cmd(
        "batch-set-option",
        ":batch-set-option NS OPT VAL — fan option-settings write across multi-selection",
        Category::BulkOps,
        CommandKind::Prefill("batch-set-option "),
    ),
    cmd_with_aliases(
        "deselect",
        &["select-clear"],
        ":deselect — clear multi-selection (Esc also works in Normal mode)",
        Category::BulkOps,
        CommandKind::ZeroArg,
    ),
    // ── View / filter / sort ─────────────────────────────────────────────
    cmd(
        "sort",
        ":sort KEY [desc] — set sort (name/app/status/health/version/age)",
        Category::View,
        CommandKind::Prefill("sort "),
    ),
    cmd(
        "group",
        ":group on|off — toggle group-by-application",
        Category::View,
        CommandKind::Prefill("group "),
    ),
    cmd(
        "redact",
        ":redact on|off — toggle redact mode (account id, ARN, CNAMEs)",
        Category::View,
        CommandKind::Prefill("redact "),
    ),
    cmd(
        "events",
        ":events on|off — toggle the events panel",
        Category::View,
        CommandKind::Prefill("events "),
    ),
    cmd(
        "event-time",
        ":event-time [utc|local|age] — event timestamp display; no arg cycles. Default UTC. Also bound to T.",
        Category::View,
        CommandKind::Prefill("event-time "),
    ),
    cmd(
        "cols",
        ":cols list | hide NAME | show NAME | reset — column management",
        Category::View,
        CommandKind::Prefill("cols "),
    ),
    cmd(
        "save-view",
        ":save-view NAME — snapshot filter+sort+grouping+scope under NAME",
        Category::View,
        CommandKind::Prefill("save-view "),
    ),
    cmd(
        "view",
        ":view NAME — load a previously saved view",
        Category::View,
        CommandKind::Prefill("view "),
    ),
    cmd(
        "views",
        ":views — list saved views",
        Category::View,
        CommandKind::ZeroArg,
    ),
    cmd(
        "view-drop",
        ":view-drop NAME — remove a saved view",
        Category::View,
        CommandKind::Prefill("view-drop "),
    ),
    cmd_with_aliases(
        "filter",
        &["f"],
        ":filter NAME / :f NAME — recall a saved filter",
        Category::View,
        CommandKind::Prefill("filter "),
    ),
    cmd(
        "save",
        ":save NAME — save the current filter as NAME",
        Category::View,
        CommandKind::Prefill("save "),
    ),
    cmd(
        "drop",
        ":drop NAME — remove a saved filter",
        Category::View,
        CommandKind::Prefill("drop "),
    ),
    cmd(
        "filters",
        ":filters — list saved filters",
        Category::View,
        CommandKind::ZeroArg,
    ),
    cmd(
        "readonly",
        ":readonly on|off — toggle destructive-action lockout",
        Category::View,
        CommandKind::Prefill("readonly "),
    ),
    cmd(
        "pin",
        ":pin — pin / unpin the selected env (also `*`)",
        Category::View,
        CommandKind::ZeroArg,
    ),
    cmd(
        "alias",
        ":alias NAME LABEL — set or update a local env alias",
        Category::View,
        CommandKind::Prefill("alias "),
    ),
    cmd_with_aliases(
        "alias-drop",
        &["alias-rm"],
        ":alias-drop NAME — remove an alias",
        Category::View,
        CommandKind::Prefill("alias-drop "),
    ),
    cmd(
        "export",
        ":export — yank filtered view as TSV",
        Category::View,
        CommandKind::ZeroArg,
    ),
    cmd(
        "json",
        ":json — yank filtered view as JSON",
        Category::View,
        CommandKind::ZeroArg,
    ),
    cmd_with_aliases(
        "report",
        &["markdown"],
        ":report / :markdown — yank filtered view as Markdown",
        Category::View,
        CommandKind::ZeroArg,
    ),
    cmd(
        "refresh",
        ":refresh — re-fetch the table immediately",
        Category::View,
        CommandKind::ZeroArg,
    ),
    cmd_with_aliases(
        "quit",
        &["q"],
        ":quit / :q — exit ebman",
        Category::View,
        CommandKind::ZeroArg,
    ),
    cmd_with_aliases(
        "help",
        &["?"],
        ":help / ? — toggle this help screen",
        Category::View,
        CommandKind::ZeroArg,
    ),
    // ── Setup / discovery ────────────────────────────────────────────────
    cmd(
        "settings",
        ":settings — interactive form to edit ~/.config/ebman/config.toml",
        Category::Setup,
        CommandKind::ZeroArg,
    ),
    cmd_with_aliases(
        "about",
        &["credits"],
        ":about / :credits — version, license, attributions",
        Category::Setup,
        CommandKind::ZeroArg,
    ),
    cmd(
        "update",
        ":update — show + yank the upgrade command for the detected install channel",
        Category::Setup,
        CommandKind::ZeroArg,
    ),
    cmd(
        "whatsnew",
        ":whatsnew — embedded changelog popup",
        Category::Setup,
        CommandKind::ZeroArg,
    ),
    cmd(
        "loglevel",
        ":loglevel LEVEL — live-reload tracing filter (trace/debug/info/warn/error)",
        Category::Setup,
        CommandKind::Prefill("loglevel "),
    ),
];

/// Flat name + alias list for plugin-collision detection. Generated
/// lazily on first access; the registry is a `const` slice so this
/// computation is pure + cheap (~90 entries).
pub fn all_names() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::with_capacity(COMMANDS.len() * 2);
    for c in COMMANDS {
        out.push(c.name);
        out.extend(c.aliases.iter().copied());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_name_is_unique() {
        // No duplicate command names or aliases. Each must dispatch to
        // exactly one handler.
        let mut seen: HashSet<&str> = HashSet::new();
        for c in COMMANDS {
            assert!(
                seen.insert(c.name),
                "duplicate command name in registry: {}",
                c.name
            );
            for a in c.aliases {
                assert!(
                    seen.insert(a),
                    "duplicate command alias in registry: {a} (alias of {})",
                    c.name
                );
            }
        }
    }

    #[test]
    fn every_command_has_nonempty_help() {
        for c in COMMANDS {
            assert!(
                !c.help.is_empty(),
                "command '{}' has empty help line",
                c.name
            );
        }
    }

    #[test]
    fn all_names_includes_aliases() {
        let names = all_names();
        // Every alias has to be reachable; smoke-check a known one.
        assert!(names.contains(&"r"), "region alias 'r' missing");
        assert!(names.contains(&"diagnose"), "why alias 'diagnose' missing");
        assert!(
            names.contains(&"inflight"),
            "pending alias 'inflight' missing"
        );
    }

    /// Drift detector — the whole point of the registry is that the
    /// help screen + palette + dispatch arms can't go stale. Parse
    /// `app.rs`'s `fn execute_command` body and check every
    /// `"name" =>` arm name exists in [`COMMANDS`] (as either a
    /// canonical name or an alias).
    ///
    /// Catches:
    ///   - A new arm added to the dispatcher without a registry entry
    ///     (so the help / palette would be silently missing it).
    ///   - A typo in a registry name that doesn't match a real arm.
    #[test]
    fn registry_covers_every_dispatch_arm() {
        let src = include_str!("app.rs");
        // Locate `fn execute_command`'s body. The match starts at
        // `match cmd {` and ends at the `other =>` fallthrough — we
        // bound the scan there so we don't pick up arms from any
        // other matches further down the file.
        let fn_start = src
            .find("fn execute_command")
            .expect("execute_command not found in app.rs");
        let body = &src[fn_start..];
        let match_start = body
            .find("match cmd {")
            .expect("`match cmd {` not found in execute_command body");
        // The dispatcher's fallthrough is `other => { ... }` — use
        // that as the end marker so trailing match arms in helper fns
        // can't confuse the parser.
        let body_after_match = &body[match_start..];
        let end_marker = body_after_match
            .find("other => {")
            .expect("`other => {` fallthrough not found in execute_command");
        let dispatch_body = &body_after_match[..end_marker];

        let names: std::collections::HashSet<&str> = all_names().into_iter().collect();

        let mut missing: Vec<String> = Vec::new();
        for line in dispatch_body.lines() {
            // Arm-pattern lines start with `"` and contain `=>` somewhere
            // to the right. Body lines that happen to start with a quoted
            // string don't have `=>` on the same line.
            let trimmed = line.trim_start();
            if !trimmed.starts_with('"') {
                continue;
            }
            let Some(arrow_pos) = trimmed.find("=>") else {
                continue;
            };
            // Only scan the pattern section (left of `=>`), so the arm
            // body can't fool us with string literals that happen to
            // look like names ("events panel ON", "redact off", etc.).
            let pattern = &trimmed[..arrow_pos];
            let mut rest = pattern;
            while let Some(open) = rest.find('"') {
                let after_open = &rest[open + 1..];
                let Some(close) = after_open.find('"') else {
                    break;
                };
                let name = &after_open[..close];
                if !names.contains(name) {
                    missing.push(name.to_string());
                }
                rest = &after_open[close + 1..];
            }
        }

        assert!(
            missing.is_empty(),
            "execute_command dispatches arms with no registry entry: {missing:?}.\n\
             Add them to crate::commands::COMMANDS or alias them on an existing entry."
        );
    }

    /// Reverse direction — every name in [`COMMANDS`] (and every alias)
    /// must have a real dispatch arm. Catches dead registry entries
    /// that would show up in the palette but go to the `unknown
    /// command` branch when the operator selected them.
    #[test]
    fn every_registry_name_has_a_dispatch_arm() {
        let src = include_str!("app.rs");
        let fn_start = src
            .find("fn execute_command")
            .expect("execute_command not found in app.rs");
        let body = &src[fn_start..];
        let match_start = body
            .find("match cmd {")
            .expect("`match cmd {` not found in execute_command body");
        let body_after_match = &body[match_start..];
        let end_marker = body_after_match
            .find("other => {")
            .expect("`other => {` fallthrough not found in execute_command");
        let dispatch_body = &body_after_match[..end_marker];

        // Collect every quoted name that appears in an arm pattern.
        let mut arm_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for line in dispatch_body.lines() {
            let trimmed = line.trim_start();
            if !trimmed.starts_with('"') {
                continue;
            }
            let Some(arrow_pos) = trimmed.find("=>") else {
                continue;
            };
            let pattern = &trimmed[..arrow_pos];
            let mut rest = pattern;
            while let Some(open) = rest.find('"') {
                let after_open = &rest[open + 1..];
                let Some(close) = after_open.find('"') else {
                    break;
                };
                arm_names.insert(&after_open[..close]);
                rest = &after_open[close + 1..];
            }
        }

        let mut orphans: Vec<&str> = Vec::new();
        for c in COMMANDS {
            if !arm_names.contains(c.name) {
                orphans.push(c.name);
            }
            for alias in c.aliases {
                if !arm_names.contains(alias) {
                    orphans.push(alias);
                }
            }
        }

        assert!(
            orphans.is_empty(),
            "registry entries with no dispatch arm in execute_command: {orphans:?}.\n\
             Either remove them from the registry or add the matching `\"name\" => ...` arm."
        );
    }
}
