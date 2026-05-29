# Lint rules reference

`ebman lint` ships with a built-in rule engine that surfaces common Elastic Beanstalk misconfigurations. Each rule fires under a defined condition and (optionally) ships an auto-remediation that `ebman lint --fix --yes` can dispatch.

The TUI surfaces lint findings in three places: the `:lint` overlay, the confirm-modal warning lines (any rule with `severity >= Warn` that fires against the pre-write state), and the `:explain ISSUE_ID` LLM-backed explainer.

This page is the source-of-truth reference. Lint rule IDs follow the `EBL###` pattern; numbering is stable across releases.

---

## Severity

| Severity | When it fires | Default exit code (`ebman lint`) |
|---|---|---|
| `Error` | Operator-correctable issue that's actively wrong (no rule currently emits this — reserved for future use) | 3 |
| `Warn` | Operator should fix this; defaults work but you're on a worse path | 3 |
| `Info` | Soft hint; common operator preferences that aren't universally right | 3 |

`ebman lint --severity warn` filters out `Info` issues; `--severity error` filters to only `Error` issues.

## Per-rule configuration

`config.toml`:

```toml
[lint]
# Skip rules entirely (no firing in TUI or CLI).
disable = ["EBL017", "EBL019"]

# Skip auto-fix dispatch for specific rules. The rule still fires
# and the issue is still reported; `--fix` just won't auto-remediate.
fix_disable = ["EBL004"]
```

Project-local `.ebman/ebman.toml` supports the same keys and extends the user-level config (does not replace it).

---

## Rules

### EBL001 — AllAtOnce deploy policy on multi-instance env

**Severity:** Warn · **Auto-fix:** `SetOption DeploymentPolicy=Rolling`

Detection: `aws:elasticbeanstalk:command:DeploymentPolicy = "AllAtOnce"` AND `aws:autoscaling:asg:MaxSize > 1`.

Why it matters: every instance restarts simultaneously during a deploy, so the env is fully unavailable for the rollout duration. On a multi-instance env this is almost never intentional.

Fix: switch to `Rolling` (preserves capacity) or `RollingWithAdditionalBatch` (zero downtime).

### EBL002 — Web tier without health-check URL

**Severity:** Warn · **Auto-fix:** Manual

Detection: env tier is `Web` AND `aws:elasticbeanstalk:application:Application Healthcheck URL` is unset/empty.

Why it matters: EB falls back to TCP-only health checks. Operators get false positives (the process is running but the app is broken) and lose the per-instance health visibility EB's HTTP path provides.

Fix: operator must know the app's health-check path (`/health`, `/_status`, etc.) — auto-fix can't guess. The rule prints the namespace + setting name so `:set-option` is one keypress away.

### EBL003 — Env Red for an extended period

**Severity:** Warn · **Auto-fix:** None (state condition, not a config issue)

Detection: env health is `Red` or `Severe` AND `updated_at` is more than 4 hours ago.

Why it matters: long-Red envs that haven't been acted on are either incidents in progress (operator's awareness check) or forgotten state (operator should triage).

### EBL004 — BatchSize exceeds MaxSize

**Severity:** Warn · **Auto-fix:** `SetOption BatchSize=MaxSize` (clamps)

Detection: `aws:autoscaling:updatepolicy:rollingupdate:MaxBatchSize > aws:autoscaling:asg:MaxSize`.

Why it matters: the deploy effectively becomes `AllAtOnce` because EB tries to batch more instances than exist. The batching numbers lie.

Fix: clamp `BatchSize` to `MaxSize`.

### EBL005 — Single-instance env in production

**Severity:** Info · **Auto-fix:** Manual

Detection: env name matches `prod`/`production` (case-insensitive) AND `aws:autoscaling:asg:MaxSize == 1`.

Why it matters: single-instance prod envs have no capacity headroom and no AZ redundancy. Often a leftover from initial bootstrap that didn't get sized up.

Fix: scale to 2+ instances. Operator-context-dependent.

### EBL006 — Cooldown below recommended

**Severity:** Info · **Auto-fix:** `SetOption Cooldown=60`

Detection: `aws:autoscaling:asg:Cooldown < 60`.

Why it matters: low cooldowns cause scale-up / scale-down thrashing during traffic spikes.

Fix: bump to 60s (AWS-recommended floor for most workloads).

### EBL007 — ELB without HTTPS listener

**Severity:** Warn · **Auto-fix:** Manual (cert ARN required)

Detection: env has a `LoadBalanced` environment-type with no listener configured on port 443.

Why it matters: HTTP-only listeners are an op-sec / compliance gap. Most modern compliance regimes (SOC 2, ISO 27001, FedRAMP) require TLS in transit.

Fix: operator picks a cert ARN; `:listener-edit 443` opens the cert-rotation form.

### EBL008 — Stale platform version

**Severity:** Warn · **Auto-fix:** Manual

Detection: env's solution_stack name is older than the newest stack in the same family (via `ListAvailableSolutionStacks`).

Why it matters: stale platforms miss security patches and language-runtime updates. Family-tracking version comparison (e.g. `v6.1.0` → `v6.2.0`).

Fix: `:upgrade <new-platform-arn>` — operator picks the target.

TUI-live: 0.17+. CLI-live: 0.18+.

### EBL009 — ASG missing health-check grace period

**Severity:** Info · **Auto-fix:** `SetOption HealthCheckGracePeriod=300`

Detection: `aws:autoscaling:asg:HealthCheckGracePeriod < 60` (or unset).

Why it matters: ASG starts terminating new instances before they've finished bootstrapping. Common cause of "the env keeps replacing itself" loops on slow-starting apps.

Fix: bump to 300s (5 min — enough for most app startups).

### EBL010 — Missing required tags

**Severity:** Info · **Auto-fix:** Manual

Detection: `required_tags` in config.toml is non-empty AND env's tag keys are non-empty AND at least one required key is missing (case-insensitive).

Why it matters: cost-allocation tags, compliance tags, and ownership tags that the org has standardized on need to be present.

Fix: operator dispatches `:tag Owner=team-a` (one per missing key). Tag values are operator-specific so auto-fix can't guess.

Live: 0.18+ (env_tag_keys fetched via `list_tags` at lint time).

### EBL011 — Worker DLQ stuck

**Severity:** Warn · **Auto-fix:** Manual

Detection: env tier is `Worker` AND DLQ depth > 100.

Why it matters: headline failure mode for SQS-driven workers — consumer crashes or hangs, messages land in the DLQ, depth climbs until operator notices.

Fix: operator triages via `aws sqs receive-message` + worker logs before redriving / purging / scaling. `--fix` can't decide which.

TUI-live: 0.18+ (from `App.worker_dlq_depths`). CLI-unwired.

### EBL012 — Green-but-0-instances divergence

**Severity:** Warn · **Auto-fix:** Manual

Detection: env status is `Ready` AND health is `Green`/`Ok` AND `fetch_env_instance_counts.healthy == 0`.

Why it matters: classic ELB-vs-EB health-check divergence: EB reports Green but ALB target-group reports no healthy targets → traffic is failing silently.

Fix: operator drills into Detail/Health to identify the source of divergence.

Live: 0.18+ (via parallel `fetch_env_instance_counts` at lint time).

### EBL013 — Launch configuration ASG (legacy)

**Severity:** Warn · **Auto-fix:** Manual (env rebuild required)

Detection: any non-empty option in the `aws:autoscaling:launchconfiguration` namespace.

Why it matters: AWS is sunsetting EC2 launch configurations (no new account onboardings since 2024-12-31). EB envs still on the legacy shape will face migration friction down the line.

Fix: plan a launch-template migration — verify the platform version supports it, then rebuild the env. Capacity-loss planning is operator-context-dependent.

Live: 0.20+.

### EBL017 — Managed Platform Updates disabled

**Severity:** Info · **Auto-fix:** Manual

Detection: `aws:elasticbeanstalk:managedactions:ManagedActionsEnabled` is not `"true"` (case-insensitive), or the setting is absent (EB defaults to disabled on most modern platforms).

Why it matters: env doesn't receive automatic security patches during the configured maintenance window. Operators must dispatch `:upgrade` manually when AWS publishes a new platform version.

Fix: `:set-option aws:elasticbeanstalk:managedactions:ManagedActionsEnabled true` AND configure `PreferredStartTime`. Some operators disable deliberately (frozen prod env mid-incident, controlled patching via CI) — disable EBL017 in those cases.

Live: 0.19+.

### EBL019 — AllAtOnce on multi-subnet env

**Severity:** Warn · **Auto-fix:** `SetOption DeploymentPolicy=Rolling`

Detection: EBL001's condition (`DeploymentPolicy=AllAtOnce` + `MaxSize>1`) AND `aws:ec2:vpc:Subnets` has 2+ subnets.

Why it matters: stronger version of EBL001. On a multi-AZ env, AllAtOnce takes EVERY availability zone offline at once, defeating the fault tolerance you're paying for.

Fix: same as EBL001 — switch to `Rolling`. Subnet count is the cheapest proxy for "multi-AZ" — EB doesn't expose AZ mapping in option settings, so we infer from subnet count. False-positive possible on the rare case where two subnets live in the same AZ; operators can disable EBL019 if that bites.

Live: 0.20+.

---

## Roadmap

Held / pending live-EB verification:
- EBL014 — deprecated CW namespace in `:scaling-triggers`
- EBL015 — custom platform with no published versions in 180+ days
- EBL016 — live health-check probe non-2xx (`--probe-live` gated)
- EBL018 — env without WAF + on prod tier
- EBL020 — env with X-Ray enabled but instance profile has no `xray:PutTraceSegments`

These rules have clear semantics but each needs detection-shape verification against a live EB env before shipping.

---

## Adding a new rule

See [rule-development.md](rule-development.md) (coming in 0.21+). The short version:

1. Implement the `Rule` trait (`src/lint.rs`): `id()`, `severity()`, `applies()`, optional `fix()`.
2. Register in `default_rules(&[])` so it's picked up by the registry.
3. Add 3-5 tests covering: fires-on-expected-input, does-not-fire-without-trigger, fix shape (if auto-fixable), and at least one false-positive guard.
4. Update this docs page with the rule's entry.

The `LintContext` builder pattern (`.with_dlq_depth(...)` / `.with_env_tag_keys(...)` / etc.) is how new external inputs get plumbed into rules. Adding a new field to `LintContext` is a 3-site edit (struct + builder method + the rule that reads it).
