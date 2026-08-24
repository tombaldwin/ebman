# ebman backlog

Living list of done / pending / dropped work. New entries get added at the bottom of their section. Priority tiers below are loose — pick what fits.

---

## Done

Moved to [`docs/backlog/archive.md`](docs/backlog/archive.md) — 2347
lines of completed work, so this file can be read whole.

That matters because `CLAUDE.md` makes updating this file a condition of
"done" for every landed item. At 375KB it could not be held in context,
so it got read in fragments — and entries were duplicated, or
contradicted, or a follow-up got buried inside a `[x]` entry where
nobody scanning for `- [ ]` would ever see it. The split is mechanical:
every section with zero open items moved.

## Backlog

Tier definitions:
- **Refactors** — structural / design tightening surfaced by code review.
- **Tier 0** — distribution & hygiene before shipping publicly.
- **Tier 1** — blocks daily-driver replacement of the AWS console.
- **Tier 2** — UX patterns directly borrowed from e1s / lazygit / lazydocker.
- **Tier 3** — observability and smart surfacing.
- **Tier 4** — multi-account / org-scale operations.
- **Tier 5** — safety, audit, and destructive-action workflow.
- **Tier 6** — power-user, scripting, and extensibility.
- **Tier 7** — polish and quality of life.
- **Tier 8** — maybe / unprioritised; not committed to scope.

#### 0.29 queue — 0.28 pre-tag review deferrals (2026-08-20)

The write/freeze pre-tag review (2 lenses) fixed 2 Critical + 2 Important + 1 Minor before tag (see CHANGELOG). Deferred, non-blocking:
- [x] **Unified MCP tool registry** (arch I1) — DONE 2026-08-22, as a PIN rather than a restructure. The two sides already agreed (8 read + 6 write descriptors, 14 dispatch arms) so there was no live bug to fix; what was missing was anything making them agree. Mirrors what `src/commands.rs` does for the TUI registry: a test reads the `call_tool` match arms from source and asserts the two sets are equal in both directions — a descriptor with no arm is a tool an agent calls and gets nothing from, an arm with no descriptor is dead, because `tools/call` refuses names absent from the table. A second test pins that no write tool is advertised without `--allow-writes`, which the same membership check turns into a write-surface property rather than a listing cosmetic. Restructuring into `&[ToolDef{...}]` would buy nothing further: the schemas are `json!` literals that have to live somewhere, and the pin already catches the drift the entry was worried about. Original note:  — the spec's `&[ToolDef{name, schema, is_write, handler}]` single table; today name/schema/handler live in three sites (tool_table descriptors + call_tool match + RPC existence check) with no compile-time or test link. A coverage test that every `tool_table(true)` name resolves to a real handler was NOT added this run — add it OR the full slice refactor (~half day). Drift is currently a runtime `isError`, not a panic.
- [x] **Shared verb-dispatch helper** (arch M2) — DONE 2026-08-22 as `CliVerb`, ~40 lines. And it immediately paid for itself: the test asserting the CLI's audit label matches the TUI's for the same verb FAILED, because the CLI wrote `Restart` while the TUI writes `RestartAppServer`. Two consequences, both real — `ebman audit --action` matched half the history either way, and `audit replay` accepted only `Restart`, so **the most common restart in any log, the TUI's, was unreplayable**. The CLI writes the canonical name now and replay accepts both spellings. Original note:  — `dispatch_write` (writes.rs) and `action::run` (cli/action.rs) both hand-map verb→method + the audit pair; a shared `dispatch_verb()` removes the drift surface (~2h).
- [x] **pin/freeze check-order + freeze-message rendering unified** (arch M3) — DONE 2026-08-22. `freeze::refusal_message` is the one sentence; the CLI's two sites go through `refuse_write`, which does freeze-then-pin like the MCP gate. The order lives in one function now rather than being two bare calls in sequence, which is how it came to differ. Original note:  — CLI checks pin-then-freeze, MCP checks freeze-then-pin; the freeze refusal string is rendered in two places. Cosmetic inconsistency for an operator comparing outputs.
- [x] **Superseded-token message** (bugs/arch M1) — FIXED 2026-08-22. `WriteState::install` retires the token it replaces (bounded at 8), and `mismatched_token_message` tells a superseded token from an unknown one so the agent knows whether to re-read the newer plan or re-send what it holds. Past the cap it falls back to "unknown", which is honest. Was: CONFIRMED STILL REAL 2026-08-22 (`writes.rs:468`): there is one `pending` slot, so a newer plan replaces the old one and confirming the old token returns "unknown confirm_token", indistinguishable from a typo; distinguish "superseded by a newer plan".
- [x] **`lint --quiet` can exit non-zero with nothing to explain it.** FIXED 2026-08-24.
  `src/cli/lint.rs:866` sets `cycle_degraded = true` when a probe could
  not run, then gates only the `eprintln!` on `!quiet`. So `--quiet`
  suppresses the reason while keeping the failing exit code — a red CI
  step with an empty log. Either `--quiet` should also stop degrading
  the exit, or the degrade reason needs a channel `--quiet` doesn't
  silence. Noted 2026-08-24; verified still live at 0.33.0.

- [x] **`lint --json` has no degraded field.** FIXED 2026-08-24 — same root cause as the item above (the degrade reason had no reliable channel), so both were fixed by funnelling all four degrade sites through one `degrade()` helper that prints, records for `--json`, and sets the flag together. Guarded against a fifth site being written the old way. `coverage_warnings`
  reaches `eprintln!` only (`src/cli/lint.rs:866-872`) and never enters
  the JSON payload, so a machine consumer cannot tell a clean run from
  one where a probe was skipped on AccessDenied — which is exactly the
  distinction `ProbeOutcome::Unknown` was introduced to preserve. The
  human output makes it; the JSON output flattens it back. Noted
  2026-08-24; verified still live at 0.33.0.

- [ ] **The six `Vec`-shaped Detail fetches still pair a value with a
  `loading_*: bool`.** Surfaced 2026-08-24 by the `Fetch<T>` work, which
  converted the two pairs that carried their own `Result` and stopped
  there. These six (`events`, `instances`, `queues`, `metrics`, `tags`,
  `env_vars`) settle into `DetailState`'s *shared* error slot instead, so
  wrapping them in `Fetch<T>` adds an error arm nothing fills and changes
  what the footer shows.

  The decision it needs is a UI one, not a mechanical one: per-section
  errors, or keep the single panel error. Recorded here rather than
  inside the completed `Fetch<T>` entry because a follow-up buried in a
  `[x]` item is invisible to anyone scanning for open work. See the
  `Collapse the Option<T> + loading_* pairs` entry for the full analysis.

- [ ] **`draw_table`'s `DisplayRow::Env` arm is still inline** (~200
  lines of a 389-line function). The `Separator` arm was extracted in
  0.30; this one captures ~30 `App` fields and would need a context
  struct, which is where the value per edit drops off sharply. Same
  reason as above for surfacing it: the note lived inside a completed
  entry. See the `Separator branch extracted` entry for what the attempt
  learned (holding `&App` in the context does not compile — the rows
  borrow `app.environments` and it defeats the field-level split that
  lets `&mut app.table_state` coexist).

- [ ] **`HelpTopic::Shell` is never constructed, so `draw_help_shell` is
  unreachable.** Found 2026-08-24 by converting `#[allow]` to
  `#[expect]`: the type-level suppression on `HelpTopic` was hiding a
  variant-level finding. Every other variant is assigned somewhere;
  `Shell` is not. In Shell mode every keystroke goes to the PTY —
  including `?` — so there is no way to ask for the shell help while you
  are in the shell.

  The screen exists and is written. The question is whether to delete it
  or reach it another way (from the global help, or before attaching),
  which is a design call rather than a mechanical one.

- [ ] **`DeploySnapshot::env_name` is written and never read.** Same
  discovery. The map holding these is keyed by env name, so the field
  duplicates its own key — but it is also what makes a persisted line
  self-describing in `state.toml`, which matters when reading that file
  by hand. Decide: drop it, or document it as an on-disk-format field
  and stop treating it as dead.

- [ ] **CLI rollout/auto-rollback freeze is start-only** (bugs M3) — a freeze declared mid-rollout doesn't stop later regions; matches the pin start-gate semantics, flagged as a conscious choice.

#### `aws/` fourth review pass — 2026-08-22

Reviewed the third-review fixes and the write-safety tests. Fifteen findings; the severe ones were all defects in those fixes.

- [x] **A test wrote into the developer's real cache** — confirmed on the machine: `~/.cache/ebman/cost-unknown-us-east-1.toml` held a fabricated `env."full" = 1.00` with a fresh timestamp, written by `cargo test`. The cost cache is stale only after 24 h, so the next real session would have rendered that fiction, shown `—` for every real env, and skipped the fetch that would correct it. `util::cache_dir()` now redirects under `cfg!(test)`.
- [x] **`list_environments` + `.complete()` made a region vanish** — the fan-out only reported an error when EVERY region failed, so a region hitting its page budget went from contributing a short list to disappearing silently. `AppMsg::Refresh` carries `partial_errors` and the handler names the missing regions — set *after* the auto-clear, since a successful refresh wipes `error_message`.
- [x] **The `accounts.*` emit destroyed real config** — `parse` materialises an entry before matching the field, so a typo left a phantom spec with an empty ARN, and the new emit wrote it back as `role_arn = ""` over the operator's real line. Phantoms are now skipped. Value escaping was added and then removed: `parse` reads with `trim_matches('"')` and has no escape handling, so an escape it can't decode is worse than none — the limitation is pinned by an `#[ignore]`d test.
- [x] **The cache epoch guard didn't close the race** — the epoch was read outside the lock the insert takes, so a clear could complete between them and the stranded builder repopulated the map a profile switch had just emptied. Now one `install_if_current` doing both under the same lock.
- [x] **The gap marker was made less visible by being made to survive** — the `GAP` sentinel fell through to the `_` arm, `muted`, indistinguishable from routine chatter. The severity→colour map was inlined in three places and had drifted; now one `ui::event_severity_style`. And the marker can still be evicted by its own batch, so a sticky `truncated_polls` counter in the overlay chrome carries the signal instead.
- [x] **Three of the new tests could not fail** — the only test for the console-region fix re-implemented the fixed expression inline, the trim test asserted arithmetic over two constants, and the epoch test only checked a counter increments. All three rewritten to call production code, each verified by mutation.
- [x] **`:alarm-add` doesn't exist** — the command is `:alarm-create`, and the invented name reached the config docs and three source comments. CI pins the registry to the dispatch arms but not to prose; a test now does, verified against the mistake.
- [x] **`.complete()` could wall a large account, and the rule was applied inconsistently** — the requests set no page size, and six listings that feed pickers or `.find()` lookups still took `.items()` while their own comments said a short result reads as absence. Page sizes set, the rule applied throughout, and a drift guard requires any remaining `.items()` to be named with a reason.
- [x] **`alarm_dimensions` accumulated and rewrote the operator's line** — a second `alarm_dimensions =` line unioned with the first instead of replacing it, and `serialize` emitted the full match set so a hand-written `"Environment"` became `"EnvironmentName,Environment"` on the first save. Now rebuilt from the canonical name and only the added names are written.
- [x] **`costs_complete` had no reader where it mattered** — `:cost status` reported "no data yet" while figures were on screen, and `:fleet-cost` rendered an under-reporting total with no marker. Both now say so, and the flag resets when the map is torn down.
- [x] **`yank_cli` still used the home region** — the sibling of a fix applied ten lines above, on the surface most likely to be pasted into a channel as evidence. One `App::region_for` accessor now serves all three sites. The instance-console link deliberately keeps the home region: `d.instances` is fetched with the home-region client, so the row's region would name a home-region instance ID in another region's console.
- [x] **Smaller** — a stray space rendering as "1000 -event buffer" (the guard added one commit earlier tested for three spaces, so it couldn't see it; it tests for two now), a `MAX_PAGES` shadow, the `PARTITIONS` header asserting an unsound ordering rule (now a checked cross-product test), and the process-global cache lock not covering the app-side tests that clear it.

Still open, recorded rather than fixed:

- [x] **Detail shows home-region data for a fan-out row** — FIXED 2026-08-22 (0.30.0). `spawn_detail_*` all used `self.aws`, whose region is `context.region`, so opening Detail on an environment from another region showed that region's name with the home region's instances, metrics and events. Detail carries its own client now. The instance-console link had been *worked around* to match the wrong data; that compensation became a bug once the data was right, and was fixed in the 0.30.1 review round.
- [x] **`SCAN_PAGES = 500` is worst-case 500 sequential round trips** — FIXED 2026-08-22. on three interactive triage paths, with no timeout, no cancel and no partial render — and `detail_nav` has no in-flight guard, so scans longer than the 15 s tick stack up.
- [x] **The client-cache TTL reaches one call path** — FIXED 2026-08-22. — only `list_environments_in_region` uses `cached_client`; everything else goes through `App::spawn_aws` → `self.aws`, which is replaced only by an explicit context switch. A single-region operator never reaches the cached path at all, so pasting fresh static credentials still does nothing until restart.
- [ ] **`WRITE_COMMANDS` is hand-written** — RE-SCOPED TWICE, 2026-08-22. **It is not derivable from a verb table** (see the design pass below: the test lists `:command` invocations, of which only 3 of 12 correspond to an `Action` variant — a different axis). Keep the list hand-written; the open question is only whether a reachability guard can flag a missing entry, and the attempt at that produced false positives in both directions. Earlier note follows. RE-SCOPED after an attempt. The plan was "add a `write` flag to `CommandSpec` and derive the test list". Deriving the *classification* automatically does not work, demonstrated in both directions: a transitive walk from each dispatch arm reports `:explain` and `:changes` as gated (they are not — zero `deny_write` calls; the walk reaches shared helpers) and `:rebuild` as non-mutating (it is *the* canonical write; it goes through the confirm modal into `spawn_action`, which the walk doesn't reach). 67 of 131 commands came back flagged, and the list is wrong at both ends.
  So the flag has to be set by hand for 112 commands, and a partially-correct one is WORSE than none: it puts a safety guarantee behind an unreliable classification. What makes it worth doing is the second half — a structural guard that no command outside the declared write set can reach a mutator, in the shape of `every_spawn_declares_whether_it_is_per_env`. That is the part that catches "someone added a write and forgot the flag", and it is the part a script can't fake. Needs its own session with the registry open, not a slot in a batch.
  Original note: roughly two-thirds of the write surface is unpinned by the property test. Every omission does reach `deny_write` today — verified 2026-08-22 by checking all 40 mutating call sites: 5 sit in ungated functions (`spawn_batch_*`, `spawn_ssm_run_impl`) and all 5 are gated by their callers (`deny_write_batch` at queue time in `cmd_write.rs`; `spawn_action`'s `is_read_only_for` for SSM). Coverage gap, not a live bypass. rather than derived from the registry, so roughly two-thirds of the write surface is unpinned. Every omission does reach `deny_write` today (checked), so this is a coverage gap rather than a live bypass; `CommandSpec` needs a `write` flag to derive from.

#### Supply-chain + API gates — 2026-08-22

Three gates added to CI. Two of them found something on the first run, which is the argument for having them.

- [x] **`cargo-deny`** (advisories / licences / bans / sources). Nothing had ever checked 61 dependencies against RUSTSEC. First run: six advisories, one yanked crate, three licence rejections.
  - The licence rejections were **my policy being too narrow** — BSL-1.0, CDLA-Permissive-2.0 and BlueOak-1.0.0 are all permissive and compatible; added with a note that each earned its place from a real dependency rather than being pre-loaded.
  - `cargo update` cleared the yanked `spin` and one `h2` copy. It also **broke the build**: `serde_yml` 0.0.12 → 0.0.13 changed `Value::get` to take `&str` and changed mapping iteration to hand out the key as `&str` — two breaking changes inside a *patch* release. Four call sites in `lint.rs` got simpler as a result. It also stopped reading `""` as a null document, which broke the EB CLI config parser; that now states "empty means no settings" itself rather than depending on a YAML library's null handling — the same fix shape as the tfstate parse earlier today.
  - The six surviving advisories are waived with **dated, individually justified** entries: five are transitive through the AWS SDK's rustls 0.21 / hyper stack or ratatui's `paste`, none reachable from anything ebman does, all fixed upstream or harmless. A gate that always fails gets ignored, so the exceptions are in the open with reasons rather than the threshold being loosened.
- [ ] **Migrate off `serde_yml`** — RE-SCOPED 2026-08-23 after looking properly. Two findings changed the shape of this:

  **It was nine consumers, not one.** The entry said "only remaining use is `saved_config.rs`". In fact five more were live, and **four of them were JSON being parsed by a YAML parser** — including `parse_baseline`, whose own error message says "baseline JSON parse failed", and three round-trip tests asserting output *is valid JSON* while reading it with a YAML reader that accepts things JSON rejects. All five moved to `serde_json` (already a direct dependency), so the surface is now exactly two files: `saved_config.rs` (EB saved configurations) and `eb_cli.rs` (`.elasticbeanstalk/config.yml`) — both genuine YAML. The `json_surfaces_are_parsed_by_a_json_parser` guard was scoped to the two files I happened to be editing when I wrote it; it covers all five now.

  **There is no obviously-right replacement**, which is why this stays open rather than being done today. Every serde-integrated YAML crate in the ecosystem is stale: `serde_yaml` deprecated (Mar 2024), `serde_yaml_ng` last released May 2024, `serde_norway` Dec 2024. The only actively-developed option is `saphyr` (released 2026-08-18) — but it is 0.0.x and is a parser rather than a serde integration, so it means hand-writing the deserialisation for both files rather than swapping a crate name. That is a trade-off with no clear winner, so it wants a deliberate decision, not a drive-by.

  Meanwhile the waiver's blast radius is two files instead of nine, and neither parses anything an attacker supplies — EB writes the saved configs, the EB CLI writes the other.
- [x] **`cargo-semver-checks`** on pull requests. ebman is lib + bin on crates.io, so a signature change in the lib decides whether the next tag is a patch or a minor — 0.30.2 shipped one (`ui::series_anomaly_label` gained a parameter) that a human review caught, which is not a thing to notice by reading.
- [x] **Least-privilege workflow permissions.** Four open CodeQL alerts, one per CI job (`actions/missing-workflow-permissions`) — jobs inheriting the repository default rather than declaring what they need. `ci.yml` is `contents: read` throughout; `release.yml` drops from a blanket `contents: write` to read, with only the `publish` job opting up, since `build` uploads via `actions/upload-artifact` and `crates_io` authenticates with its own token.

#### Minor (batchable)
- [x] control.sock chmod-after-bind race — CLOSED by the SO_PEERCRED check on every connection (`control.rs`), which is stronger than file perms and needs no process-global umask change. Confirmed 2026-08-22; entry was stale.
- [x] 0600 perms on audit.log / ebman.log / crash logs / explain cache — done in 0.27 via `open_append_secure` / `write_secure`. The gap that remained: `write_atomic` (shared crate) used `std::fs::write`, so `config.toml` — which carries `notify_webhook` and `accounts.*.external_id` — was umask-default. Shadowed locally 2026-08-22 with the mode set on the temp file, not chmod'd after the rename.
- [x] audit-line escaping — `target=` / `version=` already went through `field_token`; the gaps were `append_lint_fix`'s four raw fields, `region=` in both raw writers, and the header's `profile=` (free text from `~/.aws/config`, and `\t` is the separator). All escaped 2026-08-22.
- [x] report_bug scrubber mojibake — ALREADY FIXED; verified 2026-08-22. `scrub_12_digit_numbers` walks chars. The one remaining `byte as char` is inside `url_encode`, where the byte has just been matched against the ASCII unreserved set — correct there.
- [x] ascii icon-mode stragglers — FIXED 2026-08-22. Five sites, not six: `ui.rs:3458` turned out to be `separator_glyph`, which already had an `IconStyle::Ascii` arm — my scan flagged its `_ =>` line for having no ascii context within three lines. The real five were the header delta arrows, the sort marker, and the Metrics anomaly badge, which had `▲` baked into its *message string* where a grep for glyph helpers would never find it. `series_anomaly_label` takes an `IconStyle` now. Guarded at both levels: the helpers in `ui.rs`, and a rendered frame in ascii mode carrying no `▲`/`▼` at all — because the pure helpers can be right while a call site still hardcodes.
- [x] drift redaction — ALREADY FIXED in 0.27 (`redact_drift_fields` reaches `ebman drift` text and `--json`, the MCP tool, and the TUI overlay); entry was stale. A guard test now names the three call sites so a fourth consumer can't skip it. 2026-08-22.
- [ ] **Minor bugs — verified 2026-08-22.** The old one-line batch of ~19 was checked item by item against current code; **eleven were already fixed** by the 0.29/0.30 work and are struck below. What survives:
  - [x] **Detail Logs tab scroll is unclamped upward** — NOT A BUG; my own verification was wrong. `scroll_apply` clamps only at 0, but the Logs call site in `detail_nav.rs` already clamps at the total line count. Checking the helper in isolation instead of its call site is the same mistake the `WRITE_COMMANDS` walk made.
  - [x] **`run_shell_command` doesn't chunk >50 instances** — FIXED 2026-08-22. Sends in chunks of 50 and keys the poll loop on a per-instance command id, since there is now more than one. A failed chunk no longer discards the successful ones: those instances come back as `SendFailed` rows by name, which is strictly more than the operator got before.
  - [x] **`derive_dlq_url` guesses** — FIXED 2026-08-23 via `DlqOrigin` + `dlq_absence_note`; see the 0.31.1 changelog. Original note follows.  — `format!("{trimmed}-dlq")`, right for the EB convention. Downgraded on inspection: a wrong guess IS detected (`NonExistentQueue` on the derived URL resolves to "no DLQ" rather than an error), so the gap is only that the operator isn't told the difference between "no DLQ configured" and "we guessed a name and it wasn't there". Observability, not correctness.
  - [x] **EBL010's tag-fetch failure is indistinguishable from "no tags"** — FIXED 2026-08-22. `env_tag_keys` is `Option<&[String]>`: `None` is "not loaded" and skips, `Some(&[])` is a successful fetch of an env with NO tags and fires for every required key — that env was invisible to the rule, and it is the worst case the rule exists to catch. Matches the `Option` shape its neighbours `dlq_depth` and `healthy_instance_count` already used.
  - [ ] **Unicode display-width column math** — WONTFIX unless something demands it. `pad_right` counts chars, so wide/combining characters misalign — but it is used in one place, the rollout overlay's region / env / version columns, and EB constrains env names to alphanumerics and hyphens. The only realistic non-ASCII input is an operator-chosen version label. Adding a `unicode-width` dependency for a cosmetic misalignment in that case is the wrong trade; recorded so the next reader doesn't re-derive it.
  - [x] **JSON was parsed by the YAML parser** — FIXED 2026-08-22, and wider than recorded. Three inputs ebman does not control went through `serde_yml` on the reasoning that JSON is a YAML subset: both LLM response bodies (carrying model-generated text) and `terraform.tfstate` (discovered by walking up from cwd). True but beside the point — it means every YAML feature, anchor/alias expansion included, applies to that input. `serde_json` had been a direct dependency the whole time, so the comment justifying the detour was stale as well. Two round-trip TESTS had the same hole in reverse: they verified a JSON writer with a YAML reader, which accepts output JSON would reject. A guard pins the parser choice per file; `saved_config.rs` stays on YAML because EB saved configurations genuinely are YAML.
    Behaviour change worth knowing: an empty `terraform.tfstate` is no longer valid input. It was a null YAML document deserialising to `resources: []`, so an empty or truncated file read as "no envs" and passed `drift --exit-code` green. It now reports "no terraform.tfstate found" — same class as the 0.27 fix for backend pointers parsing as zero envs.
  - [x] **The three "needs a look" items were all already fixed** — verified 2026-08-22, each at its call site rather than by reading a helper:
    - *saved-configs window* — `draw_saved_configs_interactive` counts group headers inside the window and re-windows once, with the single pass justified (a second could change the header count by at most one row, which is one row of cosmetic overshoot in a popup). Sound.
    - *DLQ opening with no row selected* — the message handler selects row 0 on a fresh load; its comment names the exact symptom (`unwrap_or(0)` masking `None`, leaving Enter/x/r inert and the first `j` skipping to row 1).
    - *help-restore ghost states* — `apply_rebuild` clears `help.pre_mode` / `pre_overlay` on a context switch, naming the ghost state it prevents. And the `pre_overlay` tail routing is NOT dead: it is the second slot that keeps log-tail events from being lost while help is open over the tail.
  - Fixed since the line was written (confirmed in code, not assumed): `p` purge armable from Main view; `DlqMessages` carrying queue identity; watchdog rollback against a vanished env (disarms with a message); MCP `fleet_cost` NaN (`is_finite` guard in `cost_cache`); `versions --json` empty created date (emits `null`); `ebman envs` unknown flags and typo'd subcommands (both exit 2 with usage); `audit --tail` two text formats (one render path); `rollout --regions` dedupe; `project.rs` silent drop (warns to log + stderr); watch interval drift (start-to-start); MCP `id: null`.

Also queued from the 0.26 pre-tag architecture review: rewrite_credential_error + probe helpers out of app.rs; ui.rs submodule split; MCP registry unification (gate on v2 writes); EBL015 warnings surface in MCP; per-tool client dedup.

#### Console parity — BONUS
- [ ] **`:custom-platform-create <packer-config>`** — SKIPPED this run (2026-07-15): needs S3-bundle upload plumbing + minutes-scale `CreatePlatformVersion` polling with more than one reasonable shape (fire-and-forget vs poll), all unverifiable against live EB here. Was tagged "fine to slip to 0.26" — it slipped.

### Feature candidates — competitive scan (2026-05-24)

Ten new ideas surfaced by a backlog/peer-TUI review after the 0.7.0 ship. Ordered roughly by operator-value-per-hour. None overlap with already-tracked items; the niche items already on the backlog (custom-platform create, topology graph, Route 53, etc.) stay where they are. Sized for a 0.9 batch — pick from the top.

- [x] **`:diff env-A env-B`** — Done (2026-05-24). Discovery: `:diff ENV` already existed (single-arg, selected-vs-arg, structured `Overlay::Diff` via the existing `diff_envs` renderer covering Name / App / Tier / Status / Health / Platform / Version / CNAME / Updated). The right shape was to extend that arm to also accept two args, not to add a parallel command — so the dispatch at `src/app.rs` now matches `(rest.first(), rest.get(1))` and routes the two-arg form to a path that names both envs explicitly with no selected-env fallback. Same-env-twice gets a clear "pick two different envs" error rather than silently comparing an env against itself (added to the single-arg form too as a small UX win). +3 tests (two-arg happy path, same-env rejection, unknown-env error). Help text + commands-registry description updated. **Scope note**: the BACKLOG entry originally suggested combining the env-metadata diff with the option-settings diff in a single overlay — that's a separate UX change to the overlay surface (would touch `Overlay::Diff` + `draw_diff_overlay`), not the "name both envs" change this entry described. Operators who want both diffs today run `:diff A B` then `:config-diff` separately. A combined view can be a follow-on if it's actually wanted.
- [x] **`:ssh [i-abc]`** — Done (2026-05-24). New `cmd_ssh` routes to the existing `pending_shell_target → open_embedded_shell` machinery (the same flow as pressing `s` on Detail/Instances), so the TUI-suspend/resume + alt-screen dance is shared code. With an arg, the instance ID is validated to start with `i-` (refuses typo'd env-names that would otherwise produce an opaque CLI error). No-arg form opens a new `PickerKind::SshInstance` populated from cached `Detail.instances` — if Detail isn't open with the Instances tab loaded, surfaces a clear error pointing the operator at the precondition rather than silently no-op'ing. **Scope note**: the BACKLOG entry originally also asked for `:ssm-run "<cmd>"` (cross-instance command runner via `ssm:SendCommand` + polling). That's a separate (bigger) feature — needs new SDK calls, polling state, and a multi-instance result aggregator. Tracked separately below.  +3 tests (arg happy path, typo'd arg rejection, no-arg-without-Detail error). Existing infrastructure used: `open_embedded_shell` (live), `run_inline_ssm` (kept dead-code as the "drop out fully" reference).
- [x] **`:ssm-run "<cmd>"`** — Done (2026-05-24). New `aws-sdk-ssm = "1"` dep, `SsmClient` wired alongside ACM / Secrets / IAM (region-scoped). `AwsClient::run_shell_command(instance_ids, command, wall_clock_secs)` fires `SendCommand` with `AWS-RunShellScript`, then polls per-instance `GetCommandInvocation` every 2s (matches `run_insights_query`'s cadence). Each invocation reaching Success / Failed / Cancelled / TimedOut drops out of the wait set; instances still pending after the wall-clock get a synthetic `TimedOut(local)` row so the operator sees which ones didn't finish. Results sorted by instance ID for determinism. `cmd_ssm_run` in app.rs reads target IDs from cached `Detail.instances` (same source as `:ssh` no-arg), strips surrounding quotes from the joined command tokens, gates via `deny_write` (treats SSM as a write because a shell command can mutate state), and lands the aggregated body via `format_ssm_results` — per-instance section headers `─── id [status, exit=N] ───` then `stdout:` / `stderr:` blocks, with 50-line + 200-char-per-line truncation so a verbose command doesn't blow out the overlay. Hard 60s wall-clock cap to keep the TextOverlay from hanging. +5 tests cover renderer happy path / empty stub / output truncation / no-args usage / no-Detail guidance. **Scope notes**: not adding a `--timeout` flag (60s default + SSM's own server-side TimeoutSeconds covers the read-probe use case); not following `standard_output_url` / `standard_error_url` for >24KiB outputs (operator can pipe to `head`/`tail`); not adding a multi-instance picker — `:ssm-run` always fans across all cached instances, just like the BACKLOG entry described.
- ~~**`:upgrade`**~~ Withdrawn (2026-05-24). The existing `:update` (`src/app.rs:9168`) carries an explicit design comment against auto-upgrade: "Doesn't actually upgrade — operators on AWS-touching tools prefer conscious upgrades, and self-replacing the binary across Cellar / cargo-bin / tarball layouts has too many platform footguns." That decision predates this BACKLOG entry; the entry was written without checking. `:update` already detects the install channel and yanks the right `brew upgrade ebman` / `cargo install ebman --force` command to the clipboard, so the gap is just "paste vs press enter." Not worth pushing against the existing design call without a fresh prompt.
- [x] **Cost overlay per env** — Done (2026-05-24). `app.costs: HashMap<String, f64>` is already populated by `:cost on` (Cost Explorer fan-out cached at `~/.cache/ebman/cost-{account}-{region}.toml`). Surfaced in two places: (a) `:why` overlay — new top-of-overlay row right after the runbook line, format `$NN/mo` with the same green/muted/red bucket palette as the envs-table COST column; (b) Detail/Health status line — appended as a `cost: $NN/mo` chip alongside status/health/DLQ so spend lives in the same scanline as health. Both sites no-op when `app.costs` is empty (operators who haven't enabled cost tracking see unchanged layout). No new state, no new fetch, no new dependency — pure rendering over the existing cache. Unit format is monthly (`/mo`) not hourly as the BACKLOG entry suggested — matched to what Cost Explorer actually returns + what the COST column shows, consistency wins. **Scope note**: bucket-threshold logic is now duplicated 3 sites (envs table / `:why` / Detail Health). Considered extracting `cost_bucket_color(cost, theme)` but the 3-module reach + the obviousness of the thresholds make the helper a wash. Worth revisiting if a 4th site shows up.
- [x] **Local config diff against `.elasticbeanstalk/saved_configs/*.cfg.yml`** — Done (2026-05-24). Took the YAML dep call — added `serde_yml = "0.0"` (actively-maintained successor to the archived serde_yaml). New `src/saved_config.rs` module: `parse_saved_config(yaml) -> Vec<ConfigOption>` walks the `OptionSettings: {namespace: {name: value}}` nested map and emits the same shape `fetch_env_configuration_options` returns, with YAML scalar coercion (`true` → `"true"`, `4` → `"4"`, `'4'` → `"4"`) so the diff stays consistent across quoted-vs-unquoted forms; `discover_saved_configs(cwd)` walks up to `.elasticbeanstalk/saved_configs/`, returning paths alphabetically sorted; `saved_config_name(path)` strips `.cfg.yml` / `.yaml` / `.yml` suffixes for the operator-facing name. New `:config-diff-local [NAME]` command in app.rs: no-arg auto-picks if there's exactly one saved config (lists names when there are multiple so the operator can rerun with one); reuses `diff_config_options` + `render_config_diff_overlay` so the diff UI is identical to `:config-diff`. +7 tests cover parse happy path / unquoted scalar coercion / missing-OptionSettings / garbage YAML / name extraction / discovery walk / empty-dir-returns-empty. **Scope notes**: read-only operation (no `:config-apply-local` to push the local YAML to the env — that's a separate destructive feature that needs its own confirm flow); also doesn't show env metadata diff (Description / Platform / Tags) — only OptionSettings, which is what operators actually diff.
- [x] **`:lineage`** — Done (2026-05-24). New `cmd_lineage` reuses the `list_events_for_env(_, 100)` fetch already used by `:changes` / `:rollback`, filters events that carry a non-empty `version_label`, and collapses consecutive same-label events into one row (one deploy generates multiple events: started / instance OK / env update completed). Pure `build_lineage(events) → Vec<LineageRow>` does the collapse + ordering (newest-first); pure `format_lineage(env, events)` renders the overlay with the deploy's span (`took`) and gap to the next-older deploy (`Δ since previous`). +3 tests cover collapse / version_label filter / span+gap rendering. Empty event window produces a stub matching the `:changes` style. **Scope note**: 100-event window same as `:changes` — high-frequency-deploy envs may need a deeper window; defer until anyone hits the cap.
- [ ] **`:queue` action-queue inspector** — Builds on `:pending`. Show currently-dispatched + recently-completed writes across *all* envs (not just selected), with per-row abort for cancellable ops (best-effort; most EB writes aren't cancellable but the dispatch ack can be discarded). Useful when running batch ops — operator sees what's still in flight without scrolling event tape. **Held (2026-05-24)** — `:pending` already shows the same data globally (iterates `self.pending_actions` across all envs). The genuinely new piece would be per-row abort, but most EB writes (UpdateEnvironment, deploys, restarts) aren't cancellable server-side — only the local dispatch ack can be dropped, which limits the operational meaning of an "abort" action. Without abort, `:queue` collapses to `:pending --in-flight` (one line of filter logic). Defer until the abort semantics are designed honestly.
- [x] **Saved views as tabs (gh-dash style)** — SHIPPED (2026-05-26, 0.12). Unified `named_filters` + `saved_views` into a single store (`App.saved_views`). `]` / `[` now cycles full views — filter+sort+group+scope all apply together. Chip bar at the top of the main view reads from `saved_views`. `:filter NAME` / `:save NAME` / `:drop NAME` / `:filters` all operate on the unified store with the filter-only encoded form; `:save-view NAME` / `:view NAME` / `:view-drop NAME` / `:views` use the same store with the full encoded form. Legacy `filter.NAME = "..."` lines in `state.toml` auto-promote into `saved_views` on first load using the filter-only encoding; explicit `view.NAME` wins on collision. First save after upgrade drops the legacy `filter.*` output. Pure helpers `encode_filter_only_view` + `view_filter_value` unit-tested. **Scope note**: the original BACKLOG framing imagined a structured `SavedView { filter, sort_key, sort_desc, grouped }` struct — the encoded-string form already shipped as part of `:save-view` does the same job and avoids the schema-migration scope.
- ~~**Profile / region quick-chord**~~ Withdrawn (2026-05-24) — already shipped, just not as Ctrl chords. `p` and `r` (plain keys in Normal mode at `src/app.rs:3311-3312`) open the Profile / Region picker overlays directly. Better than the Ctrl chords the BACKLOG entry proposed: no modifier required, and `Ctrl-R` would have clashed with the existing manual-refresh keybind anyway. The BACKLOG entry was written without re-grepping the existing keybinds — closing the loop honestly.
- [x] **CloudWatch alarm state timeline** — Done (2026-05-24). `:alarm-history NAME` fetches up to 50 entries via `cw:DescribeAlarmHistory`, surfaces them as a TextOverlay newest-first with timestamp + kind (`StateUpdate` / `ConfigurationUpdate` / `Action`) + summary. New `AlarmHistoryEntry` struct in `aws.rs` (at / kind / summary), new `fetch_alarm_history(alarm_name, max_records)` method on `AwsClient`, new `cmd_alarm_history` in `cmd_alarms.rs`, pure `format_alarm_history(alarm_name, entries)` in `app.rs`. Empty result shows the 90-day-retention hint so the operator knows whether the fetch succeeded. +2 tests (rendered entries / empty stub / missing timestamp). **Scope note**: the `H`-on-alarms-list-row drill-in keybind is deferred — the alarms-list overlay would need to become interactive (it's currently a static `TextDump`), which is a different piece of UX work. Command-from-`:` works today.

### Top priority — console-parity + peer-TUI polish (2026-05-21)

Surfaced by a critical console-vs-ebman + ebman-vs-peer-TUI comparison. Ranked by user-value-per-hour. The smaller ergonomics items in particular (autocompletion, did-you-mean, first-run hint) are the gap that makes ebman look unpolished next to k9s / lazygit — high impact, low cost.

- [x] **`:options` — full settable-option vocabulary with current values** — Done (task #113). Two-call merge of `DescribeConfigurationOptions` (vocab/metadata) + `DescribeConfigurationSettings` (current values) keyed on `(namespace, name)`. `▸` operator-set / `•` default; emits `value_type` / `change_severity` / range / enum-options when EB returns them. Optional `NAMESPACE` arg filters.
- [x] **`:` autocompletion against `commands::COMMANDS`** — Done (task #114). Tab cycles forward, Shift-Tab cycles back; origin fragment cached on first press so repeated cycling restores the prefix cleanly.
- [x] **"Did you mean?" on unknown commands** — Done (task #115). Levenshtein against `commands::all_names()`, threshold 2.
- [x] **First-run nudge** — Done (task #116). `state::file_exists()` gate sets `first_run_hint`; sticky footer row hints at `?` / `:` / `Ctrl-K` until first input.
- [x] **Resource topology as hierarchical text** — Done (task #117). Indented ASG → instances → ELB → TGs (Worker tier shows ASG → instances → queue). Pure `render_env_resources_tree`.
- [x] **`:explain` IAM diagnosis** — Done (task #118). `:explain` no-arg scrapes the last `AccessDenied:` toast; `:explain ARN ACTION` evaluates explicit pairs via `iam:SimulatePrincipalPolicy`. Surfaces SCP / permissions-boundary blockers when the simulator flags them.

**Secondary** (same review, smaller payoff or design call needed):

- [x] **Form-based edit for the long tail of namespaces** — Done (task #119, 0.6). The "top-3 namespaces still need forms" premise had drifted: by 0.6 nearly every config family already had a dedicated command/form — `:capacity` (ASG), `:rds-attach`, `:listener-edit`, `:env-edit` (env vars), `:logs-stream`, `:notify`, `:managed-window`, `:deployment-policy`, `:rolling-update`, `:health-check-url`, `:subnets`, `:keypair`, `:service-role`, … — and the genuine remainder (`proxy`, `healthreporting`) is 1–2 settings each, well served by `:set-option`. The one real multi-field gap was metric-based autoscaling: `:scaling-triggers` is now a 9-field modal form over `aws:autoscaling:trigger` (metric / statistic / unit / period / breach duration / lower+upper thresholds / scale increments), pre-filling the env's current trigger.
- [x] **Config tab in-place editor — key rename** — Done. `r` on the Config tab opens an in-place editor for the row's *key*; commit dispatches set-new + remove-old in one `UpdateOptionSettings` / `UpdateTags` call, carrying the value across. `ConfigEdit.is_new: bool` refactored to a `ConfigEditMode` enum (`Value` / `NewRow` / `RenameKey`). The Config-tab editor now has every section: cursor nav, value edit, add, delete, rename, scroll-follow.
- [x] **Per-tab help-density polish** — Done (task #120). The Detail footer key strip is now structured `(key, label)` pairs (`detail_tab_keys`) rather than a flat string; `render_detail_keystrip` renders keys bold + bright against muted labels, separated by a thin `·`, so each pair is scannable without extra width. Global keys (`tab` / `?` / `esc`) are appended uniformly by a shared `DETAIL_GLOBAL_KEYS` const, fixing the prior inconsistency where only some tabs advertised tab-cycling. A drift test asserts no tab lists a key twice. +3 tests.
- [ ] **Mouse: column resize via drag + right-click row menus** — PARTIAL: drag already exists for the events-panel divider (`input.rs` `drag_origin`), so the interaction pattern is proven; what's missing is table COLUMN resize and right-click menus. Wheel + click-to-select is the current floor. Operators coming from console expect drag + right-click. TBD whether this is worth the design cost for a primarily-keyboard tool.
- [x] **Per-env runbook hint** — Done (task #121, 0.6). Config-file map, not a CLI command, as floated: `runbooks.ENV = "https://…"` lines in `config.toml` parse into `Config.runbooks` / `App.runbooks` and round-trip through `serialize` (so `:settings` save preserves them). The `:why` triage overlay shows a bold `runbook  <url>` line at the top when the selected env has one. +2 tests (parse incl. blank-URL skip, serialize round-trip).

### UI polish — deferred candidates (2026-05-20)

Proposed during the Powerline-aesthetic pass but skipped because the cost / payoff was marginal vs. the rest of the surface. Easy to pick up if the visual surface gets another pass.

- [ ] **TIER / STATUS pill caps in env table (option A)** — every row's pills get a Powerline trailing wedge so they read as ribbon-style tags. ~~Blocker: TIER column is `Constraint::Length(7)`~~ — STALE: TIER is `Length(11)` now and `pill_chain` already renders wedge-capped pills in the header, so the machinery exists and the width objection is gone. What remains is the design call about applying it per-row. Old note: and the existing `" Worker "` pill is already 8 cells; STATUS column is 10 and `" Terminating "` is 13. Caps would overflow more rows. Revisit if/when the table column widths get widened — or render the cap *only* when the cell has room.

### Console parity — write-side gaps (operators currently open the console for these)

Gaps surfaced during the 2026-05-19 console-vs-ebman comparison. Each entry is a console feature with no ebman equivalent. Ordered by daily-operator frequency.

- [x] **Attach / detach RDS database** — Done (tasks #109 + #110, 0.6). `:rds` (2026-05-21) reads the env's `aws:rds:dbinstance.*` option settings (DBPassword redacted). `:rds-attach` is a 7-field modal form (engine / class / storage / master user+password / deletion policy / Multi-AZ) over `aws:rds:dbinstance`, pre-filling if a DB is already attached. `:rds-detach ENV` "safe-ifies" the coupled DB — sets `DBDeletionPolicy=Snapshot` so it survives env termination, behind a typed-name confirm (the `ENV` arg must repeat the env name). **Scope reality:** Elastic Beanstalk has *no* detach operation — an EB-created RDS instance lives in the env's CloudFormation stack and true decoupling needs an env rebuild; `:rds-detach` makes the data safe to keep, it doesn't move it (command help + toast say so). The separate immediate `rds:CreateDBSnapshot` from the original sketch was dropped: it needs DB-instance-id discovery via CloudFormation stack introspection plus an `aws-sdk-rds` dependency, neither verifiable here — and `DBDeletionPolicy=Snapshot` already guarantees a termination-time snapshot. Could be revisited if a point-in-time backup *before* termination is wanted.
- [x] **ALB listener + TLS cert config** — Done (tasks #108 + #111, 0.6). `:listeners` (2026-05-21) reads the env's `aws:elbv2:listener:*` namespaces grouped by port. `:listener-edit PORT` is a modal cert-rotation form: a single MultiSelect field whose options are the region's ISSUED ACM certificates (loaded live via a new `aws-sdk-acm` dependency + `acm:ListCertificates`), pre-selected with the listener's current `SSLCertificateArns`; submit writes the new cert set to `aws:elbv2:listener:<PORT>` through the option-settings path. Scope notes: delivered as a command (`:listener-edit 443`), not a Detail "LB tab" — a whole new tab was disproportionate to the feature. Protocol / SSLPolicy / ListenerEnabled / rules stay on `:set-option`; the form is scoped to cert rotation, the dominant edit. The ACM call shape is unverified against a live account (the SDK compiles against it).
- [x] **Capacity profile beyond min/max + instance type** — Done. `:capacity` modal form (MinSize / MaxSize / InstanceType / Cooldown) shipped in 0.3.0; `a → Capacity` menu entry shipped in 0.3.1. Multi-instance-type / spot-base / scheduled-scaling fleets still missing but those are niche enough to drop from this list — operators using them are mostly EB CLI / Terraform users.
- [ ] **Custom platforms — create** — delete shipped as `:custom-platform-delete <arn>`. Create still missing: console offers a wizard that builds a new custom AMI from a Packer template (slow — minutes — needs polling); ours would be `:custom-platform-create <packer-config>` via `elasticbeanstalk:CreatePlatformVersion`. Niche but a real gap for operators who maintain in-house base AMIs.

### Tier 6 — power-user / scripting
- [ ] **Embedded recorder** — record + replay sessions to `.cast` (asciinema). Deferred — needs its own input-capture + replay infrastructure.

### Tier 8 — maybe / unprioritised
- [ ] **Snapshot at a point in time** — "what envs looked like 1h ago" (would need local history).
- [ ] **Visual resource topology graph** — console shows a "Resources" graph linking ASG → EC2 instances → ELB → target groups. We have `:resources` as a text dump which most operators prefer; the graph is nice-to-have but rarely the reason someone opens the console.
- [ ] **Route 53 / custom DNS integration** — console offers a one-click "set up custom domain" wizard tied to a Route 53 hosted zone. Niche and easy to do via AWS CLI or the Route 53 console directly.

## Skipped — needs retry

Populated by autonomous runs per `CLAUDE.md` stop-conditions. Each entry: one-line reason. Drop the entry once retried (successfully or with the user's deliberate decision to defer further).

- **Embedded asciinema recorder (Tier 6)** — needs its own input-capture/replay infrastructure; defer.
- **`:custom-platform-create` (0.25 BONUS)** — S3-bundle upload plumbing + minutes-scale CreatePlatformVersion polling with multiple reasonable shapes; unverifiable against live EB in an autonomous run. Slipped to 0.26 as the lineup anticipated.
- **EBL015 / EBL018 (0.25 lint batch)** — each needs new AWS surface (per-platform DescribePlatformVersion dates / aws-sdk-wafv2 GetWebACLForResource); recorded in docs/lint-rules.md roadmap with reasons.

**Retried successfully** (kept here briefly so the history's discoverable):

- **README screenshots / demo gif** — rendered 2026-06-04 from an interactive session (`vhs demo.tape`), so the no-TTY blocker no longer applies. The fixture was reskinned to the PROJECT IRONWOOD world (`poly` fleet + the Grey `ironwood` env on a distinct Go platform); see the demo-lore Done entry above.
- **Option settings editor** — shipped in 0.3.0 (`:env`, `:set-option`, `:capacity` modal, every per-namespace command).
- **Split `src/app.rs`** — shipped as task #66 (ten `cmd_*.rs` sub-modules); app.rs 14,277 → 12,478.
- **`sts:AssumeRole` account switcher** — shipped in 0.3.0 (`accounts.NAME.role_arn` config + `:account NAME` switcher). [[multi-account-discovery]].

---

## Dropped / explicitly out of scope

- Multi-service AWS dashboard (RDS / ECS / Lambda). Stays out of scope — ebman is EB-focused on purpose; generic-AWS TUIs already exist (clawscli, cloudlens) and sprawl.
- `Ctrl-N` to dismiss alert badge. Removed when alerts switched from "transitions since last ack" to "currently Red".

---

## Notable inspirations

- **[e1s](https://github.com/keidarcy/e1s)** — same problem shape (k9s-for-ECS). UX template; `b` console deeplink and `d` describe overlay come from here.
- **[k9s](https://github.com/derailed/k9s)** — original model. Resource aliases, `:` command bar, drill-down.
- **[stu](https://github.com/lusingander/stu)** — Rust + ratatui S3 explorer; same stack idioms.
- **[gitui](https://github.com/gitui-org/gitui)** — ratatui async patterns under load.
- **[lazydocker](https://github.com/jesseduffield/lazydocker)** — panel + tab metaphor mirrors our drill-down.
- **[lazygit](https://github.com/jesseduffield/lazygit)** — per-panel hint strip, contextual action menu.
- **[gh dash](https://github.com/dlvhdr/gh-dash)** — sectioned dashboards inspired the "env groups as tabs" idea.
- **[bottom](https://github.com/ClementTsang/bottom)** — ratatui dashboard widget patterns; Metrics tab follows this.
- **[harlequin](https://github.com/tconbeer/harlequin)** / **[atuin](https://github.com/atuinsh/atuin)** — fuzzy-find UI patterns for filtering long streams.
- **[tig](https://github.com/jonas/tig)** — paged event-log + ref panel for timeline views.
