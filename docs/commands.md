# Command reference

Type `:` to open the command bar. Tab-completion is not implemented, but `Ctrl-K` fuzzy-searches every command + env + view + plugin. Press `?` in-app for a context-aware help screen built from the same registry.

## View / filter / sort

- `:sort KEY [desc]` — set sort (name / app / status / health / version / age).
- `:group on|off` — toggle group-by-application.
- `:redact on|off` — toggle redact mode (account id, ARN, CNAMEs).
- `:events on|off` — toggle the events panel.
- `:event-time [utc|local|age]` — event timestamp display; no arg cycles (default UTC). Also bound to `T`.
- `:cost on | off | status` — toggle the COST column ($/month per env via Cost Explorer; 24h cache).
- `:cols list | hide NAME | show NAME | reset` — column management.
- `:filter NAME` / `:f NAME` — recall a saved filter.
- `:save NAME` — save the current filter as `NAME`.
- `:drop NAME` — remove a saved filter.
- `:filters` — list saved filters.
- `:save-view NAME` — snapshot filter + sort + grouping + scope under `NAME`.
- `:view NAME` — load a previously saved view.
- `:views` — list saved views.
- `:view-drop NAME` — remove a saved view.
- `:pin` — pin / unpin selected env (also `*`).
- `:alias NAME LABEL` — set or update a local env alias.
- `:alias-drop NAME` / `:alias-rm NAME` — remove an alias.
- `:refresh` — re-fetch the table immediately.

> Saved filters and saved views share one store; `:filter`/`:save`/`:drop`/`:filters` operate on the filter-only encoded form, `:save-view`/`:view`/`:views`/`:view-drop` on the full encoded form (filter + sort + grouping + scope). `]` / `[` cycle through them on the env table.

## Per-env inspection

- `:why` / `:diagnose` — diagnostic overlay (events + alarms + instances + recent deploys; DLQ peek for Worker envs). Bound to `!`.
- `:diff NAME` — side-by-side env comparison vs selected env. `:diff ENV-A ENV-B` names both explicitly.
- `:config-diff ENV` — option-setting deltas between the selected env and `ENV`.
- `:config-diff-local [NAME]` — diff the deployed env against a local EB CLI saved config under `.elasticbeanstalk/saved_configs/`. No arg auto-picks the lone file.
- `:lineage` — deploy-only timeline (one row per version label, newest first, with Δ between deploys).
- `:changes` — deploy + config-change timeline from the env's event history.
- `:alarm-history NAME` — recent CloudWatch alarm transitions (StateUpdate / ConfigurationUpdate / Action, newest first).
- `:ssh [i-abc]` — open an embedded SSM session into an env instance. No arg opens a picker over cached Detail/Instances. Needs `aws` CLI + session-manager-plugin on `PATH`.
- `:ssm-run "<cmd>"` — fan a shell command across the env's instances via SSM Run Command (AWS-RunShellScript). 60s wall-clock cap. Gated by read-only / per-env safety pin.
- `:resources` / `:res` — `DescribeEnvironmentResources` dump.
- `:alarms` — CloudWatch alarms attached to the env.
- `:versions` — application versions (deployed marker, deploy hint).
- `:options [NAMESPACE]` — full settable-option vocabulary for the env's platform (current value + default + type + constraints). Slow.
- `:saved-configs` / `:configs` — saved configuration templates per application (interactive: `a` apply, `i` inspect, `x` delete, `c` create).
- `:custom-platforms` / `:platforms` — custom EB platforms.
- `:listeners` — ALB listener config (per-port proto / cert / SSL policy / default rule). Web-tier only.
- `:rds` — RDS instance config attached to the env (engine / class / credentials; password redacted).
- `:secrets [FILTER]` — browse Secrets Manager (region-scoped). Metadata only.
- `:secret NAME` — fetch one secret's value (CloudTrail-audited). Respects `:redact`.
- `:apps-info` — application metadata overlay (description / dates / templates / envs).
- `:logs-insights [--window 30m|1h|6h|24h|7d] QUERY` — run a CW Logs Insights query against the env's log groups.
- `:history` — recent info / error messages.
- `:pending` / `:in-flight` — in-flight + recently-completed actions across all envs.
- `:rollbacks-armed` / `:rb-armed` — currently-armed `--auto-rollback` watchdogs (env, rollback target, time-to-deadline).

## Diagnostics

- `:lint [ENV]` — run the diagnostic rule engine against the selected env (or named env). Twelve rules: AllAtOnce-on-multi-instance (EBL001), missing health-check URL (EBL002), env Red >4h (EBL003), batch-size > max-size (EBL004), single-instance prod (EBL005), low cooldown (EBL006), ELB without HTTPS (EBL007), stale platform version (EBL008, TUI-live in 0.17+), ASG missing health-check grace period (EBL009), missing required tags (EBL010, 0.17+ — needs env_tag_keys plumbing per call site), worker DLQ stuck (EBL011, 0.17+), Green-but-0-instances divergence (EBL012, 0.17+). Operator-tunable via `lint.disable` in `config.toml`. Also available as `ebman lint` for CI; `ebman lint --fix --yes` auto-remediates EBL001 / EBL004 / EBL006 / EBL009 (the rules with an obvious correct answer); per-rule opt-out via `lint.fix_disable`. `ebman lint --watch [--interval 60s]` (0.16+) loops at the configured interval for monitoring loops; Ctrl-C to exit. `ebman lint --baseline FILE` (0.17+) snapshots issues for CI grandfathering; `ebman lint --against-baseline FILE` diffs against the snapshot — exit 3 only on NEW issues.
- `:drift [ENV|refresh]` — terraform drift report. No arg → selected env. `refresh` re-reads tfstate (run after `terraform apply` mid-session). Discovery walks up from cwd for `.terraform/terraform.tfstate` (preferred) or local `terraform.tfstate`. Also available as `ebman drift` for CI.
- `:explain` / `:explain ARN ACTION` — diagnose the last IAM AccessDenied via `iam:SimulatePrincipalPolicy`. Explicit-pairs form evaluates a given principal/action.
- `:explain EBL###` — LLM-backed explanation of a lint issue against the selected env (e.g. `:explain EBL001`). Routes to the configured Provider (Anthropic or Ollama). Opt-in via `[explain] enabled = true` in `config.toml` + provider API key. Responses cached at `~/.cache/ebman/explain/`. Also available as `ebman explain EBL### [--env NAME]` for CI.
- `ebman audit [--tail] [--since DUR] [--env NAME] [--rule ID] [--action NAME] [--json]` — CLI-only; surface the local `~/.cache/ebman/audit.log` with structure + windows + filtering for Slack-bot routing / on-call dashboards.

## Write — env state

- `:rebuild` — terminate + recreate every instance (Y/N confirm).
- `:restart` — restart the app server on every instance (Y/N confirm).
- `:terminate` — TERMINATE env. Typed-name confirm; irreversible.
- `:deploy LABEL [--preview] [--auto-rollback Nm] [--wait-for-green Nm]` — ship an existing application version. Each dispatch captures a pre-deploy snapshot in `state.toml` for `:rollback`. `--preview` opens a side-by-side overlay without dispatching. `--auto-rollback Nm` arms a watchdog that redeploys the snapshot if the env doesn't reach Green in N minutes. `--wait-for-green` pins a success/timeout status when the deploy resolves.
- `:deploy --from PATH [--label L] [--describe D] [--no-deploy]` — upload a local `.zip` (or `--from s3://bucket/key`), register a new version, optionally deploy.
- `:rollback [--to LABEL] [--auto-rollback Nm]` — redeploy the previous version. No arg uses the captured pre-deploy snapshot (falls back to event-history scan). `--to LABEL` targets a specific version. `--auto-rollback` arms a roll-forward watchdog.
- `:undo` — reverse the most-recent option-settings write. 10-entry ring buffer; `:undo` of `:undo` redoes the original.
- `:promote-env SOURCE TARGET [--auto-rollback Nm] [--wait-for-green Nm]` — ship `SOURCE`'s currently-deployed version label to `TARGET` in one dispatch. Refuses if `SOURCE` has no version, or if `SOURCE`'s version is already on `TARGET`.
- `:rollout LABEL --regions r1,r2,r3 [--env NAME] [--wait-for-green Nm]` — sequential cross-region deploy. Pre-flights every region (env existence, current version), shows a plan overlay, dispatches on `y`. Stops on first failure. Single `rollout_id` correlation across audit lines. Also available as `ebman action rollout`. New in 0.16: `--parallel [--max-concurrency N]` fans out concurrently (implies `--continue-on-fail`); `--continue-on-fail` attempts every region in sequential mode (no halt on first failure); `--staggered Nm` waits N minutes between regions in sequential mode (canary pattern; requires `--wait-for-green`).
- `:abort-rollback [ENV]` — disarm an armed `--auto-rollback` watchdog. No arg drains all in the current context.
- `:freeze-deploys [REASON…]` — session-scoped fleet-wide write-lock. Every destructive op refuses while frozen. Useful during incident triage. Re-issue to update the reason in place.
- `:thaw-deploys` — clear the session-scoped freeze.
- `:upgrade [ARN]` — list compatible platforms; with ARN, dispatch the migration.
- `:clone NEW-NAME` — clone selected env.
- `:scale N` — set ASG min=max=N. Use `:stop` for 0, `:start` for 1.
- `:stop` — ASG min=max=0 (Y/N confirm).
- `:start` — ASG min=max=1 (Y/N confirm).
- `:capacity` — modal form to edit Min / Max / Instance type / Cooldown in one shot (pre-filled from `DescribeConfigurationSettings`).
- `:scaling-triggers` — modal form for the metric-based autoscaling trigger (metric / statistic / period / breach duration / thresholds / scale increments).
- `:swap TARGET` — swap CNAMEs (Y/N confirm; same pre-flight as `a` → Swap).
- `:abort` — cancel an in-flight env update.
- `:listener-edit PORT` — modal cert picker for an ALB listener: pick from the region's ACM certificates (loaded live). PORT = `443` / numeric / `default`.
- `:rds-attach` — modal form to couple an RDS instance to the env (engine / class / storage / credentials / deletion policy / Multi-AZ). Pre-fills if one is already attached.
- `:rds-detach ENV` — safe-ify the coupled RDS: sets `DBDeletionPolicy=Snapshot` so the DB survives env termination. Repeat the env name to confirm. Does not decouple (EB has no detach op).

## Write — env config

- `:env list | set KEY VAL | unset KEY` — application env-var editor (triggers app-server restart).
- `:env-edit` — bulk env-var editor: opens current env vars in `$EDITOR` (KEY=VALUE), diffs + dispatches on save.
- `:tag KEY VALUE` — env tag editor.
- `:untag KEY` — remove env tag.
- `:set-option NS OPT VALUE` — generic option-settings escape hatch.
- `:unset-option NS OPT` — clear an option setting.
- `:instance-type TYPE` — EC2 instance type (e.g. `t3.medium`; rolling launch-config replacement).
- `:keypair NAME` — set EC2 key pair on the env's ASG.
- `:service-role ARN` — set EB service role ARN/name.
- `:instance-profile NAME` — set EC2 instance-profile on the env's ASG.
- `:public-ip on|off` — toggle EC2 public IP association.
- `:elb-scheme public|internal` — set ELB scheme (rolling).
- `:subnets` — modal MultiSelect picker for `aws:ec2:vpc.Subnets`.
- `:elb-subnets` — modal MultiSelect picker for `aws:ec2:vpc.ELBSubnets` (web-tier).
- `:security-groups` — modal MultiSelect picker for instance SGs (launch-config).
- `:deployment-policy AllAtOnce|Rolling|RollingWithAdditionalBatch|Immutable|TrafficSplitting` — deploy policy.
- `:rolling-update on|off` — ASG rolling-update policy.
- `:health-check-url /path` — HTTP health-check path.
- `:logs-stream on|off [--retention DAYS]` — toggle CW Logs streaming (default 7d).
- `:logs-tail [LOG_GROUP]` — open a live streaming overlay for a CW Logs group (picker if multiple groups; auto-picks `web.stdout.log`).
- `:notify EMAIL_OR_SNS_ARN | off` — set notification endpoint.
- `:managed-window DAY HOUR | off` — managed-platform-updates window (Mon..Sun, 0..23).

## Write — application versions / configs / alarms / platforms

- `:delete-version LABEL [--force]` — delete an application version (`--force` also nukes the S3 bundle).
- `:config-save NAME` — save current env as a config template.
- `:config-apply NAME` — apply a saved template to selected env (Y/N confirm).
- `:config-delete APP NAME` — delete a saved config template.
- `:config-inspect TEMPLATE` — inspect a saved config template.
- `:alarm-create NAME KIND THRESHOLD [OP]` — CloudWatch alarm (KIND: `health` / `4xx` / `5xx` / `latency`).
- `:alarm-delete NAME` — remove a CW alarm.
- `:custom-platform-delete ARN` — delete a custom EB platform (fails if any env uses it).
- `:metric add LABEL NS NAME [STAT]` / `:metric remove LABEL` / `:metric list` — custom Metrics-tab charts.

## Multi-account / multi-region

- `:region NAME` / `:region all` / `:r NAME` — switch region, or fan out across configured regions.
- `:profile NAME` / `:p NAME` — switch AWS profile.
- `:account NAME` — switch to an AssumeRole account (`accounts.NAME` in `config.toml`). Falls back to `:profile` aliasing when no `accounts.` entry exists.
- `:accounts` — list AWS-Organizations child accounts; rows matching a configured `accounts.NAME` get a `:account NAME` switch hint.
- `:find-env SUBSTRING` — scan every profile in `~/.aws/{config,credentials}` **and** every configured AssumeRole account in REGION.
- `:envs-by-version LABEL` — fleet-wide blast-radius for a bad build (which envs are running `LABEL`).
- `:org-health` — aggregate env / red counts per profile + per configured AssumeRole account.

## Multi-env (bulk ops)

- `space` — toggle multi-select.
- `:batch-rebuild` — fan rebuild across selection.
- `:batch-restart` — fan restart across selection.
- `:batch-deploy LABEL` — deploy the same version to every selected env in parallel.
- `:batch-tag KEY VALUE` — fan a tag write across the selection.
- `:batch-untag KEY` — fan a tag remove across the selection.
- `:batch-set-option NS OPT VALUE` — fan an option-settings write across the selection.
- `:deselect` / `:select-clear` — clear selection (Esc also works in Normal mode).

## Output / yank

- `:export` — yank filtered view as TSV.
- `:json` — yank filtered view as JSON.
- `:report` / `:markdown` — yank filtered view as a Markdown table.

## Read-only mode

- `:readonly on|off` — toggle destructive-action lockout. `--read-only` on the CLI does the same at startup. Per-env / per-account safety pins in `config.toml` add granular locks on top.

## Setup / discovery

- `:settings` — interactive form to edit `~/.config/ebman/config.toml`. Writes back on submit and live-applies theme / icons / refresh interval.
- `:update` — show (and yank to clipboard) the upgrade command for the detected install channel (Homebrew / cargo-bin / tarball).
- `:whatsnew` — embedded changelog.
- `:about` / `:credits` — version, license, attributions.
- `:report-bug` — scrubbed bug-report overlay. `y` copies the payload, `b` opens a GitHub issue with the body pre-filled. No outbound HTTP from ebman. See [safety-and-privacy](safety-and-privacy.md) for the redactor scope.
- `:plugins` — list user plugin commands defined in `commands.toml`.
- `:loglevel LEVEL` — live-reload the tracing filter (trace / debug / info / warn / error).
- `:help` / `:?` — toggle the global help screen (also `?`).
- `:quit` / `:q` — exit ebman (also `q`, `Ctrl-C`).
