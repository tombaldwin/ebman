# Configuration

ebman reads from a few files under `~/.config/ebman/` and `<repo>/.ebman/`. None of them carry credentials — those still come from `~/.aws/credentials` via the standard AWS SDK chain.

## `~/.config/ebman/config.toml`

```toml
# Refresh interval in seconds (default 15).
refresh_interval_secs = 15

# Extra regions to expose in the region picker, comma-separated.
extra_regions = ""

# Theme: "dark" (default), "light", or "high-contrast".
theme = "dark"

# Glyph set: "unicode" (default), "ascii" for low-feature terminals,
# "powerline" (alias "nerd") for Powerline-patched / Nerd Fonts, or
# "auto" to probe the terminal at startup and pick powerline if its
# support is detected (one-cell U+E0B0 advance), unicode otherwise.
icons = "unicode"

# Per-profile theme override — pin a theme per AWS profile so the screen
# itself says "you're in prod" without reading the breadcrumb. Format:
# "PROFILE:THEME,PROFILE:THEME". Theme names match the `theme = ...` key.
profile_themes = "prod:high-contrast,staging:dark"

# Start with these toggles on (state.toml takes precedence after first run).
redact_default = false
grouped_default = false

# Notification bell on increase in Red-env count.
notify_bell = false

# Tag policy — flag envs missing any of these tags in the Config tab.
required_tags = "Owner,Project"

# ADDITIONAL CloudWatch dimension names that identify an environment,
# for matching alarms to it.
#
# `EnvironmentName` is matched by default — that's what Elastic Beanstalk
# itself and `:alarm-create` write. This key adds spellings on top, for
# operators whose own alarms use a different dimension name.
#
# Alarms match on the dimension's NAME and VALUE together — an alarm is
# yours if it carries `EnvironmentName=<your-env>`. Matching on the
# value alone used to attribute an unrelated RDS alarm with
# `DBInstanceIdentifier=payments` to an environment called `payments`.
#
# A name prefixed with `-` is REMOVED from the match set. That's the
# escape hatch for the opposite false positive: if your non-EB alarms
# carry an `EnvironmentName` dimension of their own, use
# `alarm_dimensions = "MyDim,-EnvironmentName"` to stop ebman claiming
# them. Removing every name would match nothing at all, so an
# all-removals list falls back to `EnvironmentName`.
# alarm_dimensions = "Environment"
# alarm_dimensions = "MyDim,-EnvironmentName"

# Red-transition notifications — ebman emits a `tracing::warn!` and writes
# a `stage=event kind=red_transition env=…` line to the audit log at
# `~/.cache/ebman/audit.log` for every env that transitions into Red.
# Wire your own notifier (Slack, PagerDuty, …) by tailing that file — or
# set `notify_webhook` and ebman POSTs every audit line (dispatches,
# outcomes, red transitions) to the URL as a Slack-compatible JSON body.
# Note the scope: EVERY audit line goes to the URL, including `cmd="…"`
# strings from `:ssm-run` — point it only at a channel you'd paste those
# into.
# notify_webhook = "https://hooks.slack.com/services/T000/B000/XXXX"

# AssumeRole targets reachable via `:account NAME`. One stanza per
# account. `source_profile` carries the base creds for the
# sts:AssumeRole call. `external_id` and `region` are optional.
# The temporary credentials build a fresh SdkConfig carrying only the
# assumed-role identity — source-profile creds never leak into request
# signing once the switch lands.
accounts.prod.role_arn = "arn:aws:iam::111122223333:role/EbmanReadOnly"
accounts.prod.source_profile = "default"
accounts.prod.region = "eu-west-2"
# accounts.prod.external_id = "..."

# Per-env / per-account read-only locks. Borrowed from pgman's safety
# system. When pinned here, destructive actions against the env (or
# anything under the named account) are refused even when the global
# `--read-only` toggle is off. The global toggle is still the master
# switch; these add granular pins on top.
safety.envs.uflexi-prod.read_only = true
safety.accounts.prod.read_only = true

# Custom command aliases. `alias.NAME = "expansion"` lines map a
# typed `:NAME` to a full command line. Args typed after the alias
# name are appended to the expansion, so `alias.dp = "deploy
# --auto-rollback 5m"` plus `:dp build-900` becomes
# `:deploy --auto-rollback 5m build-900`. Single-level expansion
# (no transitive chaining → no cycle-detection complexity).
# alias.dp = "deploy --auto-rollback 5m"
# alias.shipit = "promote-env staging prod --wait-for-green 5m"

# Lint engine disables — CSV form. Disabled rules are skipped at
# registry-load time so they have zero per-env cost. Project-local
# `.ebman/ebman.toml` can extend (never override) this list via
# `[lint]\ndisable = ["EBL001"]`.
# lint.disable = "EBL003,EBL006"

# Per-rule opt-out for `ebman lint --fix`. Listed rules still
# surface in reports but their auto-remediation is suppressed.
# Useful when an operator has a deliberate non-standard value the
# rule would otherwise overwrite. Project-local form:
# `[lint]\nfix_disable = ["EBL004"]`.
# lint.fix_disable = "EBL004"

# `ebman explain ISSUE_ID` / `:explain EBL###` — LLM-backed
# explainer for lint issues. OFF BY DEFAULT — operators must
# explicitly opt in here AND export the provider API key.
# Presence of ANTHROPIC_API_KEY alone is not implicit consent.
# NOTE: ebman's config parser does not support inline `# comments`
# after a value — keep each value on its own line, comments on theirs.
# explain.enabled = true
# explain.provider = "anthropic"
# explain.model = "claude-haiku-4-5"
# explain.api_key_env = "ANTHROPIC_API_KEY"
# explain.ollama_url = "http://localhost:11434"
# explain.max_tokens = 1024
```

## `~/.config/ebman/commands.toml` (optional)

User plugin commands. Each `:NAME` substitutes `{name}` / `{cname}` / `{application}` / `{tier}` / `{region}` / `{profile}` placeholders and yanks the rendered command to the clipboard.

```toml
[commands.tunnel]
template = "aws ssm start-session --target $(aws ec2 describe-instances --filters Name=tag:elasticbeanstalk:environment-name,Values={name} --query 'Reservations[].Instances[].InstanceId' --output text) --profile {profile}"
description = "Yank a tunnel command into clipboard"
```

## `~/.config/ebman/state.toml`

Managed by the app — filter / sort / cursor position / named filters / saved views / pinned envs / custom metrics live there. You generally don't edit this by hand.

## `<repo>/.ebman/ebman.toml` (optional, project-local)

Project-local pinning. Commit this to git so a team launches ebman from the repo with the right profile / region / filter pre-applied. Walks up from cwd to find the `.ebman/` directory, so running ebman from any subdirectory of the project works. Profile / region win over `~/.config/ebman/state.toml` when both are set. Per-env runbook URLs merge with the user-level `runbooks.ENV = …` map — project entries win on collision.

```toml
# <repo>/.ebman/ebman.toml — commit this. Credentials still come from
# ~/.aws/credentials, never this file.
profile = "prod"          # AWS profile to use
region  = "us-west-1"     # AWS region
application = "uflexi"    # filter envs to this app on launch
filter  = "prod-"         # pre-fill the search filter

[runbooks]
"uflexi-prod" = "https://wiki/runbooks/uflexi-prod"
```
