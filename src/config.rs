//! `config.toml`: the parser, its defaults, and the safety keys.
//!
//! Parsing is a pure `parse(&str)` with a thin I/O wrapper, so the
//! whole key table is unit-testable without touching a disk.
//!
//! Unknown keys and unparseable values are COLLECTED rather than
//! ignored — see `Config::parse_errors`. A safety key that silently
//! failed to apply is worse than one that failed loudly, so the
//! safety section fails CLOSED: unreadable means writes are refused,
//! never permitted.

use std::{path::PathBuf, time::Duration};

use crate::util::{config_file, parse_bool};

#[derive(Debug, Clone)]
/// Everything `config.toml` can set, already parsed and defaulted.
///
/// Load with [`load`]. Unknown keys and unparseable values are
/// reported rather than ignored — see `Config::parse_errors` — because
/// a safety key that silently failed to apply is worse than one that
/// failed loudly.
pub struct Config {
    pub(crate) refresh_interval: Duration,
    pub(crate) extra_regions: Vec<String>,
    pub(crate) redact_default: Option<bool>,
    pub(crate) grouped_default: Option<bool>,
    pub(crate) theme: String,
    /// Glyph set: `"unicode"` (default), `"ascii"` for low-feature
    /// terminals, `"powerline"` (alias `"nerd"`) for Powerline-patched /
    /// Nerd Fonts, or `"auto"` to probe the terminal at startup and pick
    /// powerline if its support is detected, unicode otherwise.
    pub(crate) icons: String,
    pub(crate) notify_bell: bool,
    pub(crate) required_tags: Vec<String>,
    /// CloudWatch dimension names that identify an Elastic Beanstalk
    /// environment, for matching alarms to it (`alarm_dimensions`).
    ///
    /// Always contains `EnvironmentName` — that is what EB itself and
    /// `:alarm-create` write, so it can't be configured away without
    /// hiding ebman's own alarms. The config key *adds* spellings for
    /// operators whose alarms use a different dimension name.
    ///
    /// It exists because the match used to be on the dimension *value*
    /// alone, which wrongly claimed an RDS alarm named after the env;
    /// tightening it to the canonical name silently dropped
    /// operator-authored alarms spelled differently. This is the way
    /// back without reinstating the false positive.
    pub(crate) alarm_dimensions: Vec<String>,
    /// Lines `parse` didn't recognise, kept verbatim.
    ///
    /// `:settings` save rewrites the whole file from `serialize`, so
    /// anything the model doesn't carry is destroyed. That included a
    /// mistyped key — the very line an operator would otherwise spot
    /// and fix — and any key a newer release adds, which is the
    /// opposite of the graceful degradation the parser aims for.
    pub(crate) passthrough: Vec<String>,
    /// Per-profile theme override. Key = AWS profile name, value = theme
    /// name (matches the same names `theme = …` accepts). Lets the
    /// operator pin a high-contrast / dark / light theme to a specific
    /// profile so the visual cue says "you're in prod" without reading
    /// the breadcrumb. Most prod incidents start with "I thought I was
    /// in staging."
    pub(crate) profile_themes: std::collections::HashMap<String, String>,
    /// Named accounts reachable via `sts:AssumeRole`. Key = the friendly
    /// name the operator uses with `:account NAME`; value is the full
    /// AssumeRole spec — `role_arn`, `source_profile`, optional
    /// `external_id`, optional `region` override. Lines in `config.toml`
    /// use the form `accounts.NAME.field = "value"`, mirroring the
    /// `metric.LABEL.field` shape that the rest of the config uses.
    pub(crate) accounts: std::collections::HashMap<String, AccountSpec>,
    /// Per-environment runbook URLs. Key = env name; value = a URL the
    /// operator wants surfaced during triage. Lines in `config.toml` use
    /// `runbooks.ENV = "https://…"`. Shown in the `:why` overlay.
    pub(crate) runbooks: std::collections::HashMap<String, String>,
    /// The App tab, per env (`app.ENV.field`). See `app_tab`.
    pub(crate) app_tabs: crate::app_tab::AppTabs,
    /// Per-env read-only locks. When `safety_envs.get(env_name) ==
    /// Some(true)`, destructive actions against that env are refused
    /// even when the global `--read-only` toggle is off. Borrowed from
    /// pgman's `safety.databases.NAME.read_only` pattern. Lines in
    /// `config.toml` use `safety.envs.NAME.read_only = true`.
    pub(crate) safety_envs: std::collections::HashMap<String, bool>,
    /// Per-account read-only locks. Same shape as `safety_envs` but
    /// matched against the *active account name* (the `:account NAME`
    /// key for AssumeRole'd accounts, or the AWS profile name
    /// otherwise). Lines use `safety.accounts.NAME.read_only = true`.
    pub(crate) safety_accounts: std::collections::HashMap<String, bool>,
    /// Lines under `safety.` that could not be fully understood.
    ///
    /// Non-empty means the safety policy is only PARTIALLY readable,
    /// and every write is refused until it is fixed — see
    /// `write_gate::Refusal::SafetyConfigUnreadable`. Previously each of
    /// these was skipped in silence, so `safety.envs.prod = true`
    /// (missing `.read_only`), `safety.envs.prod.readonly = true` (typo)
    /// and `safety.envs.prod.read_only = ture` (bad value) all left the
    /// environment WRITEABLE while the operator believed it pinned.
    ///
    /// Deliberately not "guess which env they meant and pin that": the
    /// guess can be wrong, and `:settings` writes the config back, so a
    /// guessed pin would be silently promoted to a real one — turning a
    /// typo'd `read_only = false` into a durable `true`.
    ///
    /// Not serialised by `save`. These are diagnostics about the
    /// operator's file, not settings.
    pub(crate) safety_parse_errors: Vec<String>,
    /// `terraform.state_path` — where to read tfstate when no
    /// `terraform.tfstate` is discoverable by walking up from cwd.
    ///
    /// This is what makes `drift` usable on a fleet whose state lives in
    /// a remote backend (HCP, S3, Consul). ebman does not talk to those
    /// backends and holds no token for them: the operator runs
    /// `terraform state pull > somewhere.json` and points this at it,
    /// which works for every backend rather than the one we implemented.
    /// A standing refusal of every write, everywhere.
    ///
    /// `safety.read_only = true`. The highest-precedence rung in
    /// `write_gate`, which already existed and had no way to be set
    /// from config — only the TUI's session toggle reached it.
    ///
    /// This is the shape the whole permission model is moving toward:
    /// **config may only ever say no.** A standing restriction is
    /// decided calmly, once, and only narrows; a standing permission
    /// has to predict what you will need and is paid for at the worst
    /// moment. So there is a config key to forbid writes and there is
    /// deliberately none to allow them.
    pub(crate) safety_read_only: bool,

    /// Return dead-lettered message BODIES on a peek.
    ///
    /// Defaults to `true` — current behaviour, so enabling the switch
    /// is a choice and leaving it alone changes nothing. The default
    /// is not a judgement that bodies are harmless; it is that turning
    /// them off silently can cost the operator the answer.
    ///
    /// `beanstalk.sqsd.*` (task name / path / scheduled time) is a
    /// separate field from `body` in the same message, so suppressing
    /// one leaves the other intact — that is structural. But those
    /// attributes are set by EB's worker daemon for its own scheduled
    /// tasks; a message an APPLICATION posted to the worker queue
    /// carries its identity in the body or nowhere, which is why
    /// `peek_messages` asks for `All` attributes in the first place.
    /// So for that shape, `false` removes the only diagnostic content
    /// there is, and an operator who cannot see it will not know what
    /// they are missing.
    ///
    /// Operator-set only, never agent-set: an agent must not be able
    /// to widen its own access. The tool description states which mode
    /// is active, so a peek that returns no bodies reads as a policy
    /// choice rather than an empty queue.
    pub(crate) mcp_peek_bodies: bool,
    /// `mcp.role.NAME.*`: the roles an MCP write assumes, by verb. See
    /// `docs/design/assume-role-elevation.md`. Sorted, so the file is
    /// written back the same way each time.
    pub(crate) mcp_roles: std::collections::BTreeMap<String, McpRole>,
    /// `mcp.require_role = true`: an MCP write whose verb maps to no
    /// role is refused rather than going out under the base credentials.
    pub(crate) mcp_require_role: bool,
    pub(crate) terraform_state_path: Option<String>,
    /// Optional outbound webhook for audit-line fan-out. Each audit
    /// line written to `~/.cache/ebman/audit.log` is also POSTed to
    /// this URL as JSON (fire-and-forget; failures don't block or
    /// alarm). Body shape: `{"text": "<audit line>", "action": "...",
    /// "target": "...", "account": "...", "profile": "...", "region":
    /// "...", "at": "<rfc3339>"}`. The top-level `text` makes the
    /// body Slack-incoming-webhook-compatible out of the box. Lines
    /// in `config.toml` use `notify_webhook = "https://..."`.
    pub(crate) notify_webhook: Option<String>,
    /// User-defined command aliases. Key = the bare name typed
    /// after `:` (no leading colon); value = the full command line
    /// the alias expands to. Lines in `config.toml` use
    /// `alias.NAME = "command line"`. Expansion happens in
    /// `execute_command` before the dispatch match — args typed
    /// after the alias name are appended to the expansion, so
    /// `alias.dp = "deploy --auto-rollback 5m"` + `:dp build-900`
    /// becomes `:deploy --auto-rollback 5m build-900`.
    ///
    /// Named `command_aliases` to distinguish from the existing
    /// `:alias <env> <label>` env-rename feature (state.toml-
    /// persisted, lives on `App.aliases`). Command aliases are
    /// config.toml-only.
    pub(crate) command_aliases: std::collections::HashMap<String, String>,
    /// Lint rule IDs to skip globally. CSV-in-string form in
    /// `config.toml`: `lint.disable = "EBL001,EBL006"`. Mirrors
    /// the existing `extra_regions` / `required_tags` conventions.
    /// Project-local `.ebman/ebman.toml` can extend (never
    /// override) this set via `[lint]\ndisable = ["EBL001"]`.
    pub(crate) lint_disable: Vec<String>,
    /// Lint rule IDs whose auto-fix is suppressed even when
    /// `--fix` is passed. Same CSV-in-string form:
    /// `lint.fix_disable = "EBL004"`. Operators who want lint
    /// reports but don't want a specific rule's fix dispatched
    /// (e.g. they have a non-standard BatchSize for a reason)
    /// list it here.
    pub(crate) lint_fix_disable: Vec<String>,
    /// Master switch for the LLM-backed `ebman explain`
    /// subcommand. Off by default — operators must explicitly
    /// opt in via `explain.enabled = true`. Presence of an API
    /// key in the env is not implicit consent.
    pub(crate) explain_enabled: bool,
    /// Provider key — `"anthropic"` (default) or `"ollama"`.
    pub(crate) explain_provider: String,
    /// Model identifier. Anthropic default: `claude-haiku-4-5`.
    pub(crate) explain_model: String,
    /// Env-var name to read the Anthropic API key from.
    /// Default `ANTHROPIC_API_KEY`.
    pub(crate) explain_api_key_env: String,
    /// HTTP base URL for the Ollama provider. Default
    /// `http://localhost:11434`.
    pub(crate) explain_ollama_url: String,
    /// Soft cap on response tokens. 0 = use default (1024).
    pub(crate) explain_max_tokens: u32,
}

/// A named `sts:AssumeRole` target. The operator typically pins one of
/// these per child account and switches between them via `:account
/// NAME`. `source_profile` carries the base creds (so chained role
/// hops still resolve), `external_id` is optional but required by some
/// trust policies, `region` is optional (falls back to the source
/// profile's / env default).
/// A role MCP writes assume for the verbs it lists (`mcp.role.NAME.*`).
/// `role_arn`, `source_profile` and `external_id` mean what they do for
/// `accounts.NAME.*`; `verbs` names MCP write tools.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct McpRole {
    pub role_arn: String,
    pub source_profile: Option<String>,
    pub external_id: Option<String>,
    pub verbs: Vec<String>,
}

/// What the role config says about one MCP write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RoleCheck {
    /// Not an MCP write, or one with no verb of its own (`dlq_undo`
    /// goes out under the session of the delete it reverses).
    NotAsked,
    /// The role config cannot be acted on: every MCP write refuses
    /// until it is fixed, as an unreadable `safety.` line does.
    Unreadable { problem: String },
    /// The verb maps to the role `mcp.role.ROLE`, whose ARN says this.
    Mapped { role: String, arn: RoleArn },
    /// No role lists the verb; `required` is `mcp.require_role`.
    Unmapped { required: bool },
}

/// What the role config says about an MCP write of `verb`, given the
/// MCP write verbs that exist (`known`).
///
/// Fails closed, never by guessing: a verb two roles list, a verb no
/// tool has (a typo, or a TOML array where a comma list belongs) and a
/// role with no `role_arn` each make the whole config unreadable, since
/// choosing would be the fail-open the safety parser was rewritten to
/// stop.
pub(crate) fn mcp_role_check(cfg: &Config, verb: &str, known: &[&str]) -> RoleCheck {
    let mut owner: Option<&str> = None;
    for (name, role) in &cfg.mcp_roles {
        if role.role_arn.is_empty() {
            return RoleCheck::Unreadable {
                problem: format!("mcp.role.{name} has no role_arn"),
            };
        }
        // The plan reads the role's account from its ARN to check it
        // against the env's: one it cannot read is a role it cannot
        // check, and AssumeRole would refuse it anyway.
        // Debug-quoted: the problem reaches an agent and a status bar,
        // and what failed to parse is by definition not a clean ARN.
        if parse_role_arn(&role.role_arn).is_none() {
            return RoleCheck::Unreadable {
                problem: format!(
                    "mcp.role.{name}.role_arn {:?} is not an IAM role ARN \
                     (arn:aws:iam::ACCOUNT:role/NAME)",
                    role.role_arn
                ),
            };
        }
        for v in &role.verbs {
            if !known.contains(&v.as_str()) {
                return RoleCheck::Unreadable {
                    problem: format!(
                        "mcp.role.{name}.verbs names `{v}`, which is not an MCP write \
                         verb (give a comma list, e.g. \"deploy,restart\"; the verbs are {})",
                        known.join(", ")
                    ),
                };
            }
        }
    }
    for name_v in known {
        let owners: Vec<&str> = cfg
            .mcp_roles
            .iter()
            .filter(|(_, r)| r.verbs.iter().any(|v| v == name_v))
            .map(|(n, _)| n.as_str())
            .collect();
        if owners.len() > 1 {
            return RoleCheck::Unreadable {
                problem: format!(
                    "`{name_v}` is listed by more than one role ({}); a verb has one role",
                    owners.join(", ")
                ),
            };
        }
        if *name_v == verb {
            owner = owners.first().copied();
        }
    }
    // Parsed above for every role, so this cannot be `None`; were the
    // two ever to drift, unreadable is the closed answer, never
    // "unmapped".
    match owner {
        Some(role) => match cfg
            .mcp_roles
            .get(role)
            .and_then(|r| parse_role_arn(&r.role_arn))
        {
            Some(arn) => RoleCheck::Mapped {
                role: role.to_string(),
                arn,
            },
            None => RoleCheck::Unreadable {
                problem: format!("mcp.role.{role} could not be read"),
            },
        },
        None => RoleCheck::Unmapped {
            required: cfg.mcp_require_role,
        },
    }
}

/// What an IAM role ARN says: the account it is in and the role's name
/// (the last path segment, as IAM and the console name it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RoleArn {
    /// The whole ARN, trimmed.
    pub arn: String,
    pub account: String,
    pub name: String,
}

/// `arn:PARTITION:iam::ACCOUNT:role/[PATH/]NAME`, or `None`, by IAM's
/// own rules: an AWS partition (`aws`, `aws-cn`, `aws-us-gov`,
/// `aws-iso…`), a twelve-digit account, a path of printable ASCII up to
/// 512 characters, and a name of `[A-Za-z0-9+=,.@_-]` up to 64. So what
/// parses is bounded and plain, and shows whole in a dialog.
pub(crate) fn parse_role_arn(arn: &str) -> Option<RoleArn> {
    let mut parts = arn.trim().splitn(6, ':');
    let (arn_, partition, service, region, account, resource) = (
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
    );
    let path_and_name = resource.strip_prefix("role/")?;
    let (path, name) = path_and_name
        .rsplit_once('/')
        .unwrap_or(("", path_and_name));
    let name_char = |b: u8| b.is_ascii_alphanumeric() || b"+=,.@_-".contains(&b);
    let ok = arn_ == "arn"
        && (matches!(partition, "aws" | "aws-cn" | "aws-us-gov")
            || partition.starts_with("aws-iso"))
        && service == "iam"
        && region.is_empty()
        && account.len() == 12
        && account.bytes().all(|b| b.is_ascii_digit())
        // IAM counts the path's leading and trailing slash.
        && (path.is_empty() || path.len() + 2 <= 512)
        // `role//D` and `role/a//D`: IAM's path has no empty segment.
        && !(path_and_name.contains('/') && path.split('/').any(str::is_empty))
        && path.bytes().all(|b| (0x21..=0x7e).contains(&b))
        && (1..=64).contains(&name.len())
        && name.bytes().all(name_char);
    ok.then(|| RoleArn {
        arn: arn.trim().to_string(),
        account: account.to_string(),
        name: name.to_string(),
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AccountSpec {
    pub role_arn: String,
    pub source_profile: Option<String>,
    pub external_id: Option<String>,
    pub region: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            refresh_interval: Duration::from_secs(15),
            extra_regions: Vec::new(),
            redact_default: None,
            grouped_default: None,
            theme: "dark".into(),
            icons: "unicode".into(),
            notify_bell: false,
            required_tags: Vec::new(),
            alarm_dimensions: vec![crate::aws::ENV_DIMENSION.to_string()],
            passthrough: Vec::new(),
            profile_themes: std::collections::HashMap::new(),
            accounts: std::collections::HashMap::new(),
            runbooks: std::collections::HashMap::new(),
            app_tabs: crate::app_tab::AppTabs::default(),
            safety_envs: std::collections::HashMap::new(),
            safety_accounts: std::collections::HashMap::new(),
            safety_parse_errors: Vec::new(),
            safety_read_only: false,
            mcp_peek_bodies: true,
            mcp_roles: std::collections::BTreeMap::new(),
            mcp_require_role: false,
            terraform_state_path: None,
            notify_webhook: None,
            command_aliases: std::collections::HashMap::new(),
            lint_disable: Vec::new(),
            lint_fix_disable: Vec::new(),
            explain_enabled: false,
            explain_provider: String::new(),
            explain_model: String::new(),
            explain_api_key_env: String::new(),
            explain_ollama_url: String::new(),
            explain_max_tokens: 0,
        }
    }
}

impl Config {
    /// How many targets this config pins read-only.
    ///
    /// A property of the config, summarised by the config — which is the
    /// shape two guards forced this into, and they were right both times.
    /// Reading `safety_envs` / `safety_accounts` from a `src/cli` path
    /// trips `cli_write_paths_do_not_reach_past_the_shared_gate`, because
    /// a path that reads the pins is one step from deciding with them and
    /// skipping the freeze. Routing through `write_gate` instead trips
    /// `only_the_gates_consult_the_shared_decision`, because only the two
    /// gates may touch the shared decision.
    ///
    /// `doctor` needs this to report the restrictions in force, which is a
    /// reporting need and not a deciding one — a distinction neither guard
    /// can make syntactically, and neither should have to. Here it belongs
    /// to neither the gate nor the reporter.
    pub(crate) fn pinned_target_count(&self) -> usize {
        self.safety_envs.values().filter(|ro| **ro).count()
            + self.safety_accounts.values().filter(|ro| **ro).count()
    }

    /// The resolved icon style. `main` reads this to run the Powerline
    /// glyph probe and writes the answer back before the `App` is built,
    /// which is the only reason `Config` has any public surface beyond
    /// `load`. An accessor rather than a `pub` field so the other ~30
    /// fields stay crate-private.
    pub fn icons(&self) -> &str {
        &self.icons
    }

    /// See [`Config::icons`]. Called once, with the probe's verdict.
    pub fn set_icons(&mut self, icons: String) {
        self.icons = icons;
    }
}

/// Read and parse `config.toml`, falling back to defaults.
///
/// Never fails: a missing file is the ordinary case, and a malformed
/// one yields defaults plus the errors in `Config::parse_errors`, so
/// the caller can surface them. Safety keys fail CLOSED — an
/// unreadable safety section refuses writes rather than permitting
/// them.
pub fn load() -> Config {
    let path = config_path();
    config_from_read(std::fs::read_to_string(&path), &path)
}

/// Turn the result of reading `config.toml` into a `Config`.
///
/// Split from `load` so the failure arms are testable without writing
/// to the shared test config path — the house rule keeps the I/O
/// wrapper thin and the decision pure.
fn config_from_read(read: std::io::Result<String>, path: &std::path::Path) -> Config {
    match read {
        Ok(text) => parse(&text),
        // No config file at all is the ordinary case — no pins exist,
        // so there is nothing to fail closed about.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
        // Anything else — a permission change, one non-UTF-8 byte from
        // an editor saving latin-1, a failing disk — means pins MAY be
        // defined and we cannot see them. Returning a default here was
        // the file-level version of the hole the line-level parser
        // closes: `safety.envs.prod.read_only = true` sitting in a file
        // we failed to read left prod writeable, silently.
        Err(e) => Config {
            safety_parse_errors: vec![format!(
                "{} could not be read ({e}) — any safety pins in it \
                 cannot be applied",
                path.display()
            )],
            ..Config::default()
        },
    }
}

/// Sugar for the `ebman lint` CLI: just the global
/// `lint.disable` list, no other fields. Composed with the
/// project-level disables in `project::load_lint_disables_from_cwd`.
pub(crate) fn load_lint_disables() -> Vec<String> {
    load().lint_disable
}

/// Same as [`load_lint_disables`] but for the auto-fix opt-out list.
/// Rules in this list won't have their `fix()` dispatched by `ebman
/// lint --fix` even when they're enabled for reporting.
pub(crate) fn load_lint_fix_disables() -> Vec<String> {
    load().lint_fix_disable
}

/// What one `safety.envs.NAME.FIELD` / `safety.accounts.NAME.FIELD`
/// line means.
///
/// Every arm that is not `Valid` used to be a silent `continue`, which
/// made a safety control fail OPEN: the operator wrote a pin, the
/// parser dropped it, and the environment stayed writeable with nothing
/// anywhere saying so.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SafetyPin {
    Valid {
        name: String,
        read_only: bool,
    },
    /// Understood the shape, could not act on it. The caller records it
    /// and every write is refused until the line is fixed.
    Unreadable {
        problem: String,
    },
}

/// Parse the part of a safety key after `safety.envs.` /
/// `safety.accounts.`, plus its value.
///
/// `scope` names the family for the error message ("safety.envs"), so
/// the operator is told which line to look at rather than that
/// "something under safety" is wrong.
pub(crate) fn parse_safety_pin(scope: &str, rest: &str, value: &str) -> SafetyPin {
    let unreadable = |problem: String| SafetyPin::Unreadable { problem };

    // `rsplit_once`, so the FIELD is the last segment and everything
    // before it is the name. An AWS profile name may contain dots
    // (`company.prod` is ordinary) and `safety.accounts.NAME` matches
    // the profile name — splitting from the left made every dotted
    // account unreadable, which under fail-closed means refusing every
    // write for a config that is perfectly reasonable.
    let Some((name, field)) = rest.rsplit_once('.') else {
        // `safety.envs.prod = true` — the field was omitted entirely.
        return unreadable(format!(
            "{scope}.{rest} is missing a field — did you mean {scope}.{rest}.read_only?"
        ));
    };
    let (name, field) = (name.trim(), field.trim());
    if name.is_empty() {
        return unreadable(format!("{scope}..{field} has an empty name"));
    }
    if field != "read_only" {
        // Includes a plain typo (`readonly`) and a field a NEWER ebman
        // understands but this one does not. Both are refused for the
        // same reason: an unenforceable safety control is not a safety
        // control, and silently ignoring one a newer binary would apply
        // is the worst of the three outcomes.
        return unreadable(format!(
            "{scope}.{name}.{field} is not a setting this version understands \
             (expected read_only)"
        ));
    }
    match parse_bool(value) {
        Some(read_only) => SafetyPin::Valid {
            name: name.to_string(),
            read_only,
        },
        None => unreadable(format!(
            "{scope}.{name}.read_only = {value:?} is not a boolean \
             (expected true/false, yes/no, on/off, 1/0)"
        )),
    }
}

/// Resolve `alarm_dimensions = "A,B,-C"` into the dimension names that
/// identify an environment.
///
/// Extracted from `parse`'s match so that stays a router — the arm was
/// ~50 lines of rules, which is a function, not a case. The rules, each
/// of which cost something:
///
/// - ADDITIONAL names on top of the canonical one, not a replacement:
///   `:alarm-create` always writes `EnvironmentName`
///   (`aws/cloudwatch.rs`), so dropping it would hide the alarms ebman
///   itself creates along with every EB-native one — which during
///   triage reads as "no alarms configured".
/// - Rebuilt from the canonical name each time rather than appended to,
///   so a second `alarm_dimensions =` line REPLACES the first as every
///   other key does. Appending meant an operator editing by adding a
///   line silently got the union of both, with no way to narrow it back.
/// - A leading `-` removes a name — the escape hatch for the false
///   positive this key exists to avoid. An operator whose RDS alarms
///   carry an `EnvironmentName` dimension needs `"MyDim,-EnvironmentName"`
///   to stop ebman claiming them; add-only left them no way back. `-`
///   cannot collide with a real CloudWatch dimension name, so the opt-out
///   cannot happen by accident.
/// - An empty result falls back to the canonical name: an empty list
///   matches nothing ever, and silently disabling the alarm panel is
///   exactly the "no alarms configured" misread the default prevents.
pub(crate) fn parse_alarm_dimensions(value: &str) -> Vec<String> {
    let mut names = vec![crate::aws::ENV_DIMENSION.to_string()];
    for name in crate::util::split_csv(value) {
        match name.strip_prefix('-') {
            Some(drop) => names.retain(|n| n != drop),
            None => {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    if names.is_empty() {
        names.push(crate::aws::ENV_DIMENSION.to_string());
    }
    names
}

/// A value with ONE surrounding pair of quotes removed, if it has one.
///
/// `trim_matches('"')` removed every leading and trailing quote, so a
/// value ending in a quote (a regex matching `msg="…"`, an ARN naming
/// `"hi"`) lost it, and could not survive a `:settings` round trip.
///
/// Each end is stripped on its own, so an unclosed `"dark` still reads
/// as `dark`, as it always has.
/// The value a config line holds, as the parser reads it: what follows
/// the first `=`, trimmed, then unquoted. The App tab's editor checks a
/// line it writes reads back through this, so the two cannot disagree.
pub(crate) fn line_value(line: &str) -> Option<String> {
    line_parts(line).map(|(_, _, value)| value)
}

/// A config line's key, raw value and value, as `parse` reads every
/// line: split at the first `=`, both sides trimmed, the value unquoted
/// (the raw one kept for the inline-comment check). One function so the
/// editor's read-back (`line_value`) is the parser's reading, not a copy
/// of it.
fn line_parts(line: &str) -> Option<(&str, &str, String)> {
    let (key, raw) = line.trim().split_once('=')?;
    let raw = raw.trim();
    Some((key.trim(), raw, unquote(raw).to_string()))
}

fn unquote(raw: &str) -> &str {
    let s = raw.strip_prefix('"').unwrap_or(raw);
    s.strip_suffix('"').unwrap_or(s)
}

/// What is wrong with an `app.` line's raw value, if anything this
/// parser would otherwise read silently and wrongly. It has no inline
/// comments, so a `#` after a quoted value would become part of the
/// value and match no data.
fn app_value_problem(raw: &str) -> Option<&'static str> {
    const COMMENT: &str = "looks like it carries an inline # comment, which is not \
                           supported: put the comment on its own line";
    let Some(inner) = raw.strip_prefix('"') else {
        // Unquoted: `Shop # prod` (or `Shop\t# prod`) would apply as
        // written, comment and all. A `#` that is part of the value is
        // kept by quoting it, so the message says so.
        let chars: Vec<char> = raw.chars().collect();
        let commented = chars
            .windows(2)
            .any(|w| w[0].is_whitespace() && w[1] == '#');
        return commented.then_some(
            "looks like it carries an inline # comment, which is not supported: \
             put the comment on its own line, or quote the value if the # is part of it",
        );
    };
    if !inner.ends_with('"') {
        return Some(if inner.contains('"') {
            "has text after its closing quote (inline # comments are not \
             supported: put the comment on its own line)"
        } else {
            "has an opening quote and no closing one"
        });
    }
    // `"Shop" # the "prod"` ends in a quote too, so look for a quote
    // that closes a value and is followed by `#`. Not one written `\"`:
    // that is part of the value (`href=\"#…\"`). A quote after an even
    // run of backslashes (`\\"`) is not escaped: the backslashes are.
    let chars: Vec<char> = inner.chars().collect();
    let closes_then_comment = (0..chars.len()).any(|i| {
        let backslashes = chars[..i].iter().rev().take_while(|c| **c == '\\').count();
        chars[i] == '"'
            && backslashes % 2 == 0
            && chars[i + 1..].iter().find(|c| !c.is_whitespace()) == Some(&'#')
    });
    closes_then_comment.then_some(COMMENT)
}

pub(crate) fn parse(text: &str) -> Config {
    let mut cfg = Config::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, raw, value)) = line_parts(line) else {
            continue;
        };
        match key {
            "refresh_interval_secs" => {
                if let Ok(n) = value.parse::<u64>() {
                    if n > 0 {
                        cfg.refresh_interval = Duration::from_secs(n);
                    }
                }
            }
            "extra_regions" => {
                cfg.extra_regions = crate::util::split_csv(&value);
            }
            "mcp.peek_bodies" => {
                if let Some(b) = parse_bool(&value) {
                    cfg.mcp_peek_bodies = b;
                }
            }
            "mcp.require_role" => match parse_bool(&value) {
                Some(b) => cfg.mcp_require_role = b,
                // Unreadable is not "off": an operator who wrote the key
                // meant to restrict.
                None => {
                    cfg.mcp_require_role = true;
                    cfg.passthrough.push(line.to_string());
                }
            },
            other if other.starts_with("mcp.role.") => {
                // `mcp.role.NAME.field`. Split on the last dot, as
                // `accounts.` does, so a dotted name works.
                let rest = other.trim_start_matches("mcp.role.");
                let Some((name, field)) =
                    rest.rsplit_once('.').filter(|(n, _)| !n.trim().is_empty())
                else {
                    cfg.passthrough.push(line.to_string());
                    continue;
                };
                let role = || name.trim().to_string();
                match field.trim() {
                    "role_arn" => cfg.mcp_roles.entry(role()).or_default().role_arn = value,
                    "verbs" => {
                        cfg.mcp_roles.entry(role()).or_default().verbs =
                            crate::util::split_csv(&value)
                    }
                    "source_profile" => {
                        cfg.mcp_roles.entry(role()).or_default().source_profile = Some(value)
                    }
                    "external_id" => {
                        cfg.mcp_roles.entry(role()).or_default().external_id = Some(value)
                    }
                    _ => cfg.passthrough.push(line.to_string()),
                }
            }
            "terraform.state_path" => {
                cfg.terraform_state_path = Some(value).filter(|v| !v.is_empty())
            }
            "redact_default" => cfg.redact_default = parse_bool(&value),
            "grouped_default" => cfg.grouped_default = parse_bool(&value),
            "theme" => cfg.theme = value,
            "icons" => cfg.icons = value,
            "notify_bell" => {
                if let Some(b) = parse_bool(&value) {
                    cfg.notify_bell = b;
                }
            }
            "lint.disable" => {
                cfg.lint_disable = crate::util::split_csv(&value);
            }
            "lint.fix_disable" => {
                cfg.lint_fix_disable = crate::util::split_csv(&value);
            }
            "explain.enabled" => {
                if let Some(b) = parse_bool(&value) {
                    cfg.explain_enabled = b;
                }
            }
            "explain.provider" => cfg.explain_provider = value,
            "explain.model" => cfg.explain_model = value,
            "explain.api_key_env" => cfg.explain_api_key_env = value,
            "explain.ollama_url" => cfg.explain_ollama_url = value,
            "explain.max_tokens" => {
                if let Ok(n) = value.parse::<u32>() {
                    cfg.explain_max_tokens = n;
                }
            }
            "notify_webhook" => {
                // Empty string disables; treat the same as the key
                // being absent so a `notify_webhook = ""` line acts as
                // an explicit off-switch without removing the key.
                cfg.notify_webhook = if value.is_empty() { None } else { Some(value) };
            }
            "required_tags" => {
                cfg.required_tags = crate::util::split_csv(&value);
            }
            "alarm_dimensions" => cfg.alarm_dimensions = parse_alarm_dimensions(&value),
            "profile_themes" => {
                // Format: `prod:high-contrast,staging:dark,default:light`.
                // Whitespace around tokens is tolerated; entries without a
                // `:` separator are skipped. Empty profile / empty theme
                // are skipped so a stray trailing comma can't smuggle in
                // a `"" → ""` mapping.
                cfg.profile_themes = parse_profile_themes(&value);
            }
            other if other.starts_with("runbooks.") => {
                // `runbooks.ENV = "url"`. The key after the dot is the
                // whole env name (EB env names don't contain dots).
                let name = other.trim_start_matches("runbooks.").trim();
                if !name.is_empty() && !value.is_empty() {
                    cfg.runbooks.insert(name.to_string(), value);
                }
            }
            other if other.starts_with("app.") => {
                // Once, as `app_tab::key_of` strips it: `app.app.e.x` is
                // env `app`'s line, to the parser and to the editor's save.
                let rest = &other["app.".len()..];
                if let Some(problem) = app_value_problem(raw) {
                    cfg.app_tabs
                        .reject(rest, line, format!("app.{rest} {problem}"));
                } else {
                    cfg.app_tabs.apply(rest, &value, line);
                }
            }
            other if other.starts_with("accounts.") => {
                // `accounts.NAME.field = "value"`. Split on the LAST
                // dot so multi-line specs accumulate into one HashMap
                // entry per NAME. Unknown fields are ignored so a future
                // field addition can degrade gracefully on older
                // binaries.
                //
                // `rsplit_once`: the field is the last segment, so a
                // dotted account name works. Splitting from the left
                // read `accounts.company.prod.role_arn` as name
                // `company`, field `prod.role_arn` — an unrecognised
                // field, which this arm ignores by design, so the spec
                // was silently never created and `:account company.prod`
                // reported no such account. Profile names routinely
                // contain dots.
                let rest = other.trim_start_matches("accounts.");
                let Some((name, field)) = rest.rsplit_once('.') else {
                    // Preserve it: `:settings` rewrites this file from
                    // the parsed config, so a `continue` here deletes
                    // the operator's line on the next save.
                    cfg.passthrough.push(line.to_string());
                    continue;
                };
                let name = name.trim();
                if name.is_empty() {
                    // `accounts..role_arn` would otherwise create a spec
                    // under the empty name, which `contains_key` then
                    // reports as a real account.
                    cfg.passthrough.push(line.to_string());
                    continue;
                }
                // The entry is created only once a field we recognise
                // is seen. Creating it first meant a typo — or a key a
                // newer release adds — left a spec with an empty ARN
                // that `contains_key` reports as a real account, so
                // `:account NAME` took the AssumeRole path and failed
                // with an opaque STS error instead of "no such
                // account".
                match field.trim() {
                    "role_arn" => {
                        cfg.accounts.entry(name.to_string()).or_default().role_arn = value;
                    }
                    "source_profile" => {
                        cfg.accounts
                            .entry(name.to_string())
                            .or_default()
                            .source_profile = Some(value);
                    }
                    "external_id" => {
                        cfg.accounts
                            .entry(name.to_string())
                            .or_default()
                            .external_id = Some(value);
                    }
                    "region" => {
                        cfg.accounts.entry(name.to_string()).or_default().region = Some(value);
                    }
                    // Unrecognised: preserved verbatim by `passthrough`
                    // below rather than silently creating a phantom.
                    _ => cfg.passthrough.push(line.to_string()),
                }
            }
            // `safety.envs.NAME.read_only = true` and
            // `safety.accounts.NAME.read_only = true`. Only one field
            // today (read_only); the dotted shape leaves room for
            // future fields (statement_timeout-equivalent, etc.).
            other if other.starts_with("safety.envs.") => {
                let rest = other.trim_start_matches("safety.envs.");
                match parse_safety_pin("safety.envs", rest, &value) {
                    SafetyPin::Valid { name, read_only } => {
                        cfg.safety_envs.insert(name, read_only);
                    }
                    SafetyPin::Unreadable { problem } => {
                        cfg.safety_parse_errors.push(problem);
                        // Keep the line. `:settings` rewrites the whole
                        // file from the parsed config, so dropping it
                        // would DELETE the operator's pin on the next
                        // save — and the refusal is exactly what sends
                        // them to `:settings` to look. That path ends
                        // with the refusal lifted, the pin gone and the
                        // env writeable: the original fail-open,
                        // reached by a new route.
                        cfg.passthrough.push(line.to_string());
                    }
                }
            }
            other if other.starts_with("alias.") => {
                // `alias.NAME = "command line"`. The NAME comes
                // after the dot; nested dots in NAME are rejected
                // (would conflict with future hierarchical
                // config-key plans). Empty NAME or empty value:
                // skip silently — operators sometimes leave
                // `alias. = ""` while editing.
                let name = other.trim_start_matches("alias.").trim();
                if name.is_empty() || name.contains('.') || value.is_empty() {
                    continue;
                }
                cfg.command_aliases.insert(name.to_string(), value);
            }
            other if other.starts_with("safety.accounts.") => {
                let rest = other.trim_start_matches("safety.accounts.");
                match parse_safety_pin("safety.accounts", rest, &value) {
                    SafetyPin::Valid { name, read_only } => {
                        cfg.safety_accounts.insert(name, read_only);
                    }
                    SafetyPin::Unreadable { problem } => {
                        cfg.safety_parse_errors.push(problem);
                        // Preserved for the same reason as `safety.envs`
                        // above: a save must not delete the line.
                        cfg.passthrough.push(line.to_string());
                    }
                }
            }
            // Any OTHER key under `safety.` — refused, not ignored.
            //
            // The two arms above police typos INSIDE a known family
            // (`safety.envs.prod.readonly`). This one catches a typo OF
            // the family (`safety.env.prod.read_only`, missing the `s`)
            // — which is the same operator mistake and was previously
            // the one shape that still vanished silently, leaving the
            // env writeable while the fleet-wide refusal fired for the
            // lesser typo one character away.
            //
            // It also covers a family a NEWER ebman writes
            // (`safety.regions.*`). Same reasoning the field check
            // already applies: a safety control this binary cannot
            // enforce must not read as absent.
            // Fail-closed like the pins: a value that cannot be read
            // is recorded as a parse error, which refuses every write
            // until it is fixed. A `safety.read_only = ture` that
            // silently meant `false` would be the exact defect the pin
            // parser was rewritten to stop.
            "safety.read_only" => match parse_bool(&value) {
                Some(b) => cfg.safety_read_only = b,
                None => {
                    cfg.safety_parse_errors.push(format!(
                        "safety.read_only = {value} is not a boolean (expected true or false)"
                    ));
                    cfg.passthrough.push(line.to_string());
                }
            },
            other if other.starts_with("safety.") => {
                cfg.safety_parse_errors.push(format!(
                    "{other} is not a safety setting this version understands \
                     (expected safety.read_only, safety.envs.NAME.read_only \
                     or safety.accounts.NAME.read_only)"
                ));
                cfg.passthrough.push(line.to_string());
            }
            // Unrecognised at the top level too — preserved rather
            // than dropped, for the same reason as the account fields
            // above.
            _ => cfg.passthrough.push(line.to_string()),
        }
    }
    cfg.app_tabs.finish();
    cfg
}

/// What says whether `config.toml` changed: its modification time and
/// its size. The size as well because a filesystem with one-second
/// times misses an edit made in the same second as the last read.
pub(crate) type ConfigStamp = (std::time::SystemTime, u64);

/// `path`'s [`ConfigStamp`], or `None` when it is absent or unreadable.
pub(crate) fn stamp_of(path: &std::path::Path) -> Option<ConfigStamp> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

pub(crate) fn config_path() -> PathBuf {
    config_file("config.toml")
}

/// Serialise the config back to disk. Round-trips the parse format and
/// over-writes the user's existing file. Used by the `:settings` form.
/// Atomic — writes to a sibling `.tmp` and renames into place so a
/// crash mid-write can't truncate `config.toml`.
pub(crate) fn save(path: &std::path::Path, cfg: &Config) -> std::io::Result<()> {
    let existing = std::fs::read_to_string(path);
    refuse_to_clobber_unreadable(existing.as_ref().map(|_| ()).map_err(clone_io_error), path)?;
    let body = body_for_save(cfg, existing.ok().as_deref());
    crate::util::write_atomic(path, &body)
}

/// The part of the config that applies without a restart: what decides
/// which metrics and alarms belong to an env. The TUI re-reads it when
/// the file changes and the MCP server on every call, so a hand edit to
/// either half lands everywhere at once. `alarm_dimensions` used to be
/// read once at startup beside `app.` lines read live, so an edit to it
/// matched alarms by the old dimensions and the new prefix.
#[derive(Clone, Debug)]
pub(crate) struct LiveConfig {
    pub app_tabs: crate::app_tab::AppTabs,
    pub alarm_dimensions: Vec<String>,
}

impl LiveConfig {
    pub(crate) fn of(cfg: &Config) -> Self {
        Self {
            app_tabs: cfg.app_tabs.clone(),
            alarm_dimensions: cfg.alarm_dimensions.clone(),
        }
    }
}

/// The live config a read of the config file gives: what it parses to;
/// the defaults when there is no file; `None` (keep what you have, and
/// try again later) when it cannot be read. The TUI's reload and the
/// MCP server's per-call read both decide by this.
pub(crate) fn live_config_from_read(read: std::io::Result<String>) -> Option<LiveConfig> {
    match read {
        Ok(text) => Some(LiveConfig::of(&parse(&text))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Some(LiveConfig::of(&Config::default()))
        }
        Err(_) => None,
    }
}

fn clone_io_error(e: &std::io::Error) -> std::io::Error {
    std::io::Error::new(e.kind(), e.to_string())
}

/// `text` with `env`'s `app.` lines replaced by `lines`: every other
/// line, comments and other envs' `app.` lines included, is kept as it
/// was. The new lines go where the env's first old one was, or at the
/// end.
pub(crate) fn replace_app_lines(text: &str, env: &str, lines: &[String]) -> String {
    // Whose line it is, exactly as the parser reads it: `app. e.namespace`
    // is env e's (left, it would re-apply the old value), and
    // `app .e.namespace` is not an `app.` line at all (deleting it would
    // lose a line the parser never used).
    let is_envs = |l: &str| crate::app_tab::key_of(l).is_some_and(|(e, _)| e == env);
    let mut out: Vec<String> = Vec::new();
    let mut placed = false;
    for l in text.lines() {
        if is_envs(l) {
            if !placed {
                out.extend(lines.iter().cloned());
                placed = true;
            }
            continue;
        }
        out.push(l.to_string());
    }
    if !placed {
        out.extend(lines.iter().cloned());
    }
    // Nothing to write and nothing kept: an empty file, not one line.
    if out.is_empty() {
        return String::new();
    }
    // In the file's own line ending: a CRLF file stays CRLF.
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut body = out.join(eol);
    body.push_str(eol);
    body
}

/// Write `env`'s App tab lines into `config.toml`, touching nothing
/// else; refused, like [`save`], when the file exists and cannot be read.
/// Returns what was written, so the caller applies exactly that rather
/// than reading the file back (a failed read-back emptied the tab).
pub(crate) fn save_app_lines(
    path: &std::path::Path,
    env: &str,
    lines: &[String],
) -> std::io::Result<String> {
    let existing = std::fs::read_to_string(path);
    refuse_to_clobber_unreadable(existing.as_ref().map(|_| ()).map_err(clone_io_error), path)?;
    let body = replace_app_lines(existing.as_deref().unwrap_or(""), env, lines);
    crate::util::write_atomic(path, &body)?;
    Ok(body)
}

/// What a save writes: `cfg`, except that the `app.` lines are the ones
/// in the file now rather than the ones read at startup.
///
/// `:settings` does not edit them, and they are hand-written: an
/// operator who edits them while ebman runs and then saves `:settings`
/// would otherwise have the edit silently reverted to the startup copy.
/// The same reason `lint.fix_disable` is re-read from disk.
fn body_for_save(cfg: &Config, existing: Option<&str>) -> String {
    match existing {
        Some(text) => {
            let mut cfg = cfg.clone();
            cfg.app_tabs = parse(text).app_tabs;
            serialize(&cfg)
        }
        None => serialize(cfg),
    }
}

/// Refuse to overwrite a `config.toml` that exists but could not be
/// read.
///
/// `write_atomic` renames over the target using DIRECTORY permissions,
/// so a file the process cannot READ can still be replaced. That turns
/// the fail-closed refusal into the thing it was protecting against:
/// an unreadable config refuses every write, the refusal sends the
/// operator to `:settings` to investigate, and saving there replaces
/// the file — with a serialisation of a config that has none of its
/// pins, because they could not be parsed. The refusal then lifts and
/// the environment is writeable.
///
/// A *missing* file is fine — that is the ordinary first-run case, and
/// there is nothing to destroy.
///
/// Takes the read result rather than performing it, so both arms are
/// testable without engineering an unreadable file.
fn refuse_to_clobber_unreadable(
    read: std::io::Result<()>,
    path: &std::path::Path,
) -> std::io::Result<()> {
    match read {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(std::io::Error::new(
            e.kind(),
            format!(
                "{} exists but could not be read ({e}) — refusing to \
                 overwrite it, because anything in it (including safety \
                 pins) would be lost. Fix the file's permissions or \
                 encoding, then retry.",
                path.display()
            ),
        )),
    }
}

/// Pure: render a `Config` into the TOML-ish line-oriented format the
/// parser reads. Used by `save` and unit tests.
pub(crate) fn serialize(cfg: &Config) -> String {
    let mut out = String::new();
    out.push_str("# ebman configuration — written by :settings; hand-edits welcome\n\n");
    out.push_str(&format!(
        "refresh_interval_secs = {}\n",
        cfg.refresh_interval.as_secs()
    ));
    out.push_str(&format!(
        "extra_regions = \"{}\"\n",
        cfg.extra_regions.join(",")
    ));
    if let Some(b) = cfg.redact_default {
        out.push_str(&format!("redact_default = {b}\n"));
    }
    if let Some(b) = cfg.grouped_default {
        out.push_str(&format!("grouped_default = {b}\n"));
    }
    // Only when it differs from the default: writing `true` into every
    // saved config would make the key look like something :settings
    // manages, and it is deliberately operator-only.
    if cfg.safety_read_only {
        out.push_str("safety.read_only = true\n");
    }
    if !cfg.mcp_peek_bodies {
        out.push_str("mcp.peek_bodies = false\n");
    }
    if cfg.mcp_require_role {
        out.push_str("mcp.require_role = true\n");
    }
    for (name, role) in &cfg.mcp_roles {
        out.push_str(&format!(
            "mcp.role.{name}.role_arn = \"{}\"\n",
            role.role_arn
        ));
        if !role.verbs.is_empty() {
            out.push_str(&format!(
                "mcp.role.{name}.verbs = \"{}\"\n",
                role.verbs.join(",")
            ));
        }
        if let Some(v) = &role.source_profile {
            out.push_str(&format!("mcp.role.{name}.source_profile = \"{v}\"\n"));
        }
        if let Some(v) = &role.external_id {
            out.push_str(&format!("mcp.role.{name}.external_id = \"{v}\"\n"));
        }
    }
    out.push_str(&format!("theme = \"{}\"\n", cfg.theme));
    out.push_str(&format!("icons = \"{}\"\n", cfg.icons));
    out.push_str(&format!("notify_bell = {}\n", cfg.notify_bell));
    if let Some(url) = &cfg.notify_webhook {
        out.push_str(&format!("notify_webhook = \"{url}\"\n"));
    }
    if !cfg.lint_disable.is_empty() {
        out.push_str(&format!(
            "lint.disable = \"{}\"\n",
            cfg.lint_disable.join(",")
        ));
    }
    if !cfg.lint_fix_disable.is_empty() {
        out.push_str(&format!(
            "lint.fix_disable = \"{}\"\n",
            cfg.lint_fix_disable.join(",")
        ));
    }
    if cfg.explain_enabled {
        out.push_str("explain.enabled = true\n");
    }
    if !cfg.explain_provider.is_empty() {
        out.push_str(&format!(
            "explain.provider = \"{}\"\n",
            cfg.explain_provider
        ));
    }
    if !cfg.explain_model.is_empty() {
        out.push_str(&format!("explain.model = \"{}\"\n", cfg.explain_model));
    }
    if !cfg.explain_api_key_env.is_empty() {
        out.push_str(&format!(
            "explain.api_key_env = \"{}\"\n",
            cfg.explain_api_key_env
        ));
    }
    if !cfg.explain_ollama_url.is_empty() {
        out.push_str(&format!(
            "explain.ollama_url = \"{}\"\n",
            cfg.explain_ollama_url
        ));
    }
    if cfg.explain_max_tokens != 0 {
        out.push_str(&format!(
            "explain.max_tokens = {}\n",
            cfg.explain_max_tokens
        ));
    }
    if !cfg.command_aliases.is_empty() {
        // Sort so repeated serialize cycles don't churn the file
        // when the HashMap iteration order shuffles.
        let mut pairs: Vec<(&String, &String)> = cfg.command_aliases.iter().collect();
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        for (name, expansion) in pairs {
            out.push_str(&format!("alias.{name} = \"{expansion}\"\n"));
        }
    }
    if !cfg.required_tags.is_empty() {
        out.push_str(&format!(
            "required_tags = \"{}\"\n",
            cfg.required_tags.join(",")
        ));
    }
    if !cfg.profile_themes.is_empty() {
        // Sort entries so repeated serialize cycles don't churn the file
        // when the HashMap iteration order shuffles.
        let mut pairs: Vec<(&String, &String)> = cfg.profile_themes.iter().collect();
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        let joined = pairs
            .iter()
            .map(|(k, v)| format!("{k}:{v}"))
            .collect::<Vec<_>>()
            .join(",");
        out.push_str(&format!("profile_themes = \"{joined}\"\n"));
    }
    if !cfg.runbooks.is_empty() {
        // Sorted so repeated serialize cycles don't churn the file.
        let mut pairs: Vec<(&String, &String)> = cfg.runbooks.iter().collect();
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        for (env, url) in pairs {
            out.push_str(&format!("runbooks.{env} = \"{url}\"\n"));
        }
    }
    // Verbatim and in order: the App tab is hand-written config that
    // `:settings` does not manage, and rebuilding it from the parsed
    // form would drop a line that failed to apply.
    for line in &cfg.app_tabs.lines {
        out.push_str(line);
        out.push('\n');
    }
    if !cfg.safety_envs.is_empty() {
        let mut pairs: Vec<(&String, &bool)> = cfg.safety_envs.iter().collect();
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        for (env, ro) in pairs {
            out.push_str(&format!("safety.envs.{env}.read_only = {ro}\n"));
        }
    }
    if !cfg.safety_accounts.is_empty() {
        let mut pairs: Vec<(&String, &bool)> = cfg.safety_accounts.iter().collect();
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        for (acct, ro) in pairs {
            out.push_str(&format!("safety.accounts.{acct}.read_only = {ro}\n"));
        }
    }
    // Only the names the operator ADDED — the canonical one is implicit
    // and always matched. Emitting the full list would rewrite a
    // hand-written `alarm_dimensions = "Environment"` as
    // `"EnvironmentName,Environment"` on the next `:settings` save.
    let mut extra: Vec<String> = cfg
        .alarm_dimensions
        .iter()
        .filter(|d| d.as_str() != crate::aws::ENV_DIMENSION)
        .cloned()
        .collect();
    // A deliberate opt-out has to survive the round trip too: without
    // re-emitting the `-`, a `:settings` save silently reinstated the
    // canonical name and the false positive with it.
    if !cfg
        .alarm_dimensions
        .iter()
        .any(|d| d == crate::aws::ENV_DIMENSION)
    {
        extra.push(format!("-{}", crate::aws::ENV_DIMENSION));
    }
    if !extra.is_empty() {
        out.push_str(&format!("alarm_dimensions = \"{}\"\n", extra.join(",")));
    }
    // AssumeRole account definitions. These were parsed but never
    // written, so a `:settings` save — which rewrites the whole file
    // from this function — deleted every one of them and broke
    // `:account <name>` with nothing on screen to say why.
    // Lines the parser didn't model, verbatim and last. Without this a
    // `:settings` save silently deleted a mistyped key — the very line
    // the operator would otherwise notice and fix — along with any key
    // a newer release understands and this build doesn't.
    if !cfg.passthrough.is_empty() {
        out.push_str("\n# Preserved from the previous file (not managed by :settings):\n");
        for line in &cfg.passthrough {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !cfg.accounts.is_empty() {
        let mut names: Vec<&String> = cfg.accounts.keys().collect();
        names.sort();
        for name in names {
            let Some(spec) = cfg.accounts.get(name) else {
                continue;
            };
            // An absent ARN omits that one line — it does NOT drop the
            // block. Skipping the whole account destroyed its valid
            // `source_profile` / `region` lines too, trading one bad
            // line for three; and emitting `role_arn = ""` replaced the
            // operator's real line with a plausible-looking empty one.
            // Omitting round-trips: the block re-parses exactly as it
            // was written.
            if !spec.role_arn.trim().is_empty() {
                out.push_str(&format!(
                    "accounts.{name}.role_arn = \"{}\"\n",
                    spec.role_arn
                ));
            }
            if let Some(v) = &spec.source_profile {
                out.push_str(&format!("accounts.{name}.source_profile = \"{v}\"\n"));
            }
            if let Some(v) = &spec.external_id {
                out.push_str(&format!("accounts.{name}.external_id = \"{v}\"\n"));
            }
            if let Some(v) = &spec.region {
                out.push_str(&format!("accounts.{name}.region = \"{v}\"\n"));
            }
        }
    }
    out
}

/// Pure: parse a `prof:theme,prof:theme` string into a map. Empty / `:`
/// -free / blank-key / blank-value tokens are skipped. Whitespace around
/// each side of the colon is trimmed so the operator can format it for
/// readability.
pub(crate) fn parse_profile_themes(raw: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for token in raw.split(',') {
        let Some((k, v)) = token.split_once(':') else {
            continue;
        };
        let key = k.trim();
        let val = v.trim();
        if key.is_empty() || val.is_empty() {
            continue;
        }
        out.insert(key.to_string(), val.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERBS: &[&str] = &["deploy", "restart", "rebuild", "terminate"];

    #[test]
    fn mcp_roles_parse_and_write_back() {
        let cfg = parse(
            "mcp.require_role = true\n\
             mcp.role.team.deploys.role_arn = \"arn:aws:iam::111122223333:role/D\"\n\
             mcp.role.team.deploys.verbs = \"deploy, restart\"\n\
             mcp.role.team.deploys.external_id = \"x-1\"\n\
             mcp.role.team.deploys.source_profile = \"ops\"\n\
             mcp.role.team.deploys.future = \"kept\"\n",
        );
        assert!(cfg.mcp_require_role);
        let role = cfg
            .mcp_roles
            .get("team.deploys")
            .expect("a dotted name works");
        assert_eq!(role.role_arn, "arn:aws:iam::111122223333:role/D");
        assert_eq!(role.verbs, ["deploy", "restart"]);
        assert_eq!(role.external_id.as_deref(), Some("x-1"));
        assert_eq!(role.source_profile.as_deref(), Some("ops"));
        let again = parse(&serialize(&cfg));
        assert_eq!(again.mcp_roles, cfg.mcp_roles);
        assert!(again.mcp_require_role);
        assert!(
            serialize(&cfg).contains("mcp.role.team.deploys.future = \"kept\""),
            "an unknown field is preserved, not dropped"
        );
    }

    /// An unreadable `mcp.require_role` is not "off": an operator who
    /// wrote the key meant to restrict.
    #[test]
    fn an_unreadable_require_role_restricts() {
        assert!(parse("mcp.require_role = yes please\n").mcp_require_role);
        assert!(!parse("mcp.require_role = false\n").mcp_require_role);
        assert!(!parse("").mcp_require_role);
    }

    #[test]
    fn mcp_role_check_maps_a_verb_or_says_why_not() {
        let cfg = parse(
            "mcp.role.deploys.role_arn = \"arn:aws:iam::111122223333:role/R\"\n\
             mcp.role.deploys.verbs = \"deploy,restart\"\n",
        );
        assert_eq!(
            mcp_role_check(&cfg, "deploy", VERBS),
            RoleCheck::Mapped {
                role: "deploys".into(),
                arn: RoleArn {
                    arn: "arn:aws:iam::111122223333:role/R".into(),
                    account: "111122223333".into(),
                    name: "R".into(),
                },
            }
        );
        assert_eq!(
            mcp_role_check(&cfg, "terminate", VERBS),
            RoleCheck::Unmapped { required: false }
        );
        let strict = parse(
            "mcp.require_role = true\n\
             mcp.role.deploys.role_arn = \"arn:aws:iam::111122223333:role/R\"\n\
             mcp.role.deploys.verbs = \"deploy\"\n",
        );
        assert_eq!(
            mcp_role_check(&strict, "terminate", VERBS),
            RoleCheck::Unmapped { required: true }
        );
        // No roles at all: unmapped, as today.
        assert_eq!(
            mcp_role_check(&parse(""), "deploy", VERBS),
            RoleCheck::Unmapped { required: false }
        );
    }

    /// Each way the role config cannot be acted on fails closed, for
    /// every verb: choosing would be the fail-open the safety parser was
    /// rewritten to stop.
    #[test]
    fn a_role_config_that_cannot_be_acted_on_fails_closed() {
        let unreadable = |text: &str, expect: &str| {
            let cfg = parse(text);
            for verb in VERBS {
                match mcp_role_check(&cfg, verb, VERBS) {
                    RoleCheck::Unreadable { problem } => {
                        assert!(problem.contains(expect), "{text}: {problem}")
                    }
                    other => panic!("{text}: `{verb}` was {other:?}"),
                }
            }
        };
        unreadable(
            "mcp.role.a.role_arn = \"arn:aws:iam::111122223333:role/A\"\nmcp.role.a.verbs = \"deploy\"\n\
             mcp.role.b.role_arn = \"arn:aws:iam::111122223333:role/B\"\nmcp.role.b.verbs = \"deploy\"\n",
            "more than one role (a, b)",
        );
        unreadable(
            "mcp.role.a.role_arn = \"arn:aws:iam::111122223333:role/A\"\nmcp.role.a.verbs = \"deplo\"\n",
            "`deplo`, which is not an MCP write verb",
        );
        // A TOML array, where the house form is a comma list.
        unreadable(
            "mcp.role.a.role_arn = \"arn:aws:iam::111122223333:role/A\"\nmcp.role.a.verbs = [\"deploy\"]\n",
            "give a comma list",
        );
        unreadable(
            "mcp.role.a.verbs = \"deploy\"\n",
            "mcp.role.a has no role_arn",
        );
        unreadable(
            "mcp.role.a.role_arn = \"arn:aws:iam::1:role/A\"\nmcp.role.a.verbs = \"deploy\"\n",
            "mcp.role.a.role_arn \"arn:aws:iam::1:role/A\" is not an IAM role ARN",
        );
    }

    /// What failed to parse reaches an agent and a status bar escaped,
    /// never raw: a direction override or a newline in a TOML escape
    /// must not rewrite the line.
    #[test]
    fn an_unreadable_role_arn_is_quoted_escaped() {
        let mut cfg = parse("mcp.role.a.verbs = \"deploy\"\n");
        cfg.mcp_roles.get_mut("a").unwrap().role_arn =
            "arn:aws:iam::111122223333:role/D\u{202E}\nx".into();
        let RoleCheck::Unreadable { problem } = mcp_role_check(&cfg, "deploy", VERBS) else {
            panic!("unreadable");
        };
        assert!(
            !problem.contains('\u{202E}') && !problem.contains('\n'),
            "{problem:?}"
        );
        assert!(problem.contains("\\u{202e}"), "{problem:?}");
    }

    #[test]
    fn a_role_arn_gives_its_account_and_name() {
        let ok = |arn: &str, account: &str, name: &str| {
            assert_eq!(
                parse_role_arn(arn),
                Some(RoleArn {
                    arn: arn.trim().into(),
                    account: account.into(),
                    name: name.into()
                }),
                "{arn}"
            )
        };
        ok(
            "arn:aws:iam::111122223333:role/Deploy",
            "111122223333",
            "Deploy",
        );
        ok(
            "arn:aws-us-gov:iam::111122223333:role/team/ci/Deploy",
            "111122223333",
            "Deploy",
        );
        ok(" arn:aws:iam::111122223333:role/D ", "111122223333", "D");
        ok(
            "arn:aws-iso-b:iam::111122223333:role/D",
            "111122223333",
            "D",
        );
        ok(
            "arn:aws:iam::111122223333:role/a+b=c,d.e@f_g-h",
            "111122223333",
            "a+b=c,d.e@f_g-h",
        );
        let long_name = format!("arn:aws:iam::111122223333:role/{}", "n".repeat(64));
        assert!(parse_role_arn(&long_name).is_some(), "64 is IAM's limit");
        let long_path = format!("arn:aws:iam::111122223333:role/{}/D", "p".repeat(510));
        assert!(
            parse_role_arn(&long_path).is_some(),
            "512 with its two slashes"
        );
        for bad in [
            "",
            "arn:r",
            "arn:aws:iam::111122223333:user/ops",
            "arn:aws:sts::111122223333:assumed-role/D/s",
            "arn:aws:s3::111122223333:role/D",
            "arn:aws:iam:us-east-1:111122223333:role/D",
            "arn:aws:iam::11112222333:role/D",
            "arn:aws:iam::11112222333x:role/D",
            "arn:aws:iam::111122223333:role/",
            "arn::iam::111122223333:role/D",
            "nar:aws:iam::111122223333:role/D",
            "arn:foo bar:iam::111122223333:role/D",
            "arn:azure:iam::111122223333:role/D",
            "nar:aws-iso:iam::111122223333:role/D",
            "arn:aws:iam::111122223333:role/a:b",
            "arn:aws:iam::111122223333:role/a b",
            "arn:aws:iam::111122223333:role/`D`",
            "arn:aws:iam::111122223333:role/D\u{202E}x",
            "arn:aws:iam::111122223333:role/D\nx",
            "arn:aws:iam::111122223333:role/team x/D",
            "arn:aws:iam::111122223333:role//D",
            "arn:aws:iam::111122223333:role/a//D",
            "arn:aws:iam::111122223333:role/a/b//D",
        ] {
            assert_eq!(parse_role_arn(bad), None, "{bad:?}");
        }
        let too_long_name = format!("arn:aws:iam::111122223333:role/{}", "n".repeat(65));
        assert_eq!(parse_role_arn(&too_long_name), None);
        let too_long_path = format!("arn:aws:iam::111122223333:role/{}/D", "p".repeat(511));
        assert_eq!(parse_role_arn(&too_long_path), None);
    }

    /// What a read of the config file means for the `app.` lines, for
    /// the TUI's reload and the MCP server alike.
    #[test]
    fn a_read_of_the_config_file_gives_its_app_lines_none_or_keep() {
        let read = |r| live_config_from_read(r).map(|t| t.app_tabs.envs.len());
        assert_eq!(read(Ok("app.e.namespace = \"N\"\n".into())), Some(1));
        let err = |kind| Err(std::io::Error::from(kind));
        assert_eq!(
            read(err(std::io::ErrorKind::NotFound)),
            Some(0),
            "no file: no config"
        );
        assert_eq!(
            read(err(std::io::ErrorKind::PermissionDenied)),
            None,
            "unreadable: keep"
        );
    }

    #[test]
    fn parse_overrides_defaults() {
        let text = r#"
refresh_interval_secs = 30
extra_regions = "us-gov-east-1, cn-north-1"
redact_default = true
grouped_default = false
"#;
        let cfg = parse(text);
        assert_eq!(cfg.refresh_interval, Duration::from_secs(30));
        assert_eq!(
            cfg.extra_regions,
            vec!["us-gov-east-1".to_string(), "cn-north-1".to_string()]
        );
        assert_eq!(cfg.redact_default, Some(true));
        assert_eq!(cfg.grouped_default, Some(false));
    }

    #[test]
    fn parse_profile_themes_happy_path() {
        let map = parse_profile_themes("prod:high-contrast,staging:dark,default:light");
        assert_eq!(map.get("prod"), Some(&"high-contrast".to_string()));
        assert_eq!(map.get("staging"), Some(&"dark".to_string()));
        assert_eq!(map.get("default"), Some(&"light".to_string()));
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn parse_profile_themes_trims_whitespace_and_skips_malformed() {
        // Trailing comma, missing colon, blank key, blank value all
        // produce no entries rather than panicking or yielding ""→"".
        let map = parse_profile_themes(
            "  prod : high-contrast , noseparator , :empty-key , empty-value: , ",
        );
        assert_eq!(map.get("prod"), Some(&"high-contrast".to_string()));
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn parse_profile_themes_empty_returns_empty_map() {
        assert!(parse_profile_themes("").is_empty());
    }

    #[test]
    fn parse_accounts_collects_multiline_specs() {
        let text = r#"
accounts.prod.role_arn = "arn:aws:iam::111122223333:role/EbmanReadOnly"
accounts.prod.source_profile = "default"
accounts.prod.region = "eu-west-2"
accounts.staging.role_arn = "arn:aws:iam::555555555555:role/EbmanReadOnly"
accounts.staging.external_id = "abc-xyz"
"#;
        let cfg = parse(text);
        assert_eq!(cfg.accounts.len(), 2);
        let prod = cfg.accounts.get("prod").expect("prod entry");
        assert_eq!(
            prod.role_arn,
            "arn:aws:iam::111122223333:role/EbmanReadOnly"
        );
        assert_eq!(prod.source_profile.as_deref(), Some("default"));
        assert_eq!(prod.region.as_deref(), Some("eu-west-2"));
        assert_eq!(prod.external_id, None);
        let staging = cfg.accounts.get("staging").expect("staging entry");
        assert_eq!(staging.external_id.as_deref(), Some("abc-xyz"));
        assert_eq!(staging.source_profile, None);
    }

    #[test]
    fn parse_accounts_ignores_unknown_field() {
        // Future-compat: a field we don't recognise should be ignored
        // rather than dropping the whole entry.
        let cfg = parse(
            "accounts.prod.role_arn = \"arn:…\"\n\
             accounts.prod.future_field = \"whatever\"\n",
        );
        let prod = cfg.accounts.get("prod").expect("prod entry");
        assert_eq!(prod.role_arn, "arn:…");
    }

    #[test]
    fn parse_runbooks_maps_env_to_url() {
        let cfg = parse(
            "runbooks.poly-prod = \"https://wiki/runbook/prod\"\n\
             runbooks.poly-staging = \"https://wiki/runbook/staging\"\n",
        );
        assert_eq!(cfg.runbooks.len(), 2);
        assert_eq!(
            cfg.runbooks.get("poly-prod").map(String::as_str),
            Some("https://wiki/runbook/prod")
        );
        // Blank URL is skipped — a stray `runbooks.x =` can't smuggle in
        // an empty mapping.
        assert!(parse("runbooks.x = \"\"\n").runbooks.is_empty());
    }

    #[test]
    fn runbooks_round_trip_through_serialize() {
        let mut cfg = Config::default();
        cfg.runbooks.insert("prod".into(), "https://rb/prod".into());
        let reparsed = parse(&serialize(&cfg));
        assert_eq!(
            reparsed.runbooks.get("prod").map(String::as_str),
            Some("https://rb/prod")
        );
    }

    #[test]
    fn parse_safety_envs_and_accounts() {
        let cfg = parse(
            "safety.envs.poly-prod.read_only = true\n\
             safety.envs.poly-staging.read_only = false\n\
             safety.accounts.prod.read_only = true\n",
        );
        assert_eq!(cfg.safety_envs.get("poly-prod"), Some(&true));
        assert_eq!(cfg.safety_envs.get("poly-staging"), Some(&false));
        assert_eq!(cfg.safety_accounts.get("prod"), Some(&true));
        // Unknown field under safety.envs.NAME is ignored.
        let cfg = parse("safety.envs.x.future_field = \"whatever\"\n");
        assert!(cfg.safety_envs.is_empty());
    }

    #[test]
    fn safety_round_trips_through_serialize() {
        let mut cfg = Config::default();
        cfg.safety_envs.insert("poly-prod".into(), true);
        cfg.safety_accounts.insert("prod".into(), true);
        let reparsed = parse(&serialize(&cfg));
        assert_eq!(reparsed.safety_envs.get("poly-prod"), Some(&true));
        assert_eq!(reparsed.safety_accounts.get("prod"), Some(&true));
    }

    #[test]
    fn parse_writes_profile_themes_into_config() {
        // End-to-end check: a config file with `profile_themes = "..."`
        // ends up in cfg.profile_themes correctly.
        let cfg = parse("profile_themes = \"prod:high-contrast,staging:dark\"\n");
        assert_eq!(cfg.profile_themes.len(), 2);
        assert_eq!(
            cfg.profile_themes.get("prod"),
            Some(&"high-contrast".to_string())
        );
    }

    #[test]
    fn parse_ignores_zero_interval() {
        let cfg = parse("refresh_interval_secs = 0\n");
        assert_eq!(cfg.refresh_interval, Duration::from_secs(15));
    }

    #[test]
    fn parse_empty_returns_defaults() {
        let cfg = parse("");
        assert_eq!(cfg.refresh_interval, Duration::from_secs(15));
        assert!(cfg.extra_regions.is_empty());
        assert!(cfg.redact_default.is_none());
    }

    #[test]
    fn parse_icons_auto_is_preserved() {
        let cfg = parse("icons = \"auto\"\n");
        assert_eq!(cfg.icons, "auto");
    }

    #[test]
    fn serialize_round_trips_full_config() {
        let mut profile_themes = std::collections::HashMap::new();
        profile_themes.insert("prod".into(), "high-contrast".into());
        profile_themes.insert("staging".into(), "dark".into());
        let cfg = Config {
            // Set, so the round-trip proves a standing write refusal
            // survives a :settings save rather than defaulting away.
            safety_read_only: true,
            refresh_interval: Duration::from_secs(45),
            extra_regions: vec!["eu-south-2".into(), "ap-southeast-4".into()],
            redact_default: Some(true),
            grouped_default: Some(false),
            theme: "high-contrast".into(),
            icons: "powerline".into(),
            notify_bell: true,
            required_tags: vec!["Owner".into(), "Env".into()],
            alarm_dimensions: vec![crate::aws::ENV_DIMENSION.to_string()],
            passthrough: Vec::new(),
            profile_themes,
            accounts: std::collections::HashMap::new(),
            runbooks: std::collections::HashMap::new(),
            app_tabs: parse(
                "app.Shop-prod.namespace = \"Shop\"\n\
                 app.Shop-prod.metric.heap = \"heapUsedMb|Average|MB\"\n",
            )
            .app_tabs,
            safety_envs: std::collections::HashMap::new(),
            safety_accounts: std::collections::HashMap::new(),
            safety_parse_errors: Vec::new(),
            mcp_peek_bodies: true,
            mcp_roles: [(
                "deploys".to_string(),
                McpRole {
                    role_arn: "arn:aws:iam::111122223333:role/EbmanAgentDeploy".into(),
                    source_profile: Some("ops".into()),
                    external_id: Some("x-1".into()),
                    verbs: vec!["deploy".into(), "restart".into()],
                },
            )]
            .into_iter()
            .collect(),
            mcp_require_role: true,
            terraform_state_path: None,
            notify_webhook: Some("https://hooks.slack.com/services/EXAMPLE".into()),
            command_aliases: {
                let mut m = std::collections::HashMap::new();
                m.insert("dp".to_string(), "deploy --auto-rollback 5m".to_string());
                m.insert(
                    "shipit".to_string(),
                    "promote-env staging prod --wait-for-green 5m".to_string(),
                );
                m
            },
            lint_disable: vec!["EBL003".into(), "EBL006".into()],
            lint_fix_disable: vec!["EBL004".into()],
            explain_enabled: true,
            explain_provider: "anthropic".into(),
            explain_model: "claude-haiku-4-5".into(),
            explain_api_key_env: "ANTHROPIC_API_KEY".into(),
            explain_ollama_url: "http://localhost:11434".into(),
            explain_max_tokens: 2048,
        };

        let body = serialize(&cfg);
        let reparsed = parse(&body);
        assert_eq!(reparsed.refresh_interval, cfg.refresh_interval);
        assert_eq!(reparsed.extra_regions, cfg.extra_regions);
        assert_eq!(reparsed.redact_default, cfg.redact_default);
        assert_eq!(reparsed.grouped_default, cfg.grouped_default);
        assert_eq!(reparsed.theme, cfg.theme);
        assert_eq!(reparsed.icons, cfg.icons);
        assert_eq!(reparsed.notify_bell, cfg.notify_bell);
        assert_eq!(reparsed.required_tags, cfg.required_tags);
        assert_eq!(reparsed.profile_themes, cfg.profile_themes);
        assert_eq!(reparsed.notify_webhook, cfg.notify_webhook);
        assert_eq!(reparsed.command_aliases, cfg.command_aliases);
        assert_eq!(reparsed.lint_disable, cfg.lint_disable);
        assert_eq!(reparsed.lint_fix_disable, cfg.lint_fix_disable);
        assert_eq!(reparsed.explain_enabled, cfg.explain_enabled);
        assert_eq!(reparsed.explain_provider, cfg.explain_provider);
        assert_eq!(reparsed.explain_model, cfg.explain_model);
        assert_eq!(reparsed.explain_api_key_env, cfg.explain_api_key_env);
        assert_eq!(reparsed.explain_ollama_url, cfg.explain_ollama_url);
        assert_eq!(reparsed.explain_max_tokens, cfg.explain_max_tokens);
        assert!(
            !cfg.app_tabs.envs.is_empty(),
            "the fixture must configure an App tab"
        );
        assert_eq!(reparsed.app_tabs, cfg.app_tabs);
    }

    #[test]
    fn app_checks_that_need_every_line_run_after_parsing() {
        // A metric with no namespace is only knowable once every line
        // is in, so `parse` must call `finish`.
        let cfg = parse("app.e.metric.x = \"X\"\n");
        assert_eq!(cfg.app_tabs.errors.len(), 1, "{:?}", cfg.app_tabs.errors);
    }

    #[test]
    fn a_stamp_changes_with_the_size_even_within_the_second() {
        // A private file, not the shared test config dir.
        let path = std::env::temp_dir().join(format!("ebman-stamp-{}", std::process::id()));
        std::fs::write(&path, "a").unwrap();
        let first = stamp_of(&path).expect("stamped");
        std::fs::write(&path, "abc").unwrap();
        let second = stamp_of(&path).expect("stamped");
        assert_eq!((first.1, second.1), (1, 3));
        assert_ne!(
            first, second,
            "an edit within the same second is still an edit"
        );
        std::fs::remove_file(&path).unwrap();
        assert_eq!(stamp_of(&path), None, "no file, no stamp");
    }

    #[test]
    fn replacing_one_envs_app_lines_keeps_everything_else() {
        let text =
            "# mine\ntheme = \"dark\"\napp.a.namespace = \"Old\"\napp.ab.namespace = \"Other\"\n\
                    app.a.metric.x = \"X\"\nalias.dp = \"deploy\"\n";
        let out = replace_app_lines(text, "a", &["app.a.namespace = \"New\"".to_string()]);
        assert_eq!(
            out,
            "# mine\ntheme = \"dark\"\napp.a.namespace = \"New\"\napp.ab.namespace = \"Other\"\n\
             alias.dp = \"deploy\"\n",
            "in place of the first old line; app.ab is another env"
        );
        let added = replace_app_lines(
            "theme = \"dark\"\n",
            "a",
            &["app.a.rds = \"db\"".to_string()],
        );
        assert_eq!(
            added, "theme = \"dark\"\napp.a.rds = \"db\"\n",
            "appended when absent"
        );
        let crlf = replace_app_lines(
            "theme = \"dark\"\r\napp.a.rds = \"x\"\r\n",
            "a",
            &["app.a.rds = \"y\"".to_string()],
        );
        assert_eq!(
            crlf, "theme = \"dark\"\r\napp.a.rds = \"y\"\r\n",
            "a CRLF file stays CRLF"
        );
        let not_app = replace_app_lines("app .a.namespace = \"Kept\"\n", "a", &[]);
        assert_eq!(
            not_app, "app .a.namespace = \"Kept\"\n",
            "not an app. line to the parser: kept"
        );
        let spaced = replace_app_lines(
            "app. a.namespace = \"Old\"\n",
            "a",
            &["app.a.namespace = \"New\"".to_string()],
        );
        assert_eq!(
            spaced, "app.a.namespace = \"New\"\n",
            "a spaced key is still env a's line"
        );
        let cleared = replace_app_lines(text, "a", &[]);
        assert!(!cleared.contains("app.a."), "{cleared}");
        assert!(cleared.contains("app.ab.namespace"));
    }

    #[test]
    fn a_save_keeps_the_app_lines_in_the_file_not_the_startup_copy() {
        let at_startup = parse("app.e.namespace = \"Old\"\n");
        let edited_since = "theme = \"dark\"\napp.e.namespace = \"New\"\n";
        let written = parse(&body_for_save(&at_startup, Some(edited_since)));
        assert_eq!(written.app_tabs.envs["e"].namespace.as_deref(), Some("New"));
        // No file yet: the in-memory copy is all there is.
        let first = parse(&body_for_save(&at_startup, None));
        assert_eq!(first.app_tabs.envs["e"].namespace.as_deref(), Some("Old"));
    }

    #[test]
    fn one_pair_of_quotes_is_removed_and_no_more() {
        assert_eq!(unquote(r#""a""#), "a");
        assert_eq!(unquote(r#""msg="(?<fp>[^"]+)"""#), r#"msg="(?<fp>[^"]+)""#);
        assert_eq!(unquote(r#""""#), "");
        assert_eq!(unquote("15"), "15");
        assert_eq!(
            unquote(r#""open"#),
            "open",
            "unclosed reads as it always did"
        );
        assert_eq!(unquote(r#"close""#), "close");
        assert_eq!(unquote(r#"""#), "", "a lone quote");
        assert_eq!(
            parse("theme = \"dark\n").theme,
            "dark",
            "an unclosed theme still applies"
        );
        let cfg = parse("app.e.error_fingerprint = \"msg=\"(?<fp>[^\"]+)\"\"\n");
        assert_eq!(
            cfg.app_tabs.envs["e"].error_fingerprint.as_deref(),
            Some(r#"msg="(?<fp>[^"]+)""#),
            "a trailing quote inside the value survives"
        );
    }

    #[test]
    fn each_malformed_app_value_names_its_own_problem() {
        for (raw, want) in [
            (r#""Shop" # prod"#, Some("text after its closing quote")),
            // Ends in a quote, so only the comment check catches it.
            (r#""Shop" # the "prod""#, Some("looks like it carries")),
            (r##""Shop"# the "prod""##, Some("looks like it carries")),
            (r#""Shop"#, Some("no closing one")),
            ("Shop # prod", Some("or quote the value")),
            ("Shop\t# prod", Some("or quote the value")),
            ("ERROR#(?<fp>x)", None),
            // An escaped backslash, then the closing quote, then a comment.
            (r#""Shop\\" # the "prod""#, Some("looks like it carries")),
            // A quote written \" is the value's, not a closing one.
            (r##""href=\"#(?<fp>\w+)\"""##, None),
            (r#""Shop""#, None),
            (r#""msg="(?<fp>[^"]+)"""#, None),
            ("Shop", None),
        ] {
            let got = app_value_problem(raw);
            match want {
                None => assert_eq!(got, None, "{raw}"),
                Some(w) => assert!(got.is_some_and(|g| g.contains(w)), "{raw}: {got:?}"),
            }
        }
    }

    #[test]
    fn an_app_line_with_an_inline_comment_is_an_error_not_a_namespace() {
        let cfg = parse("app.e.namespace = \"Shop\" # prod\n");
        assert!(cfg.app_tabs.envs.is_empty(), "nothing applied");
        assert_eq!(cfg.app_tabs.lines.len(), 1, "the line is kept for save");
        assert!(
            cfg.app_tabs.errors[0]
                .message
                .contains("text after its closing quote"),
            "{:?}",
            cfg.app_tabs.errors
        );
        assert_eq!(cfg.app_tabs.errors[0].env.as_deref(), Some("e"));
        // Unquoted values are untouched by the rule.
        assert!(parse("app.e.namespace = Shop\n").app_tabs.errors.is_empty());
    }

    #[test]
    fn an_app_line_survives_a_save_even_when_it_did_not_apply() {
        // Rebuilding `app.` lines from the parsed form would drop the
        // one that failed, along with the operator's only clue to it.
        // The `=` inside the fingerprint must survive too: the parser
        // splits the line on the FIRST `=`.
        let body = "\
app.e.namespace = \"N\"
app.e.metric.x = \"X|Avg\"
app.e.error_fingerprint = \"ERROR (?<fp>\\w+)=\"
";
        let cfg = parse(body);
        assert_eq!(cfg.app_tabs.errors.len(), 1, "{:?}", cfg.app_tabs.errors);
        let reparsed = parse(&serialize(&cfg));
        assert_eq!(reparsed.app_tabs, cfg.app_tabs);
        assert_eq!(
            reparsed.app_tabs.envs["e"].error_fingerprint.as_deref(),
            Some("ERROR (?<fp>\\w+)=")
        );
        assert!(
            reparsed.passthrough.is_empty(),
            "app lines are modelled, not passed through"
        );
    }

    #[test]
    fn parse_explain_block_full_shape() {
        let body = "\
explain.enabled = true
explain.provider = \"ollama\"
explain.model = \"llama3.2\"
explain.api_key_env = \"MY_KEY\"
explain.ollama_url = \"http://10.0.0.5:11434\"
explain.max_tokens = 512
";
        let cfg = parse(body);
        assert!(cfg.explain_enabled);
        assert_eq!(cfg.explain_provider, "ollama");
        assert_eq!(cfg.explain_model, "llama3.2");
        assert_eq!(cfg.explain_api_key_env, "MY_KEY");
        assert_eq!(cfg.explain_ollama_url, "http://10.0.0.5:11434");
        assert_eq!(cfg.explain_max_tokens, 512);
    }

    #[test]
    fn parse_explain_disabled_by_default() {
        let cfg = parse("");
        assert!(!cfg.explain_enabled);
    }

    #[test]
    fn parse_lint_fix_disable_csv_collects_into_vec() {
        let body = "lint.fix_disable = \"EBL004, EBL001\"\n";
        let cfg = parse(body);
        assert_eq!(cfg.lint_fix_disable, vec!["EBL004", "EBL001"]);
    }

    #[test]
    fn parse_lint_disable_csv_collects_into_vec() {
        let body = "lint.disable = \"EBL001, EBL003 ,EBL006\"\n";
        let cfg = parse(body);
        assert_eq!(cfg.lint_disable, vec!["EBL001", "EBL003", "EBL006"]);
    }

    #[test]
    fn parse_lint_disable_skips_empty_tokens() {
        // Trailing comma / double-comma should not produce empty
        // rule IDs that would silently never match.
        let body = "lint.disable = \"EBL001,,EBL003,\"\n";
        let cfg = parse(body);
        assert_eq!(cfg.lint_disable, vec!["EBL001", "EBL003"]);
    }

    #[test]
    fn parse_alias_lines_collect_into_command_aliases() {
        let body = "alias.dp = \"deploy --auto-rollback 5m\"\nalias.foo = \"rebuild\"\n";
        let cfg = parse(body);
        assert_eq!(cfg.command_aliases.len(), 2);
        assert_eq!(
            cfg.command_aliases.get("dp").map(|s| s.as_str()),
            Some("deploy --auto-rollback 5m"),
        );
        assert_eq!(
            cfg.command_aliases.get("foo").map(|s| s.as_str()),
            Some("rebuild"),
        );
    }

    #[test]
    fn parse_alias_rejects_dotted_names_and_empty_values() {
        // Dotted alias names would conflict with hierarchical
        // config-key conventions if we ever add `alias.NAME.field`
        // style. Skip silently rather than erroring.
        let body = "alias.dp.prod = \"deploy\"\nalias.empty = \"\"\nalias. = \"x\"\n";
        let cfg = parse(body);
        assert!(cfg.command_aliases.is_empty());
    }

    #[test]
    fn serialize_round_trips_default_config() {
        let cfg = Config::default();
        let body = serialize(&cfg);
        let reparsed = parse(&body);
        assert_eq!(reparsed.refresh_interval, cfg.refresh_interval);
        assert_eq!(reparsed.theme, cfg.theme);
        assert_eq!(reparsed.icons, cfg.icons);
        assert!(reparsed.extra_regions.is_empty());
        assert!(reparsed.required_tags.is_empty());
    }

    #[test]
    fn documented_alarm_dimensions_example_parses() {
        let cfg = parse("alarm_dimensions = \"Environment\"\n");
        assert_eq!(
            cfg.alarm_dimensions,
            vec!["EnvironmentName".to_string(), "Environment".to_string()],
            "the key ADDS spellings; the canonical one is always matched"
        );
    }

    #[test]
    fn alarm_dimensions_has_a_deliberate_opt_out() {
        // This key exists because matching on the dimension *value*
        // alone claimed an RDS alarm named after the env. Making it
        // add-only fixed the "operator's own spelling is invisible"
        // half and reinstated the other: an operator whose non-EB
        // alarms carry an `EnvironmentName` dimension had no way to
        // stop ebman claiming them.
        let cfg = parse("alarm_dimensions = \"MyDim,-EnvironmentName\"\n");
        assert_eq!(cfg.alarm_dimensions, vec!["MyDim".to_string()]);

        // Order doesn't matter — a removal applies wherever it lands.
        let cfg = parse("alarm_dimensions = \"-EnvironmentName,MyDim\"\n");
        assert_eq!(cfg.alarm_dimensions, vec!["MyDim".to_string()]);

        // Removing everything would match nothing, ever. Nobody means
        // that by this key, and a blind alarm panel reads as "no
        // alarms configured" — the exact misread the canonical
        // default is there to prevent.
        let cfg = parse("alarm_dimensions = \"-EnvironmentName\"\n");
        assert_eq!(
            cfg.alarm_dimensions,
            vec!["EnvironmentName".to_string()],
            "an empty match set falls back rather than blinding the panel"
        );

        // And the opt-out survives a `:settings` save, which rewrites
        // the whole file from `serialize`.
        let cfg = parse("alarm_dimensions = \"MyDim,-EnvironmentName\"\n");
        let reparsed = parse(&serialize(&cfg));
        assert_eq!(
            reparsed.alarm_dimensions, cfg.alarm_dimensions,
            "without re-emitting the `-`, a save reinstates the false positive"
        );
    }

    #[test]
    fn alarm_dimensions_cannot_configure_away_the_canonical_name() {
        // `:alarm-create` always writes `EnvironmentName`. If the config
        // could replace the match set, ebman's own alarms — and every
        // EB-native one — would become invisible to it, which during
        // triage reads as "no alarms configured".
        let cfg = parse("alarm_dimensions = \"Environment,EnvName\"\n");
        assert!(cfg
            .alarm_dimensions
            .contains(&crate::aws::ENV_DIMENSION.to_string()));
        // And listing it explicitly doesn't duplicate it.
        let cfg = parse("alarm_dimensions = \"EnvironmentName,EnvironmentName\"\n");
        assert_eq!(cfg.alarm_dimensions, vec!["EnvironmentName".to_string()]);
    }

    #[test]
    fn alarm_dimensions_defaults_to_the_canonical_name() {
        let cfg = parse("");
        assert_eq!(cfg.alarm_dimensions, vec!["EnvironmentName".to_string()]);
    }

    #[test]
    fn an_empty_alarm_dimensions_keeps_the_default() {
        // Blanking the key must not silently match nothing.
        let cfg = parse("alarm_dimensions = \"\"\n");
        assert_eq!(cfg.alarm_dimensions, vec!["EnvironmentName".to_string()]);
    }

    #[test]
    fn settings_save_must_not_drop_hand_written_config() {
        // `:settings` save rewrites the WHOLE file from `serialize`, so any
        // key `parse` accepts but `serialize` omits is destroyed.
        let original = concat!(
            "required_tags = \"Owner,Project\"\n",
            "alarm_dimensions = \"EnvironmentName,Environment\"\n",
            "lint.disable = \"EBL001,EBL007\"\n",
            "lint.fix_disable = \"EBL003\"\n",
            "explain.provider = \"ollama\"\n",
            "explain.enabled = true\n",
            "explain.ollama_url = \"http://localhost:11434\"\n",
            "explain.api_key_env = \"MY_KEY\"\n",
            "explain.max_tokens = 2048\n",
            "accounts.prod.role_arn = \"arn:aws:iam::123456789012:role/EbAdmin\"\n",
            "accounts.prod.source_profile = \"default\"\n",
            "accounts.prod.external_id = \"xyz\"\n",
        );
        let before = parse(original);
        let after = parse(&serialize(&before));

        let mut lost: Vec<&str> = Vec::new();
        if after.required_tags != before.required_tags {
            lost.push("required_tags");
        }
        if after.alarm_dimensions != before.alarm_dimensions {
            lost.push("alarm_dimensions");
        }
        if after.lint_disable != before.lint_disable {
            lost.push("lint.disable");
        }
        if after.lint_fix_disable != before.lint_fix_disable {
            lost.push("lint.fix_disable");
        }
        if after.explain_provider != before.explain_provider {
            lost.push("explain.provider");
        }
        if after.explain_enabled != before.explain_enabled {
            lost.push("explain.enabled");
        }
        if after.explain_ollama_url != before.explain_ollama_url {
            lost.push("explain.ollama_url");
        }
        if after.explain_api_key_env != before.explain_api_key_env {
            lost.push("explain.api_key_env");
        }
        if after.explain_max_tokens != before.explain_max_tokens {
            lost.push("explain.max_tokens");
        }
        if after.accounts != before.accounts {
            lost.push("accounts.*");
        }

        assert!(lost.is_empty(), "a :settings save destroys: {lost:?}");
    }
    #[test]
    fn a_typod_account_field_survives_a_settings_save_without_creating_an_account() {
        // Three things have to hold at once, and earlier attempts got
        // one at the cost of another:
        //   - the typo is PRESERVED, because it's the line the operator
        //     would otherwise spot and fix;
        //   - no phantom account exists, because `contains_key` gates
        //     `:account NAME`, and a spec with an empty ARN failed with
        //     an opaque STS error instead of "no such account";
        //   - the account's valid sibling lines survive, because
        //     skipping the whole block to avoid writing `role_arn = ""`
        //     traded one bad line for three.
        let cfg = parse(concat!(
            "accounts.prod.rolearn = \"arn:aws:iam::123456789012:role/EbAdmin\"\n",
            "accounts.prod.source_profile = \"default\"\n",
            "accounts.prod.region = \"eu-west-1\"\n",
            "accounts.real.role_arn = \"arn:aws:iam::999:role/Ok\"\n",
        ));
        assert!(
            cfg.accounts
                .get("prod")
                .map(|a| a.role_arn.trim().is_empty())
                .unwrap_or(true),
            "a mistyped ARN key must not produce a usable account"
        );

        let out = serialize(&cfg);
        assert!(
            out.contains("accounts.prod.rolearn = \"arn:aws:iam::123456789012:role/EbAdmin\""),
            "the mistyped line must survive the save:\n{out}"
        );
        assert!(
            !out.contains("accounts.prod.role_arn"),
            "an empty ARN must not be written back:\n{out}"
        );
        assert!(
            out.contains("accounts.prod.source_profile = \"default\"")
                && out.contains("accounts.prod.region = \"eu-west-1\""),
            "the account's valid lines must survive:\n{out}"
        );
        assert!(out.contains("accounts.real.role_arn = \"arn:aws:iam::999:role/Ok\""));

        // And the whole thing round-trips.
        let back = parse(&out);
        assert_eq!(back.accounts.get("real"), cfg.accounts.get("real"));
    }

    #[test]
    fn account_values_containing_quotes_round_trip() {
        // Was #[ignore]d as a pinned limit while `parse` stripped every
        // surrounding quote; one pair now, so the quotes inside survive.
        // `:settings` rewrites the WHOLE file, so a value that breaks
        // TOML quoting doesn't just corrupt its own line.
        let mut cfg = Config::default();
        cfg.accounts.insert(
            "odd".into(),
            AccountSpec {
                role_arn: "arn:aws:iam::1:role/He said \"hi\"".into(),
                source_profile: Some("back\\slash".into()),
                external_id: None,
                region: None,
            },
        );
        let out = serialize(&cfg);
        // The round trip is the real assertion: it must survive.
        let back = parse(&out);
        assert_eq!(
            back.accounts.get("odd").map(|a| a.role_arn.as_str()),
            Some("arn:aws:iam::1:role/He said \"hi\""),
            "serialized:\n{out}"
        );
    }
    #[test]
    fn a_second_alarm_dimensions_line_replaces_the_first() {
        // Every other key is last-write-wins. Appending meant an
        // operator editing by adding a line silently got the union of
        // both, with no way to narrow it back.
        let cfg = parse(concat!(
            "alarm_dimensions = \"A\"\n",
            "alarm_dimensions = \"B\"\n",
        ));
        assert_eq!(
            cfg.alarm_dimensions,
            vec!["EnvironmentName".to_string(), "B".to_string()],
            "the later line must replace the earlier one"
        );
    }

    #[test]
    fn a_hand_written_alarm_dimensions_line_round_trips_unchanged() {
        // `:settings` rewrites the whole file. Emitting the full match
        // set turned the operator's `"Environment"` into
        // `"EnvironmentName,Environment"` on the first save.
        let original = "alarm_dimensions = \"Environment\"\n";
        let out = serialize(&parse(original));
        assert!(
            out.contains("alarm_dimensions = \"Environment\"\n"),
            "expected the operator's own value back:\n{out}"
        );
        // And the parsed meaning is unchanged across the round trip.
        assert_eq!(
            parse(&out).alarm_dimensions,
            parse(original).alarm_dimensions
        );
    }

    #[test]
    fn an_unset_alarm_dimensions_is_not_written() {
        let out = serialize(&Config::default());
        assert!(!out.contains("alarm_dimensions"), "{out}");
    }
    #[test]
    fn an_unrecognised_key_survives_a_settings_save() {
        // `:settings` rewrites the whole file, so anything the model
        // doesn't carry was destroyed — including a key a newer release
        // understands and this build doesn't, which is the opposite of
        // the graceful degradation the parser aims for.
        let cfg = parse("some_future_key = \"value\"\nrefresh_interval_secs = 30\n");
        let out = serialize(&cfg);
        assert!(
            out.contains("some_future_key = \"value\""),
            "an unknown key must survive:\n{out}"
        );
        assert!(out.contains("refresh_interval_secs = 30"));
        // And a second save doesn't duplicate it.
        let twice = serialize(&parse(&out));
        assert_eq!(twice.matches("some_future_key").count(), 1, "{twice}");
    }

    /// Each of these was a silent `continue` before, and each left the
    /// environment WRITEABLE while the operator believed it pinned.
    /// That is a safety control failing open, which is the only
    /// direction that actually costs anything.
    #[test]
    fn a_malformed_safety_line_is_reported_rather_than_skipped() {
        let cases = [
            // Field omitted entirely.
            ("prod", "true"),
            // Field misspelled — or understood by a NEWER ebman than
            // this one, which is refused for the same reason: a control
            // this binary cannot enforce must not read as absent.
            ("prod.readonly", "true"),
            ("prod.read_only_ish", "true"),
            // Value is not a boolean.
            ("prod.read_only", "ture"),
            ("prod.read_only", ""),
            // No name to attach the pin to.
            (".read_only", "true"),
        ];
        for (rest, value) in cases {
            match parse_safety_pin("safety.envs", rest, value) {
                SafetyPin::Unreadable { problem } => {
                    assert!(
                        problem.contains("safety.envs"),
                        "the message must name the line to fix: {problem}"
                    );
                }
                other => panic!("safety.envs.{rest} = {value:?} must not parse: {other:?}"),
            }
        }
    }

    /// The other direction: well-formed lines must still parse, in every
    /// boolean spelling. Without this, "refuse everything unparseable"
    /// could be satisfied by refusing everything.
    #[test]
    fn well_formed_safety_lines_still_parse_cleanly() {
        for (value, expected) in [
            ("true", true),
            ("YES", true),
            ("on", true),
            ("1", true),
            ("false", false),
            ("no", false),
            ("off", false),
            ("0", false),
        ] {
            assert_eq!(
                parse_safety_pin("safety.envs", "prod.read_only", value),
                SafetyPin::Valid {
                    name: "prod".into(),
                    read_only: expected
                },
                "{value} should parse as {expected}"
            );
        }

        let cfg = parse("safety.envs.poly-prod.read_only = true\n");
        assert!(
            cfg.safety_parse_errors.is_empty(),
            "a clean config must not refuse writes: {:?}",
            cfg.safety_parse_errors
        );
        assert_eq!(cfg.safety_envs.get("poly-prod"), Some(&true));
    }

    /// Parse errors are diagnostics about the operator's file, not
    /// settings. Writing them back would be nonsense, and a round-trip
    /// through `:settings` must not invent config keys.
    #[test]
    fn parse_errors_are_not_serialised_back_out() {
        let cfg = parse("safety.envs.prod = true\n");
        assert_eq!(cfg.safety_parse_errors.len(), 1, "the line is malformed");
        let rendered = serialize(&cfg);
        assert!(
            !rendered.contains("safety_parse_errors") && !rendered.contains("is missing a field"),
            "diagnostics leaked into the saved config: {rendered}"
        );
    }

    /// A malformed safety line must survive a save.
    ///
    /// The sequence this prevents: a broken pin refuses every write →
    /// the operator opens `:settings` to find out why → saving rewrites
    /// config.toml from the parsed config → the malformed line is gone
    /// → the refusal lifts and the pin they meant to set has vanished.
    /// They would reasonably conclude it was fixed. It was deleted, and
    /// the environment is writeable again — the original fail-open,
    /// reached by a new route.
    #[test]
    fn a_malformed_safety_line_survives_a_save() {
        for raw in [
            "safety.envs.prod = true",
            "safety.envs.prod.readonly = true",
            "safety.accounts.prod.read_only = ture",
        ] {
            let cfg = parse(&format!("{raw}\n"));
            assert_eq!(cfg.safety_parse_errors.len(), 1, "{raw} should not parse");
            let out = serialize(&cfg);
            assert!(
                out.contains(raw),
                "saving deleted the operator's line {raw:?}: {out}"
            );
            // And it is still refused after the round-trip, rather than
            // quietly becoming valid.
            assert_eq!(
                parse(&out).safety_parse_errors.len(),
                1,
                "the round-tripped config must still refuse: {out}"
            );
        }
    }

    /// A dotted account name is ordinary — AWS profile names allow them,
    /// and `safety.accounts.NAME` matches the profile name. Splitting
    /// the key from the left made every one of them unreadable, which
    /// under fail-closed means refusing every write for a config that is
    /// perfectly reasonable.
    #[test]
    fn a_dotted_account_name_is_a_valid_pin() {
        let cfg = parse("safety.accounts.company.prod.read_only = true\n");
        assert!(
            cfg.safety_parse_errors.is_empty(),
            "a dotted profile name must not refuse the fleet: {:?}",
            cfg.safety_parse_errors
        );
        assert_eq!(cfg.safety_accounts.get("company.prod"), Some(&true));

        // The field is still the LAST segment, so a typo after a dotted
        // name is still caught rather than swallowed into the name.
        let bad = parse("safety.accounts.company.prod.readonly = true\n");
        assert_eq!(bad.safety_parse_errors.len(), 1, "typo must still refuse");
    }

    /// Profile names routinely contain dots, and `accounts.NAME.field`
    /// splits the key to find the field. Splitting from the LEFT read
    /// `accounts.company.prod.role_arn` as name `company`, field
    /// `prod.role_arn` — unrecognised, and this arm ignores unrecognised
    /// fields by design, so the spec was silently never created and
    /// `:account company.prod` reported no such account.
    ///
    /// Same bug as `safety.accounts.*` had; found while fixing that one.
    #[test]
    fn a_dotted_account_name_builds_its_spec() {
        let cfg = parse(
            "accounts.company.prod.role_arn = \"arn:aws:iam::1:role/r\"\n\
             accounts.company.prod.region = \"eu-west-2\"\n",
        );
        let spec = cfg
            .accounts
            .get("company.prod")
            .expect("a dotted account name must build a spec");
        assert_eq!(spec.role_arn, "arn:aws:iam::1:role/r");
        assert_eq!(spec.region.as_deref(), Some("eu-west-2"));
        assert!(
            !cfg.accounts.contains_key("company"),
            "and must not create a phantom under the first segment: {:?}",
            cfg.accounts.keys().collect::<Vec<_>>()
        );
    }

    /// An `accounts.` line the parser cannot use must survive a save,
    /// for the same reason a malformed safety pin must: `:settings`
    /// rewrites the file from the parsed config, so a bare `continue`
    /// deletes the operator's line.
    #[test]
    fn an_unusable_accounts_line_survives_a_save() {
        for raw in [
            "accounts.prod = \"oops\"",
            "accounts..role_arn = \"arn:aws:iam::1:role/r\"",
        ] {
            let cfg = parse(&format!("{raw}\n"));
            assert!(
                !cfg.accounts.contains_key("") && !cfg.accounts.contains_key("prod"),
                "{raw} must not create a phantom account: {:?}",
                cfg.accounts.keys().collect::<Vec<_>>()
            );
            let out = serialize(&cfg);
            assert!(out.contains(raw), "saving deleted {raw:?}: {out}");
        }
    }

    /// A typo OF the family, not just IN it.
    ///
    /// `safety.envs.prod.readonly` refused the whole fleet while
    /// `safety.env.prod.read_only` — the same operator mistake, one
    /// character away — vanished silently and left prod writeable. Two
    /// independent reviewers found this in the release that introduced
    /// the fail-closed rule, because the net was written around the
    /// families it already recognised.
    #[test]
    fn an_unknown_safety_family_is_refused_not_ignored() {
        for raw in [
            "safety.env.prod.read_only = true",          // missing the `s`
            "safety.account.prod.read_only = true",      // ditto
            "safety.envs = true",                        // no name at all
            "safety.regions.eu-west-2.read_only = true", // a newer ebman's family
        ] {
            let cfg = parse(&format!("{raw}\n"));
            assert_eq!(
                cfg.safety_parse_errors.len(),
                1,
                "{raw} must refuse rather than vanish"
            );
            assert!(
                serialize(&cfg).contains(raw),
                "and must survive a save: {raw}"
            );
        }

        // The known families still parse — this must not become "refuse
        // everything under safety.".
        let good =
            parse("safety.envs.prod.read_only = true\nsafety.accounts.acme.read_only = false\n");
        assert!(
            good.safety_parse_errors.is_empty(),
            "well-formed pins must still work: {:?}",
            good.safety_parse_errors
        );
        assert_eq!(good.safety_envs.get("prod"), Some(&true));
        assert_eq!(good.safety_accounts.get("acme"), Some(&false));
    }

    /// An unreadable config file must refuse, not default.
    ///
    /// The file-level twin of the line-level rule: pins may be defined
    /// in a file we cannot read, and `Config::default()` has none — so
    /// returning it turns "cannot read the policy" into "there is no
    /// policy". A permission change or a single non-UTF-8 byte was
    /// enough to leave a pinned prod writeable, silently.
    #[test]
    fn an_unreadable_config_file_refuses_rather_than_defaulting() {
        use std::io::{Error, ErrorKind};
        let path = std::path::Path::new("/tmp/does-not-matter/config.toml");

        // Missing is fine — no file, no pins, nothing to enforce.
        let missing = config_from_read(Err(Error::from(ErrorKind::NotFound)), path);
        assert!(
            missing.safety_parse_errors.is_empty(),
            "a missing config is the ordinary case, not a refusal"
        );

        // Anything else must fail closed.
        for kind in [
            ErrorKind::PermissionDenied,
            ErrorKind::InvalidData, // non-UTF-8 bytes
            ErrorKind::Other,
        ] {
            let cfg = config_from_read(Err(Error::from(kind)), path);
            assert_eq!(
                cfg.safety_parse_errors.len(),
                1,
                "{kind:?} must refuse — pins may exist in a file we cannot read"
            );
            assert!(
                cfg.safety_parse_errors[0].contains("config.toml"),
                "the message must name the file: {:?}",
                cfg.safety_parse_errors
            );
        }

        // And a readable file still parses normally.
        let ok = config_from_read(Ok("safety.envs.prod.read_only = true\n".into()), path);
        assert!(ok.safety_parse_errors.is_empty());
        assert_eq!(ok.safety_envs.get("prod"), Some(&true));
    }

    /// Saving must not destroy a config it could not read.
    ///
    /// The loop this breaks: an unreadable `config.toml` refuses every
    /// write, the refusal sends the operator to `:settings` to find out
    /// why, and the save replaces the file with a serialisation that has
    /// none of the pins — because none could be parsed. The refusal
    /// lifts and the environment is writeable. `write_atomic` renames
    /// over the target using directory permissions, so being unable to
    /// READ the file does not prevent replacing it.
    #[test]
    fn a_save_refuses_to_clobber_an_unreadable_config() {
        use std::io::{Error, ErrorKind};
        let path = std::path::Path::new("/tmp/does-not-matter/config.toml");

        // Readable, and missing, both proceed.
        assert!(refuse_to_clobber_unreadable(Ok(()), path).is_ok());
        assert!(
            refuse_to_clobber_unreadable(Err(Error::from(ErrorKind::NotFound)), path).is_ok(),
            "first run has nothing to destroy"
        );

        for kind in [
            ErrorKind::PermissionDenied,
            ErrorKind::InvalidData,
            ErrorKind::Other,
        ] {
            let err = refuse_to_clobber_unreadable(Err(Error::from(kind)), path)
                .expect_err("{kind:?} must refuse the save");
            let msg = err.to_string();
            assert!(
                msg.contains("refusing to overwrite"),
                "the operator must be told the save did not happen: {msg}"
            );
            assert!(msg.contains("config.toml"), "and which file: {msg}");
        }
    }

    /// The guard must be WIRED, not merely present: `save` refuses to
    /// replace a file it cannot read, and writes the `app.` lines the
    /// file has now rather than the startup copy. Driven against a
    /// file of the test's own, now that `save` takes its path.
    #[test]
    fn save_keeps_an_unreadable_file_and_the_files_app_lines() {
        let dir =
            std::env::temp_dir().join(format!("ebman-test-{}-config-save", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let _ = std::fs::remove_dir(&path);

        // The startup copy has no app. lines; the file has one since.
        std::fs::write(&path, "app.e.namespace = \"OnDisk\"\n").unwrap();
        save(&path, &Config::default()).expect("saves");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("app.e.namespace = \"OnDisk\""), "{text}");

        // Unreadable (a directory where the file is): refused, untouched.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let e = save(&path, &Config::default()).expect_err("refused");
        assert!(e.to_string().contains("refusing to"), "{e}");
        assert!(path.is_dir());
        std::fs::remove_dir(&path).unwrap();
    }

    /// Each rule in `parse_alarm_dimensions` cost something to learn, so
    /// each gets a case.
    #[test]
    fn alarm_dimensions_rules_each_hold() {
        let canonical = crate::aws::ENV_DIMENSION.to_string();

        // Additional, not a replacement — dropping the canonical name
        // would hide the alarms ebman itself creates.
        let names = parse_alarm_dimensions("MyDim");
        assert!(names.contains(&canonical), "{names:?}");
        assert!(names.contains(&"MyDim".to_string()), "{names:?}");

        // `-` removes: the escape hatch for RDS alarms that carry an
        // EnvironmentName dimension.
        let names = parse_alarm_dimensions("MyDim,-EnvironmentName");
        assert!(
            !names.contains(&canonical),
            "the opt-out must work: {names:?}"
        );
        assert_eq!(names, vec!["MyDim".to_string()]);

        // Removing everything falls back rather than blinding the panel:
        // an empty list matches nothing, which reads as "no alarms".
        assert_eq!(
            parse_alarm_dimensions("-EnvironmentName"),
            vec![canonical.clone()]
        );

        // No duplicates, and the canonical name is not doubled when
        // named explicitly.
        let names = parse_alarm_dimensions("EnvironmentName,MyDim,MyDim");
        assert_eq!(
            names.iter().filter(|n| **n == canonical).count(),
            1,
            "{names:?}"
        );
        assert_eq!(
            names.iter().filter(|n| *n == "MyDim").count(),
            1,
            "{names:?}"
        );

        // Empty value is the default.
        assert_eq!(parse_alarm_dimensions(""), vec![canonical]);
    }

    /// The extraction must be WIRED — the arm has to call the helper,
    /// not keep an inline copy that drifts while the tests pin the
    /// helper.
    #[test]
    fn the_alarm_dimensions_key_goes_through_the_helper() {
        let cfg = parse("alarm_dimensions = \"MyDim,-EnvironmentName\"\n");
        assert_eq!(
            cfg.alarm_dimensions,
            vec!["MyDim".to_string()],
            "the parse arm must apply the same rules as the helper"
        );
    }

    /// `mcp.peek_bodies` survives a `:settings` round-trip.
    ///
    /// The switch is worthless if opening a form turns it back on.
    /// `:settings` rebuilds a whole `Config` from App state and saves
    /// that, so any key the App does not carry is silently reset to
    /// its default — and this key's default is `true`. It is mirrored
    /// into `ResolvedConfig` for exactly this reason and no other; the
    /// TUI never otherwise reads it.
    #[test]
    fn peek_bodies_survives_a_settings_save() {
        let cfg = parse("mcp.peek_bodies = false\n");
        assert!(!cfg.mcp_peek_bodies, "parsed");

        let round_tripped = parse(&serialize(&cfg));
        assert!(
            !round_tripped.mcp_peek_bodies,
            "a save must not re-enable message bodies"
        );

        // The default is not written out — it is operator-only, not a
        // key `:settings` manages, and emitting `true` into every
        // saved config would imply otherwise.
        let on = crate::config::Config::default();
        assert!(on.mcp_peek_bodies);
        assert!(
            !serialize(&on).contains("peek_bodies"),
            "the default must not be serialised"
        );
        assert!(
            parse(&serialize(&on)).mcp_peek_bodies,
            "and absence must still read as on"
        );
    }

    /// An unparseable value leaves the stricter setting in place.
    #[test]
    fn a_malformed_peek_bodies_value_does_not_flip_it_on() {
        let mut cfg = parse("mcp.peek_bodies = false\nmcp.peek_bodies = yes-please\n");
        assert!(
            !cfg.mcp_peek_bodies,
            "a value that does not parse must not widen access"
        );
        cfg = parse("mcp.peek_bodies = FALSE\n");
        assert!(!cfg.mcp_peek_bodies, "case-insensitive booleans parse");
    }

    /// `safety.read_only` forbids writes, and fails closed when it
    /// cannot be read.
    ///
    /// The first config key that is purely a restriction. There is
    /// deliberately no counterpart that grants — a standing refusal is
    /// decided calmly and only narrows, while a standing permission
    /// has to predict what you will need.
    #[test]
    fn a_standing_read_only_forbids_writes_and_fails_closed() {
        assert!(
            !parse("").safety_read_only,
            "absent means not forbidden — this key only ever adds a refusal"
        );
        assert!(parse("safety.read_only = true\n").safety_read_only);
        assert!(!parse("safety.read_only = false\n").safety_read_only);

        // A value that cannot be read must not quietly mean `false`.
        // `safety.*` parse errors refuse every write until fixed, so
        // this fails closed rather than lifting the restriction.
        let bad = parse("safety.read_only = ture\n");
        assert!(
            !bad.safety_parse_errors.is_empty(),
            "an unreadable safety value must be recorded, not ignored"
        );
        let msg = &bad.safety_parse_errors[0];
        assert!(
            msg.contains("safety.read_only"),
            "must name the line: {msg}"
        );
        // The RENDERED message, not the literal. This very line shipped
        // with a 26-space hole: a Python heredoc ate the `\`
        // continuation and produced the collapsed form, which the
        // wrapped-literal guard cannot see (it only catches the
        // multi-line shape). The error bar is one line, so the hole
        // pushes the rest off a narrow terminal.
        assert!(
            !msg.contains("  "),
            "a wrapped literal left a gap in the rendered message: {msg:?}"
        );

        // Survives a :settings save.
        let on = parse("safety.read_only = true\n");
        assert!(
            parse(&serialize(&on)).safety_read_only,
            "a save must not lift a standing write refusal"
        );
        assert!(
            !serialize(&parse("")).contains("safety.read_only"),
            "and the default must not be written out"
        );
    }
}

/// Properties of the `app.` lines' parser and writers, over generated
/// files. About ten fixes this cycle were the parser and a writer
/// disagreeing (quoting, escapes, inline comments, CRLF, spaced keys),
/// each found one example at a time.
#[cfg(test)]
mod app_line_properties {
    use super::*;
    use proptest::prelude::*;

    /// A value, often hostile: spaces, `#`, `"` and `\` among the
    /// ordinary characters the fields take.
    fn value() -> impl Strategy<Value = String> {
        prop_oneof![
            "[A-Za-z0-9_./:*-]{1,12}",
            "[A-Za-z0-9_-]{1,6}=[A-Za-z0-9_-]{1,6}(,[A-Za-z0-9_-]{1,6}=[A-Za-z0-9_-]{1,6}){0,2}",
            "[a-z]{1,8}\\|(Sum|Average|p99|Maximum)(\\|count)?",
            "<?[0-9]{1,4}(\\.[0-9])?,[0-9]{1,4}",
            "[A-Za-z0-9 #\"\\\\=|,.-]{0,16}",
        ]
    }

    fn field() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("namespace".to_string()),
            Just("dimensions".to_string()),
            Just("instance_dimension".to_string()),
            "metric\\.[a-z]{1,4}",
            "threshold\\.[a-z]{1,4}",
            Just("rds".to_string()),
            Just("elasticache".to_string()),
            Just("alarm_prefix".to_string()),
            Just("log_group".to_string()),
            Just("error_fingerprint".to_string()),
            Just("bogus".to_string()),
        ]
    }

    /// One `app.` line for `env`, in the shapes people write.
    fn app_line(env: &'static str) -> impl Strategy<Value = String> {
        (field(), value(), 0..8usize).prop_map(move |(f, v, shape)| match shape {
            0 => format!("app.{env}.{f} = \"{v}\""),
            1 => format!("app.{env}.{f}=\"{v}\""),
            2 => format!("  app. {env}.{f} = \"{v}\"  "),
            // Inline comments, the shape two fixes this cycle were about.
            3 => format!("app.{env}.{f} = \"{v}\" # a note"),
            4 => format!("app.{env}.{f}=\"{v}\"#note"),
            5 => format!("app.{env}.{f}\t=\t\"{v}\""),
            6 => format!("App.{env}.{f} = \"{v}\""),
            _ => format!("app.{env}.{f} = {v}"),
        })
    }

    /// An `app.` line as the App tab's editor writes one: what a
    /// replacement carries. The hostile shapes are the file around it.
    fn written_line(env: &'static str) -> impl Strategy<Value = String> {
        (field(), value()).prop_map(move |(f, v)| format!("app.{env}.{f} = \"{v}\""))
    }

    /// A line of a config file: other envs' `app.` lines, comments,
    /// blank lines and other keys around env `e`'s.
    fn file_line() -> impl Strategy<Value = String> {
        prop_oneof![
            app_line("e"),
            app_line("f"),
            app_line("e-wk"),
            "# [a-z #\"]{0,20}",
            Just(String::new()),
            Just("refresh_interval_secs = 15".to_string()),
            Just("theme = \"dark\"".to_string()),
        ]
    }

    fn file() -> impl Strategy<Value = (Vec<String>, bool)> {
        (proptest::collection::vec(file_line(), 0..16), any::<bool>())
    }

    fn join(lines: &[String], crlf: bool) -> String {
        let eol = if crlf { "\r\n" } else { "\n" };
        lines.iter().map(|l| format!("{l}{eol}")).collect()
    }

    fn is_envs(line: &str, env: &str) -> bool {
        crate::app_tab::key_of(line).is_some_and(|(e, _)| e == env)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(400))]

        /// Replacing env `e`'s lines gives `e` exactly what the new lines
        /// say, keeps every other line byte for byte and in order, leaves
        /// every other env's config as it was, and keeps the file's own
        /// line ending.
        #[test]
        fn replacing_an_envs_lines_touches_nothing_else(
            (lines, crlf) in file(),
            new in proptest::collection::vec(written_line("e"), 0..6),
        ) {
            let text = join(&lines, crlf);
            let out = replace_app_lines(&text, "e", &new);

            let new_text = join(&new, false);
            let (got, want) = (parse(&out).app_tabs, parse(&new_text).app_tabs);
            prop_assert_eq!(
                got.envs.get("e"),
                want.envs.get("e"),
                "e is what the new lines say:\n{}", out
            );
            let others = |t: &str| -> Vec<String> {
                t.lines().filter(|l| !is_envs(l, "e")).map(str::to_string).collect()
            };
            prop_assert_eq!(others(&out), others(&text), "the rest, verbatim, in order");
            let (before, after) = (parse(&text).app_tabs, parse(&out).app_tabs);
            for env in ["f", "e-wk"] {
                prop_assert_eq!(before.envs.get(env), after.envs.get(env), "{} untouched", env);
            }
            if crlf && !text.is_empty() {
                prop_assert_eq!(out.matches('\n').count(), out.matches("\r\n").count(), "still CRLF");
            }
        }

        /// A `:settings` save writes the `app.` lines back as read: what
        /// every env's config means, and the lines that did not apply.
        #[test]
        fn a_settings_save_keeps_every_envs_app_config((lines, crlf) in file()) {
            let text = join(&lines, crlf);
            let read = parse(&text);
            let saved = parse(&serialize(&read));
            prop_assert_eq!(&saved.app_tabs.envs, &read.app_tabs.envs);
            let rejected = |c: &Config| -> Vec<Option<String>> {
                c.app_tabs.errors.iter().map(|e| e.line.clone()).collect()
            };
            prop_assert_eq!(rejected(&saved), rejected(&read));
        }

        /// What the App tab's editor writes for a parsed spec reads back
        /// as that spec, or the editor refuses it: it never writes a line
        /// that means something else.
        #[test]
        fn the_editor_writes_back_exactly_what_it_was_given(
            lines in proptest::collection::vec(app_line("e"), 1..8),
        ) {
            let mut tabs = parse(&join(&lines, false)).app_tabs;
            let clean = tabs.errors.is_empty();
            let Some(spec) = tabs.envs.remove("e") else {
                return Ok(());
            };
            let fields = crate::app::app_tab_fields_for_tests(Some(&spec));
            match crate::app::app_tab_lines_for_tests("e", &fields) {
                Ok(written) => {
                    let back = parse(&join(&written, false)).app_tabs;
                    prop_assert_eq!(back.envs.get("e"), Some(&spec), "{:?}", written);
                }
                // Refusing is right only for a config the parser itself
                // faulted (a metric with no namespace, say).
                Err(e) => prop_assert!(!clean, "a clean config refused: {:?} for {:?}", e, lines),
            }
        }
    }
}

#[cfg(test)]
mod parser_properties {
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1500))]

        /// `config.toml` is hand-edited and every surface parses it on
        /// startup, so a line the parser chokes on would stop ebman
        /// opening at all. It must degrade to errors, never panic.
        #[test]
        fn config_parse_never_panics(s in "(?s).{0,400}") {
            let _ = super::parse(&s);
        }

        /// Near-valid lines under the prefixes with their own scanners:
        /// `safety.` (which fails closed), `app.` (the App tab) and
        /// `accounts.`.
        #[test]
        fn config_parse_survives_near_miss_lines(
            prefix in "(safety\\.envs|safety\\.accounts|app|accounts|lint|alarm)",
            name in "[a-zA-Z0-9_.-]{0,16}",
            field in "[a-z_]{0,16}",
            value in "[a-zA-Z0-9 \"=;|,._/-]{0,60}",
        ) {
            let _ = super::parse(&format!("{prefix}.{name}.{field} = {value}"));
            let _ = super::parse(&format!("{prefix}.{name}.{field} = \"{value}\""));
            let _ = super::parse(&format!("{prefix}.{name} = {value}"));
        }
    }
}
