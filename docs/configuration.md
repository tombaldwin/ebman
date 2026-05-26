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

# Red-transition notifications — ebman emits a `tracing::warn!` and writes
# a `stage=event kind=red_transition env=…` line to the audit log at
# `~/.cache/ebman/audit.log` for every env that transitions into Red.
# Wire your own notifier (Slack, PagerDuty, …) by tailing that file. The
# previous built-in `webhook_url` POST was trimmed — single-URL POST was
# too rigid for real ops workflows, and the audit log already carried the
# same data with timestamps.

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
runbooks.uflexi-prod = "https://wiki/runbooks/uflexi-prod"
```
