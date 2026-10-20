# The App tab: is it healthy, and who is using it?

Status: design, 2026-10-20. PLAN.md item 3.

One Detail tab per environment that answers both questions on one
screen, from data the operator already publishes. Generic: ebman knows
CloudWatch, Logs Insights and Elastic Beanstalk, never the application.

Scope ruling (2026-10-20): RDS and ElastiCache appear only as resources
**named** in that env's config, and only as CloudWatch metrics through
the same panel. No tag discovery and no new SDK clients. BACKLOG's
"multi-service dashboard" line stays out of scope.

## What exists, and what it lacks

| piece | today | gap |
|---|---|---|
| Metrics tab | `draw_detail_metrics` (`ui/detail.rs:1127`): one chart per series, now/max/min/Δ, `[` `]` range | env aggregate only; no units |
| Custom metrics | `:metric add`, saved in **state.toml**, global to every env | not shareable config; not per env; a failed fetch is dropped silently (`spawn_detail.rs:238`) |
| Alarms | dimension match (`list_alarms_for_env`), ALARM-first in Health tab and `:why` | no name-prefix match; `:alarms` overlay is unsorted |
| Instance health | `DescribeInstancesHealth` with `All` attributes (`aws/eb.rs:2066`) | `ApplicationMetrics` (p99, request counts, status codes) and `System` (CPU) are fetched and **thrown away** |
| Logs Insights | `run_insights_query`, `:logs-insights` | ad hoc only; nothing counts errors |
| MCP | `recent_logs` etc. | no metrics read tool |

## Config

Per env, in `config.toml`, as a prefix family like `runbooks.ENV`.
EB environment names are letters, digits and hyphens, so `app.ENV.field`
splits unambiguously.

```toml
app.Shop-prod.namespace = "Shop"
# Dimensions that select this env's series. Default: EnvironmentName=<env>.
app.Shop-prod.dimensions = "Environment=prod"
# Dimension that names an instance, for the per-instance breakdown.
app.Shop-prod.instance_dimension = "InstanceId"
# LABEL = "MetricName|stat|unit". stat defaults to Average.
app.Shop-prod.metric.users = "usersActive|Maximum|count"
app.Shop-prod.metric.heap = "heapUsedMb|Average|MB"
app.Shop-prod.metric.5xx = "errors5xx|Sum|count"
# Optional colouring: warn,critical. Higher is worse unless prefixed "<".
app.Shop-prod.threshold.heap = "1500,1800"

app.Shop-prod.rds = "shop-prod-db"              # DBInstanceIdentifier
app.Shop-prod.elasticache = "shop-prod-cache"   # CacheClusterId

app.Shop-prod.alarm_prefix = "Shop-prod-"
# A log group name, or a suffix under /aws/elasticbeanstalk/<env>/.
app.Shop-prod.log_group = "var/log/app/main.log"
# Named group `fp` is the fingerprint. Values are literal: one
# backslash. Default: the first 100 characters from ERROR.
app.Shop-prod.error_fingerprint = "ERROR (?<fp>[\w.$]+(Exception|Error))"
```

Unknown fields under `app.ENV.` are a parse error carried to the
operator, not passthrough: a typo in a metric spec must not read as
"no data".

`docs_drift::every_config_key_is_documented` only sees literal match
arms, so the `app.` family (and the existing `runbooks.` / `safety.`
families) are invisible to it. The guard is extended to take each
family's field list from the parser, so a new field cannot ship
undocumented.

`:metric add` and its state.toml entries stay as they are: a personal,
ad hoc overlay on the Metrics tab. The App tab is shared config. Both
render through the same series code.

## Fetching

All through `detail_client()` (the row's region), one spawn per
section, each with its own `DetailFetch` variant so a failure is shown
in its section rather than blanking the tab or vanishing.

- **App metrics:** one `GetMetricData` call. Per metric, a plain query
  for the env aggregate and, when `instance_dimension` is set, a
  `SEARCH('{NS,<dims>,<instance_dimension>} MetricName="X"', 'stat', P)`
  expression for the per-instance series. One call, no `ListMetrics`.
- **Linked resources:** the same call shape against `AWS/RDS`
  (`CPUUtilization`, `DatabaseConnections`, `FreeableMemory`,
  `ReplicaLag`) and `AWS/ElastiCache` (`EngineCPUUtilization`,
  `DatabaseMemoryUsagePercentage`, `CurrConnections`, `Evictions`). The
  fixed lists are data, not code paths. Serverless ElastiCache uses a
  different dimension (`clusterId`); a named resource that returns no
  series says so instead of drawing empty charts.
- **Instances:** keep `ApplicationMetrics` and `System` from the call
  already made. This works without EB's CloudWatch custom-metrics
  setting, which the `AWS/ElasticBeanstalk` env series need.
- **Alarms:** the existing fetch plus `alarm_prefix`, OR'd with the
  dimension match. ALARM first everywhere, `:alarms` included.
- **Top errors:** a Logs Insights query built by a pure function from
  the fingerprint regex:

  ```
  fields @message
  | filter @message like /ERROR/
  | parse @message /<regex>/
  | stats count(*) as n by fp
  | sort n desc | limit 10
  ```

  The regex is checked for a `fp` group and for an unescaped `/` before
  it is sent.

**Cadence.** Period 60s. Like every Detail tab, the App tab fetches
when opened and on `^R`, and on the tick only when `R` turns
auto-refresh on. Logs Insights
bills per GB scanned, so top errors runs on tab open and `r` only, and
the section shows how old it is.

## The tab

```
 App ─ Shop-prod ─ 1h ──────────────────────────────────────────
 ALARMS  2 in ALARM: Shop-prod-heap, Shop-prod-5xx · 6 OK
 users      42 count  ⣀⣠⣤⣶⣿⣶⣤   i-0a 20  i-0b 22
 heap     1620 MB     ⣀⣀⣤⣤⣶⣶⣿   i-0a 1510  i-0b 1730      (warn)
 5xx         3 count  ⣀⣀⣀⣀⣀⣠⣀
 rds cpu    31 %      ⣀⣠⣀⣤⣀⣀⣠   conns 48  free 1.2G  lag 0s
 cache mem  44 %      ⣀⣀⣀⣀⣠⣀⣀   cpu 8%  conns 120  evict 0
 INSTANCES  i-0a  req 12/s  p99 840ms  5xx 0   cpu 41%
            i-0b  req 13/s  p99 2.1s   5xx 3   cpu 77%
 TOP ERRORS (last 15m, 40s ago)  12 PoolTimeoutException  4 NPE
```

One row per series, not one chart per series: the Metrics tab already
draws charts, and a dozen of them do not fit one screen. Values coloured
from `threshold.LABEL` with `health_yellow` / `health_red`. The range is
the Metrics tab's `metrics_range_secs`, so `[` `]` moves both.

An env with no `app.ENV.*` config shows instances, alarms and the
`AWS/ElasticBeanstalk` series, and one line naming the config keys that
would fill the rest.

## MCP: `app_metrics`

A read tool taking `env`, `minutes`, `region`, `profile`. It returns the
same sections as JSON, each with `complete` and `errors`, in step with
`every_result_that_can_be_partial_says_so`. CAVEATS in its description:

- a metric is only as fresh as its publish interval;
- only metrics named in config are read; a clean result says nothing
  about anything else;
- top errors counts lines matching `ERROR` in the configured group only.

## Order of work

Each step lands green and separately. Re-scoped 2026-10-20 after step 1
from layers into vertical slices: a pure builder with no caller fails
`-D warnings` as dead code, so each slice goes from AWS call to screen.

1. Done: `app.ENV.*` parsing, `docs/configuration.md`, the family docs
   guard.
2. Done: the App tab with app metrics. Builders, SEARCH expressions, the
   paginated fetch, a `Fetch<AppMetrics>` (not a `DetailFetch` variant:
   the newer pattern carries its own error), rendering with
   thresholds, and the env's config errors on screen.
3. Done: linked resources (`rds`, `elasticache`) in the same call, one
   row each; a named resource with no data at all gets a note asking
   whether the name is the right kind of id.
4. Done: the instance `ApplicationMetrics` / `System` already fetched,
   as instance rows on every env's App tab (no config needed). The tab's
   loading state stays its own: tying it to `loading_instances` let the
   eager fetch on open block the tab's first metrics fetch.
5. Done: `alarm_prefix` OR'd with the dimension match, through one
   `AlarmMatch` every caller builds the same way (Health, `:why`,
   `:alarms`, MCP `why`), each pinned by its own test; results sorted
   ALARM first at the source, so `:alarms` is too; an alarm line on the
   App tab. Composite alarms are not read, as before.
6. Done: top errors, opt-in by `log_group`, over a fixed 15 minutes, on
   tab open and `^R` only (an explicit tick trigger, with the tick's
   Detail refresh pulled out of the run loop so a test drives it); not
   stacked, not in `tab_loading` (a query must not hold up metrics).
7. `app_metrics` on MCP, and `docs/headless.md`.
8. Later, by ruling: a two-env compare layout.

Fixed on the way because they are the same code: the Metrics tab's
silently-dropped custom-metric failure.
