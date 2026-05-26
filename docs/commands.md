# Command reference

Type `:` to open the command bar. Tab-completion is not implemented, but `Ctrl-K` fuzzy-searches every command + env + view + plugin.

## Navigation / inspection

- `:region NAME` / `:region all` — switch region, or fan out across every configured region.
- `:profile NAME` — switch AWS profile.
- `:account NAME` — switch to a configured AssumeRole account (`accounts.NAME` in `config.toml`). Falls back to `:profile NAME` aliasing when no `accounts.` entry exists.
- `:accounts` — list child accounts in the active AWS organization; rows matching a configured `accounts.NAME` get a `:account NAME` switch hint.
- `:sort KEY [desc]` — set sort (name/app/status/health/version/age).
- `:group on|off` — toggle group-by-application.
- `:redact on|off` — toggle redact mode.
- `:events on|off` — toggle events panel.
- `:filter NAME` / `:f NAME` — load a saved filter.
- `:save NAME` / `:drop NAME` / `:filters` — manage named filters.
- `:save-view NAME` / `:view NAME` / `:views` / `:view-drop NAME` — saved views (filter + sort + grouping + scope).
- `:cols list|hide NAME|show NAME|reset` — manage columns.
- `:pin` — pin / unpin selected env.
- `:alias NAME LABEL` / `:alias-drop NAME` — local env aliases.
- `:loglevel LEVEL` — live-reload the tracing filter.

## Per-env inspection

- `:why` — Red-env diagnostic overlay (recent events / alarms / instance health / recent deploys; main + DLQ peek for Worker envs). Bound to `!` on the env table.
- `:diff NAME` — side-by-side env comparison against the selected env. `:diff ENV-A ENV-B` names both explicitly (post-0.8) so no selected-env fallback is needed.
- `:config-diff ENV` — option-setting deltas between the selected env and `ENV`. `:config-diff-local [NAME]` diffs against a local EB CLI saved config under `.elasticbeanstalk/saved_configs/`.
- `:lineage` — deploy-only timeline for the selected env (one row per version label, newest first, with Δ between deploys and `took` span).
- `:alarm-history NAME` — recent CloudWatch alarm transitions (StateUpdate / ConfigurationUpdate / Action entries, newest first).
- `:ssh [i-abc]` — open an embedded SSM Session Manager session into an env instance. No arg opens a picker over cached Detail/Instances.
- `:ssm-run "<cmd>"` — fan a shell command across the env's instances via SSM Run Command; per-instance status / exit / stdout / stderr in one overlay.
- `:resources` / `:res` — `DescribeEnvironmentResources` dump.
- `:alarms` — CloudWatch alarms referencing the env.
- `:versions` — application versions (deployed marker, total count, deploy hint).
- `:saved-configs` / `:configs` — saved configuration templates (interactive: `a` apply, `i` inspect, `x` delete, `c` create).
- `:custom-platforms` / `:platforms` — custom EB platforms.
- `:plugins` — list user plugin commands.
- `:history` — recent status / error log.
- `:pending` / `:in-flight` — overlay of dispatched actions + outcomes.
- `:whatsnew` — embedded changelog.
- `:about` / `:credits` — version, license, attributions.
- `:update` — show (and yank to clipboard) the upgrade command for whichever install channel (Homebrew / cargo-bin / tarball) ebman was installed from.
- `:settings` — interactive form to edit `~/.config/ebman/config.toml`; writes back on submit and live-applies theme / icons / refresh interval.

## Write — env state

- `:rebuild` / `:restart` / `:terminate` — action menu shortcuts (Terminate requires typed-name confirm).
- `:deploy LABEL` — ship an existing application version to the selected env. Every dispatch captures a pre-deploy snapshot (version label + timestamp) into `state.toml` so `:rollback` has a reliable target.
- `:deploy LABEL --preview` — open a side-by-side overlay of the currently-deployed version vs the candidate (label, description, S3 source, timestamp + rollback / traffic warnings) without dispatching.
- `:deploy LABEL --auto-rollback Nm` — arms a watchdog after dispatch. If the env reaches Green before the deadline (`5m` / `30m` / `1h` grammar), the watchdog disarms with a status toast. If not, ebman automatically redeploys the captured snapshot's previous version, with an audit-log entry. Respects per-env / per-account read-only safety pins.
- `:deploy --from PATH [--label L] [--describe D] [--no-deploy]` — upload a local `.zip` (or `--from s3://bucket/key`), register a new version, optionally deploy.
- `:upgrade [ARN]` — list compatible platforms; with ARN, dispatch the migration.
- `:clone NEWNAME` — clone the selected env.
- `:scale N` / `:stop` / `:start` — set ASG min=max=N / 0 / 1.
- `:capacity` — modal form to edit Min / Max / Instance type / Cooldown in one shot (pre-filled from `DescribeConfigurationSettings`).
- `:swap TARGET` — swap CNAMEs (Y/N confirm).
- `:abort` — abort an in-flight env update.

## Write — env config

- `:env list | set KEY VAL | unset KEY` — application env-var editor.
- `:tag KEY VALUE` / `:untag KEY` — env tag editor.
- `:set-option NAMESPACE OPTION VALUE` / `:unset-option NAMESPACE OPTION` — generic option-settings escape hatch.
- `:instance-type TYPE` — EC2 instance type (e.g. `t3.medium`).
- `:keypair NAME` / `:service-role ARN` / `:instance-profile NAME` — security tab.
- `:public-ip on|off` / `:elb-scheme public|internal` — network tab.
- `:subnets` / `:elb-subnets` / `:security-groups` — MultiSelect picker forms pre-filled with the env's current selection (lists available subnets / SGs from the VPC).
- `:deployment-policy AllAtOnce|Rolling|RollingWithAdditionalBatch|Immutable|TrafficSplitting` — deploy policy.
- `:rolling-update on|off` — ASG rolling-update policy.
- `:health-check-url /path` — HTTP health-check path.
- `:logs-stream on|off [--retention DAYS]` — toggle CW Logs streaming.
- `:logs-tail [LOG_GROUP]` — open a live streaming overlay for a CW Logs group (auto-picks `web.stdout.log`).
- `:notify EMAIL_OR_SNS_ARN | off` — notification endpoint.
- `:managed-window DAY HOUR | off` — managed-platform-updates window.

## Write — application versions / configs / alarms / platforms

- `:delete-version LABEL [--force]` — delete an application version (with optional source-bundle removal).
- `:config-save NAME` / `:config-apply NAME` / `:config-delete APP NAME` / `:config-inspect NAME` — saved-configuration templates.
- `:alarm-create NAME KIND THRESHOLD [OP]` — CloudWatch alarm (KIND: `health`, `4xx`, `5xx`, `latency`).
- `:alarm-delete NAME` — remove a CW alarm.
- `:custom-platform-delete ARN` — delete a custom EB platform.
- `:metric add LABEL NAMESPACE NAME [STAT] [DIM=VAL,...]` / `:metric remove LABEL` / `:metric list` — custom Metrics-tab charts.

## Multi-account / multi-region

- `:region all` — fan across `extra_regions` + current.
- `:account NAME` — switch to a configured AssumeRole account (see [configuration](configuration.md)).
- `:accounts` — list AWS-Organizations child accounts; switch hints rendered for those with a configured `accounts.NAME`.
- `:find-env SUBSTRING` — scan every profile in `~/.aws/{config,credentials}` **and** every configured AssumeRole account in REGION.
- `:org-health` — aggregate env / red counts per profile + per configured AssumeRole account.

## Multi-env

- `space` — toggle multi-select.
- `:batch-rebuild` / `:batch-restart` — dispatch a non-destructive action across the selection.
- `:batch-deploy LABEL` — deploy the same version to every selected env in parallel.
- `:batch-tag KEY VALUE` / `:batch-untag KEY` — fan a tag write across the selection.
- `:batch-set-option NAMESPACE OPTION VALUE` — fan an option-settings write across the selection.
- `:deselect` / `:select-clear` — clear selection.

## Output / yank

- `:export` — yank filtered view as TSV.
- `:json` — yank filtered view as JSON array.
- `:report` / `:markdown` — yank filtered view as a Markdown table.

## Read-only mode

- `:readonly on|off` — toggle. `--read-only` on the CLI also locks every write surface.
