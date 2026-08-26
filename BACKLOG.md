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

- [x] **`HelpTopic::Shell` is never constructed, so `draw_help_shell` is
  unreachable** — DONE 2026-08-25. Reached by name: `:help <topic>` takes
  any of `global`, `detail`, `dlq`, `action`, `shell`, `saved-configs`,
  case-insensitively; a name no topic answers to reports what was typed
  and what is available rather than opening whatever the inference would
  have picked. Bare `:help` / `?` still infer from context — the arg form
  is additive.

  `:help shell` is the right shape for this particular screen rather
  than a workaround. What it documents is how to get *out* of the shell
  (`F12` detaches, `^D` closes), which is only useful before you attach —
  and unreachable after, since `?` is a legitimate globbing character
  that belongs to the subprocess.

  Two things fell out of it. The type-level `#[allow(dead_code)]` on
  `HelpTopic` is **gone** — it existed to hide this one dead variant, and
  with `Shell` live nothing in the enum is dead, so the lint is back on.
  And `every_help_topic_renders_something_of_its_own` was assigning
  `app.help.topic` directly, so it had been proving the Shell renderer
  worked down a path production could not take — precisely the
  "test a data structure the production path doesn't reach" case. It
  drives `:help <topic>` now, and iterates `HelpTopic::ALL` with an
  exhaustive `match` for the titles, so a new topic must declare one.

  Guarded: `every_help_topic_variant_is_in_all` reads the variants out of
  `types.rs` and requires each to be in `HelpTopic::ALL`, since a variant
  missing from `ALL` cannot be named and would sit unreachable exactly as
  `Shell` did. Mutation-verified both ways — dropping `Shell` from `ALL`,
  and deleting the named-topic branch from dispatch: CAUGHT.

- [x] **`DeploySnapshot::env_name` is written and never read** — DONE
  2026-08-25, **field removed**, which is the opposite of what this entry
  proposed. The entry said it was "what makes a persisted line
  self-describing in state.toml". It isn't: `to_persisted` emits
  `"label|RFC3339"` and nothing else. The env name is the TOML key —
  `deploy_snapshot.prod-api = "build-823|…"` — so the line is
  self-describing with or without the field, and the field was a third
  copy of a string the map key and the file key already carry. Nothing
  read it anywhere.

  `parse_persisted` lost its `env_name` parameter with it: it was taking
  a name only to store it.

  What the entry was actually protecting — a hand-read `state.toml` line
  saying which env it belongs to — had no test at all, so it has one now,
  and getting there turned up two more things:

  - **`save()` had no pure writer.** `CLAUDE.md` requires `parse` to be a
    pure function so it stays unit-testable; the writer never got the same
    treatment, so the body was built inline against the filesystem.
    Extracted as `state::serialize(&PersistedState) -> String`, with
    `save` now a thin I/O wrapper.
  - **`serialize_deploy_snapshots_round_trips` was testing a copy.** It
    hand-constructed the line it expected `save` to write, so `save`
    could have stopped emitting the key entirely and it would still have
    passed. It drives `serialize` now — and the mutation below fails it,
    which it could not have done before.

  Mutation-verified: making the writer emit `deploy_snapshot = "…"`
  without the env key is CAUGHT.

  Also removed: the type-level `#[allow(dead_code)]` on `DeploySnapshot`,
  which existed only to hide this field.

- [ ] **Whole-tree mutation sweep: triage the remaining survivors.**
  The first complete-ish sweep (2026-08-25, 16 shards, ~95% of the tree)
  produced 2832 caught / 2599 missed — a 52% kill rate on viable
  mutants. Artifacts and the triage list are reproducible by re-running
  the `mutants` workflow.

  Worked so far, all in the write/safety cluster: the `:rollback`
  wrong-env guard, the Terminate type-the-name guard, `AuditFilter`,
  `parse_kv_pairs` boundaries, and the deploy watchdog conditions.

  **Remaining, by consequence rather than count:**
  - `src/app/input.rs` (228) — the keymap. A surviving "delete match
    arm" means a key silently stops working. User-visible, not
    dangerous.
  - `src/ui/*` (~539 across detail / header / overlays / table /
    chrome) — render code; mostly a wrong pixel.
  - `src/aws/eb.rs` (105) — response parsing.
  - the rest of `src/cli/lint.rs` (87) and `src/app.rs` (82).

  Expect a large equivalent-mutant tail: two of the first six
  investigated were equivalent (the byte-identical `AbortUpdate` arm,
  the redundant whitespace skip in `parse_kv_pairs`), and both pointed
  at real duplication rather than missing tests.

- [x] **CLI rollout freeze is start-only** (bugs M3) — FIXED 2026-08-26,
  reversing the recorded "conscious choice" with the maintainer's
  go-ahead. The justification on file was "matches the pin start-gate
  semantics", and that doesn't hold: a pin is static config, a freeze is
  a live incident signal that arrives mid-flight by definition. The
  exposure was real — no cap on `--wait-for-green`, none on region
  count, so a sequential rollout can dispatch its last region hours
  after the single check.

  The objection that halting leaves partial state was not new: the
  rollout already halts mid-way when a region fails under
  `continue_on_fail=false`, and already reports untouched regions as
  `skipped (rollout halted)`. The freeze halt reuses that exact path.

  `rollout_freeze_halt()` returns the reason rather than exiting, since
  a part-way rollout has a report to emit. Both dispatch loops consult
  it — the sequential one between regions, the parallel one before
  reseeding an un-started wave (in-flight regions can't be cancelled
  server-side, so they finish and report normally).

  Two tests, because the first alone was not enough: the behavioural one
  proves the predicate, and deleting a call site left it **green** — so
  a source guard pins that both loops actually consult it. Verified by
  removing each gate in turn: CAUGHT.

  Documented in `docs/safety-and-privacy.md`.

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

#### ARCHITECTURE rule guards — 2026-08-25

The five rules in `ARCHITECTURE.md` are the ones the compiler doesn't
enforce. Rules 4 and 5 had nothing behind them at all; both do now, and
both guards were mutation-verified before being believed.

- [x] **Rule 4 — guarded key arms come first** — DONE 2026-08-25.
  `src/app/tests/key_arm_order.rs` parses the tree with `syn` and
  compares arm positions *within each `match`*. It parses rather than
  greps because judging order requires knowing which `match` an arm
  belongs to: the line-level detector written first reported four
  violations in `input.rs` (`k`, `x`, `d`, `y`) and **every one was
  false** — it compared arms in different `match` blocks and in
  different functions, and misattributed both `'k'` arms to a free
  function at the top of the file. Neither existing mechanism could
  express this rule: source scans can't see scope, and `cargo-mutants`
  deletes bodies and flips operators but does not permute arms. Six
  characters carry both forms today (`r g y ] [ k`) — not the nine an
  earlier grep claimed, which was the same cross-match miscount.
  Mutation-verified by relocating the guarded `Ctrl-y` arm below the
  unguarded `'y'` arm: CAUGHT, with the offending line numbers and a
  fix instruction in the failure message. Adds `syn` (`full`, `visit`)
  and `proc-macro2/span-locations` as dev-dependencies.

- [x] **Rule 5 — the TUI never prints to the terminal** — DONE
  2026-08-25, found while checking whether the rule-4 claim in
  `CONTRIBUTING.md` was true. `src/app/tests/no_tui_stdout.rs` scans
  `src/app`, `src/ui` and `src/aws` for `println!` / `eprintln!` /
  `print!` / `eprint!`, comments stripped. Deliberately *not*
  `src/cli` or `src/main.rs`: the headless subcommands print by design
  (162 sites), and that is what keeps the guard honest — a companion
  test points the same detector at the CLI and requires it to find
  more than 20, so "the TUI is clean" cannot be confused with "the
  detector is broken". A third test asserts the three area prefixes
  still match source, so a module move can't silently empty the sweep.
  Mutation-verified by planting a `println!` in `src/ui/action.rs`:
  CAUGHT.

  Self-review of that guard then found a gap in it: print macros are
  not the only way to reach the terminal, and a `writeln!` into
  `std::io::stdout()` would have sailed straight past. Direct handle
  writes are now checked against an allowlist of `(path, count, why)`,
  with exactly one entry — the BEL byte `spawn_refresh.rs` writes to
  ring the bell on a new red alert, a control character rather than
  display text. The count is part of the pin: file-level granularity
  would let a second, unjustified write shelter behind the first one's
  recorded reason. Verified in both directions — a second write in the
  allowlisted file, and a write in a file with no entry — plus a stale
  entry naming a site that no longer exists.

- [x] **Two defects in the rule-4 guard, found reviewing it** — fixed
  2026-08-25 in the same run. (a) The `KeyCode` qualifier check asked
  whether `KeyCode` appeared *anywhere* in the path, which the `Char`
  segment itself satisfies, so it excluded nothing and would have
  admitted an unrelated enum's `Char(char)` variant. It reads the
  segment immediately before `Char` now, pinned by a test that fails
  against the old form. (b) The tree sweep's non-vacuity floor was
  `>= 2` files when 20 match — loose enough to pass on a walk that had
  collapsed to almost nothing, which is the vacuous shape this codebase
  keeps finding. Raised to 10.

- [x] **Rule 3 — async results check `generation`** — DONE
  2026-08-25, and the premise of this entry was wrong. It was written as
  "the weakest of the five, nothing sweeps for a handler that forgot the
  check". In fact rule 3 is the *best*-defended of the five
  structurally: there is one enforcement point, not a convention each
  handler follows. `AppMsg::generation()` classifies all 59 variants,
  `handle_msg` drops the message once before dispatching, and the match
  is exhaustive, so the compiler forces a new variant to be classified.
  The planned "visit every handler and require a generation comparison"
  guard would have been looking for something that isn't there — the
  handlers correctly don't check, because the router already did.

  The real gap was one level up, and it is the familiar shape: the
  compiler forces you to classify, but the cheapest way to satisfy it is
  to append the new variant to the `None` arm — a one-line change with a
  plausible reason that exempts a whole result path from the invariant.
  That arm was documented and completely untested.

  `src/app/tests/generation_guard.rs` closes it from both ends. The
  structural half needs **no allowlist and no judgement**: carrying a
  `gen: u64` field and being classified `Some` have to agree, because a
  variant that carries a generation and is exempted from the generation
  check is unambiguously wrong. Only the three variants with no `gen` at
  all need a recorded reason (`Rebuild` and `ClientRefreshed` carry
  epochs instead; `UpdateCheck` isn't context-bound), and a reason naming
  a variant that no longer qualifies fails too. A behavioural test covers
  what neither says — whether `handle_msg` still *acts* on the
  classification — with both halves in one test so "stale results are
  dropped" can't pass on a handler that applies nothing.

  Mutation-verified four ways, all CAUGHT: neutralising the early return
  in `handle_msg`; moving a gen-carrying variant into the `None` arm;
  deleting a recorded reason; and adding a stale one.

- [ ] **The wrapped-literal guard can't see the collapsed form** —
  found 2026-08-25 by introducing the defect. `CLAUDE.md` records this
  class shipping three times;
  `no_wrapped_string_literal_leaves_an_indentation_hole` now catches the
  *wrapped* shape (a literal split across lines with no `\`
  continuation, which embeds the newline and the next line's indent).
  It cannot see the same defect once collapsed onto one line — a single
  literal carrying a bare 18-space run mid-sentence — which is exactly
  what a tool-assisted edit produces when something eats the
  continuations. That is how it happened: a Python heredoc treated the
  `\` as its own line continuation and emitted the collapsed form,
  which every existing check passed.

  Not built, because the shape is a real trade-off rather than an
  oversight. Measured over production sources, a run of N spaces
  mid-literal matches: **28** at N=3, **7** at N=6, **3** at N=8, **2**
  at N=12 — and at every threshold the survivors are legitimate column
  alignment (`REGION  ENV  CURRENT  TARGET`, `Tags  loading…`, the
  rollout header in `render.rs`). So the guard needs either a threshold
  that misses shallow holes or an allowlist that grows with every new
  table header, which is a guard people route around. Worth someone
  picking a shape deliberately; not worth guessing one mid-run.

#### MCP Registry — 2026-08-25

- [x] **The registry publish couldn't be retried** — DONE 2026-08-25.
  `workflow_dispatch` exists on `release.yml` precisely so a failed
  publish can be re-run, but `mcp_registry` has `needs: crates_io`, and
  `cargo publish --locked` exits non-zero once the version is already up.
  So every re-run failed at the crates.io step and skipped the registry
  entirely. The job now asks crates.io whether the version exists and
  exits 0 if it does — asked rather than pattern-matched out of the error
  text, because a grep for "already uploaded" swallows whatever else
  cargo might have said, and this is the last gate before a release is
  public.

- [x] **`server.json` had drifted to 0.32.0** — DONE 2026-08-25, with a
  guard. Harmless in effect (the workflow rewrites the version from the
  tag before publishing) which is exactly why nothing noticed for three
  releases, but it is a checked-in file that reads as authoritative.
  Bumped to 0.34.2 and pinned by
  `docs_drift::server_json_version_matches_the_crate`; mutation-verified
  both ways, including the vacuous case where the guard stops finding any
  `version` field at all.

- [x] **The registry listed the yanked 0.34.1 as `latest`** — FIXED
  2026-08-25 21:32 UTC. 0.34.2 is published and is now `latest`, so
  discovery no longer points at the release that packaged `mutants.out/`
  with the machine's hostname and username. Ten versions listed, 0.29.2
  through 0.34.2.

  It took two fixes, not one. The registry being degraded that evening
  (19.6s health check) is why 0.34.2 missed its window; the reason every
  retry since had failed silently was the `needs: crates_io` chain
  above. Health is back to 0.49s. The dispatched run confirmed both:
  `cargo publish` exited 0 on the already-published version instead of
  failing the job, and `publish to MCP Registry` ran for the first time
  on a re-run.

#### Mutation sweep — first complete run, 2026-08-26

Run 32928330276, 03:56–11:04 UTC, against `0adc76a`. **6053 mutants:
2989 caught, 2637 missed, 28 timeout, 399 unviable — score 53.1%.**
Every shard finished under the 350-minute cap (slowest 3h47m), so the
24-way split is right and does not need revisiting.

Aggregated survivor list: `scratchpad/sweep/all-missed.txt` (session
scratch — regenerate from the run's artifacts if it has been cleaned).

Read the raw counts carefully. Roughly 1000 survivors are arithmetic and
comparison flips, and the four biggest `ui/*` files contribute 505
between them — render code, where a survivor means a wrong pixel rather
than a wrong action. Working top-down by count is the wrong order.

- [x] **`src/audit.rs` — 42 survivors triaged** — DONE 2026-08-26.
  Twelve were real and are now covered; six are genuinely equivalent and
  are reasoned about at the code they mutate rather than listed, so the
  next triage doesn't redo the analysis. Highlights:
  - **A quoted value butted against the next key** lost the rest of the
    line. Four survivors sat in "consume the closing quote"; every
    existing test put a space after the quote, and with a space the
    parser recovers. `audit replay` reconstructs an AWS action from this.
  - **`field_token` was untested on `=`** — the half that matters, since
    an unquoted `=` parses back as two fields. Field forgery in a log
    that `replay` acts on.
  - **The JSON renderer's comma placement** had four survivors: nothing
    parsed the output, the tests asserted on substrings. It is parsed
    now, per line — it is JSON Lines, not an array.
  - **`(Some(s), _)` in the text renderer was deletable**, so any outcome
    other than `ok` fell through to `-` untested. Its neighbour
    `(Some("ok"), _)` was pure duplication — deleted rather than covered.
  - **`detail_field` was a closure inside `write_audit_line_raw`**, so
    the only way to reach it was to fire a webhook. It decides what the
    webhook reports and had already been wrong once.
  - Stale rationale corrected: the JSON renderer's "so we don't pull in
    `serde_json`" stopped being true when five surfaces moved onto it.

  **Verified**: 188 mutants re-run, 139 caught, 27 missed, 19 timeouts —
  down from 42. Every remaining `parse_kv_pairs` survivor is one
  predicted equivalent *before* the run, and each now says why in place.
  One claimed fix had not worked: see the note on test shape below.

- [ ] **`src/audit.rs` — the writer seam, 20 survivors** — of the form
  `replace append_action_dispatched with ()`, plus `drain_webhooks` (5)
  and `fire_webhook` (3). Same shape as the `aws/eb.rs` item below: the
  function is I/O and nothing drives it. `drain_webhooks` is the
  tractable one — `tokio::time` with `start_paused` makes its deadline
  arithmetic deterministic — but it reads a process-global atomic, so it
  needs the `MARKER_LOCK` treatment from `freeze.rs` first.

- [x] **`src/aws/eb.rs` — the reachable logic** — 19 of 30 done
  2026-08-26 by extracting pure helpers, which is the only way to reach
  any of it: the logic sits between two `.send().await` calls.
  `QueueDiscovery` (10) is the notable one — worker-queue discovery with
  a real precedence rule, and *deletable `WorkerQueue` /
  `WorkerDeadLetterQueue` match arms*, meaning nothing checked that
  EB-reported queues were recognised at all. Also
  `vpc_context_from_settings` (4, empty-value guards),
  `sort_listener_rows` (2, default-listener-first),
  `summarise_instance_health` (2 — the existing test set exactly the two
  buckets that survived to zero) and `platform_branch_from` (1).

  **Verified**: 213 mutants re-run, 98 caught, 88 missed — 17 of the 30
  reachable killed by the run, plus 2 more below, so 19 of 30.

- [ ] **`src/aws/eb.rs` — 11 reachable survivors left**, each needing its
  own extraction: `list_events_inner` (3),
  `latest_platform_version_date` (3), and singles in `fetch_env_vars`,
  `list_tags`, `fetch_env_configuration_options`,
  `list_compatible_platforms`, `fetch_env_rds_config`.

- [x] **Test shape: one case per guard, and it has to discriminate** —
  2026-08-26. The verification runs caught the same mistake twice, in
  tests written the same day to close these very survivors, which is why
  it is worth writing down rather than just fixing.

  `field_token` quotes on whitespace, `"` **or** `=`. The test used
  `"a=b"` — one value tripping one trigger — and the surviving mutant
  flipped a *different* `||`, collapsing the predicate to "contains
  `=`". `a=b` is quoted under both. A value must trip exactly the
  trigger under test and no other, and there must be a converse case
  (a value tripping none stays bare), or "always quote" passes the lot.

  `vpc_context_from_settings` has four `!value.is_empty()` guards. The
  test proved the *representative* one and assumed its three siblings
  followed; `ELBSubnets` and `SecurityGroups` survived. Worse, the
  all-empty case it did apply to all four proves nothing for a list:
  `split_csv("")` yields an empty vec, which is what the default already
  is. Only overwriting a resolved value discriminates. Both fixed, both
  re-verified by re-applying the exact surviving mutant: CAUGHT.

  The general form: N sibling guards need N cases, each with a value
  that distinguishes *that* guard from its default. Sharing one case
  across siblings is how a test reads as coverage while checking one.

- [x] **`src/app/text.rs` + `src/app/render.rs` — 94 survivors** —
  worked 2026-08-26. Both are pure logic that `ARCHITECTURE.md` says is
  "deliberately extracted so it can be tested directly", and both were
  the *most* survivor-dense non-UI files in the tree. `text.rs` has no
  `#[cfg(test)]` block of its own; its tests live across four modules in
  `app/tests/`, and several were asserting the shape of an answer rather
  than the answer.

  The recurring cause was tests sampling the middle of a bucket:
  - `format_age` was checked at 120s, 5h and 10d with `ends_with`, so
    every `<` in the ladder was interchangeable with `<=`. Boundary-exact
    now, both sides of each edge.
  - `humanize_short_age` had no test at all (7 survivors).
  - `parse_toggle` checked every word from the state it toggles *away*
    from — and the fallback is `_ => !current`, so `parse_toggle(Some
    ("on"), false)` returns true whether the arm exists or not. Both arms
    were deletable. Each word is now checked from both states.
  - `health_rank` asserted only relative order, which survives a bucket
    falling through to `_ => 4`. Absolute ranks now.
  - `alarm_kind_to_metric` compared aliases to each other
    (`p90 == latency`), which passes when both resolve to `None` — i.e.
    when their shared arm is deleted. `4xx` was never mentioned.
  - `edit_distance` missed that turning `prev[j+1] + 1` into `* 1` makes
    deletion free; every existing case's minimum came from another term.
    A pure-suffix pair (`abc` / `abcdef`) is the one that bites.

  Two extractions were needed. `suggest_from` splits the candidate
  selection from the live registry, because testing the tie-break through
  `suggest_command` pins the registry's contents and ordering rather than
  the rule. And in `render.rs`, `tree_glyph` / `is_last` / `coarse_age`
  collapse duplication the sweep surfaced: `i + 1 == n` appeared **six
  times** in `render_env_resources_tree` (12 survivors on one
  expression, and twelve places for the tree to lose its corner), and the
  same three-branch duration ladder was written out twice in
  `format_deploy_preview`.

- [x] **`src/app/action_flow.rs` — the pending/undo machinery** —
  2026-08-26. 69 reachable survivors; this pass covers the
  destructive-action ones, which are the highest-consequence logic the
  sweep touched.

  The finding worth naming: **the undo window had no test holding it
  shut.** `tick_pending_dispatch_fires_after_deadline` covered only the
  elapsed direction, so `if now < pd.deadline` was interchangeable with
  `now == pd.deadline` — under which a queued destructive action fires
  on the very next tick and the operator's cancel window does not exist
  at all. That existing test still passes against the mutation; only the
  new converse catches it.

  Also covered: `push_pending`'s `PENDING_CAP` (flipped to `<` the panel
  never holds more than one row), `complete_pending`'s three-way match
  (one case per conjunct — wrong label, wrong target, already-finished —
  since the realistic collision is the same action against a different
  env), `expire_pending`'s TTL in both directions, and the **six
  individually deletable `advance_action_flow` arms** (Rebuild, Deploy,
  UpgradePlatform, Clone, Scale, Capacity), where a deleted arm drops
  the menu entry into the catch-all and it silently does nothing while
  every other action still works.

  Mutation-verified, five re-applied by hand: CAUGHT on each.

  **Verified** for `text.rs`: 206 mutants, 197 caught, 6 missed (was 39).
  Five of the six are equivalents documented at the code; the sixth was a
  test of mine pointed at the wrong `+` — see the transposition note in
  `edit_distance`.

  **Verified** for `render.rs`: 172 mutants, 132 caught, 27 missed (was
  55). The extraction did the work it was meant to:
  `render_env_resources_tree` 18 → 2, `format_deploy_preview` 13 → 1,
  and `tree_glyph` / `is_last` / `coarse_age` have **no** survivors
  between them. What is left is spread thin — `format_lineage` (6),
  `render_explain_overlay` (5), `format_ssm_results` (4) — with no
  cluster worth a dedicated pass.

- [x] **`handle_action_key` — the confirm-modal keys** — 2026-08-26.
  Every key that answers a destructive Y/N confirm was individually
  deletable: `y`, `Enter`, `n`, `Esc`, `q`. Nothing checked that
  answering the modal did anything at all. The decline keys are the ones
  that matter — a deleted arm falls into the catch-all, so the modal
  ignores the keypress and stays open over a destructive action the
  operator has just tried to back out of. Also covered: `q` is
  deliberately NOT bound on type-the-name confirms (it may be a
  character in the env name), and the menu cursor's wrap-around, whose
  two expressions carried ten survivors between them because nothing
  walked the cursor off either end. Mutation-verified four ways: CAUGHT.

  **Verified**: 114 mutants, 69 caught, 40 missed (was 72).
  `handle_action_key` 35 → 18, with zero `ConfirmKind::YesNo` survivors.

- [x] **The TUI rollout-approval gate** — 2026-08-26. The twin of the
  CLI's `--yes` gate, and unlike that one it *is* reachable from a test.
  `any(|r| r.env_found == Some(true))` followed by `if !any_ok` carried
  survivors on both the comparison and the negation; inverted, a plan
  where **every** region failed pre-flight is the one that dispatches.
  Both halves covered — no passing region refuses, one passing region is
  enough — because "always refuse" passes the first alone. The `n` /
  `esc` / `q` decline arms were separately deletable too, so an ignored
  keypress leaves a multi-region dispatch armed on screen.
  Mutation-verified three ways: CAUGHT.

- [ ] **`src/app/action_flow.rs` — what's left**: the swap-target picker
  branches (the `!CONTROL` guards on picker j/k, the `env_found` filters
  in `open_parameterised_action_on`, 8) and `spawn_action` (7, mostly
  the whole-function seam).

- [x] **`app/msg.rs` — the DLQ handlers** — 2026-08-26, eleven
  survivors. Worth the attention because the DLQ view is the one place a
  keystroke destroys a message: `x` deletes the *selected* row, `p`
  purges the queue. Three shapes were unguarded by any test:

  - **The wrong-env guards.** `dlq.env_name != env_name` in both
    `handle_dlq_messages` and `handle_dlq_action_result` was flippable,
    so another env's peek could land in this view — the same class as
    the wrong-env spacious click and the `:rollback` wrong-env bug.
  - **The refetch cursor clamp.** `Some(cur) if cur < messages.len()`
    was interchangeable with `<=`, and index `len` is out of bounds. A
    cursor left past the end of a shorter page is exactly the shape that
    destroys the wrong message, since `x` acts on the selection. Both
    directions covered now — a cursor at `len` resets, a valid one is
    kept, because a clamp that always resets passes the first case
    alone.
  - **`retain(|m| m.id != message_id)`.** Inverted, the list keeps
    *only* the deleted message. And `failures == 0` flipped reports
    every clean replay as a failure and every partial one as clean.

  Driven through `handle_msg` rather than the private handlers, so the
  generation check and the routing are pinned too. Mutation-verified
  four ways: CAUGHT.

- [x] **`app/cost.rs` — 45 survivors, none of them whole-function** —
  2026-08-26. 37 sat in `instance_hourly_usd`, all "delete match arm":
  nothing checked the price table at all.

  Asserting 39 constants would have been a copy of the table, which pins
  nothing — test and code drift together the moment someone edits both.
  Two properties do it instead. The **values** are pinned by the
  doubling relationship AWS actually prices on (each size step doubles;
  worst real deviation in the table is 0.87%, so the tolerance is 2%),
  so a mistyped figure fails without any figure being written down
  twice. The **key set** is listed on purpose — that it must not
  silently shrink is the whole invariant — and a third test reads the
  arm count out of the source so the list can't fall behind the table in
  the other direction either.

  Also: the "N without cost data" note fired on every view with `>=`;
  the 24-hour staleness threshold (`24 * 60 * 60`) had both `*`
  survivable, and `24 + 60 * 60` marks a 1-hour-old cache stale; and in
  `app_rollup` the `Terminating` status arm and the Worker-tier/DLQ-depth
  conjunction were each independently deletable. The existing
  `app_rollup_counts_envs_red_and_updating` passes against both of those
  mutations — checked.

  Mutation-verified six ways: CAUGHT, with a deleted price arm caught by
  three tests independently.

- [x] **`app/forms.rs` — 52 of 56 survivors in `handle_form_key`** —
  2026-08-26. The same shape as the action menu: field navigation with
  wrap-around arithmetic, plus per-field-kind input rules nothing
  exercised. `Tab` and `BackTab` were separately deletable; the
  `((cur + delta) % n + n) % n` option-cursor expression carried ten
  survivors on its own; the `!is_multi` guard — which is what keeps
  Up/Down moving *within* a MultiSelect rather than between fields —
  was flippable in both directions; and the Integer field's
  `is_ascii_digit() || (c == '-' && value.is_empty())` accepted nothing
  at all as `&&`, so an operator typing a capacity would have watched
  the field stay empty.

  Mutation-verified four ways: CAUGHT.

- [ ] **`app/forms.rs` — the rest of `handle_form_key`**, chiefly
  Select/Boolean arms and the submit path.

- [x] **`src/app/safety.rs` and `src/cli/writes.rs` have ZERO
  survivors** — noted 2026-08-26, no work needed. The write gate that
  `CONTRIBUTING.md` says a bypass gets rejected on sight is fully
  mutation-covered: every mutant in both files is caught. `freeze.rs`
  has one, and it is the whole-function seam. Worth recording, because
  the headline 53.1% says nothing about *where* the coverage is.

- [x] **`src/cli/action.rs` — the rollout decision logic** —
  2026-08-26. Most of the file's 37 reachable survivors sat in
  `run_rollout`, each check written inline against `eprintln!` +
  `std::process::exit(2)`, so none was reachable from a test. Extracted
  `validate_rollout_flags` (which combinations are refused, and the
  `--parallel implies --continue-on-fail` resolution) and
  `unattempted_regions` (the "skipped (rollout halted)" list, which was
  written out twice — once per output format — and both copies carried
  the same survivor; losing those lines is what 0.14.1 shipped a fix
  for). Mutation-verified three ways: CAUGHT.

- [ ] **`run_rollout`'s `if !yes` confirmation gate is untestable** —
  found 2026-08-26 and NOT fixed. Deleting the `!` inverts it: `--yes`
  would print "re-run with --yes" and exit 2, and *omitting* `--yes`
  would dispatch the rollout. That is a safety inversion with nothing
  behind it.

  It cannot be reached from a test as things stand. The gate sits after
  the per-region preflight, so an integration test without credentials
  exits at `list_environments` long before it. Moving the gate ahead of
  the preflight would make it testable but changes behaviour — today the
  operator learns the env is missing from a region *before* being asked
  to confirm, which is the better order. The real fix is the same one
  the SDK seam needs: a fake client layer. Flagging rather than
  guessing.

- [~] **`src/cli/lint.rs::run` is a god-function — 57 survivors in one
  622-line body**, out of 87 for the file. 31 of them are `delete !` and
  15 are `&&` → `||`: condition checks threaded through a single large
  async CLI function that also does the AWS calls and the printing, so
  none was reachable from a test.

  **Partially addressed 2026-08-26: 6 of the 57 — the six that decide
  anything, as opposed to the ~50 that decide whether a line prints.**

  - **`filter_issues`** — `--min-severity` and `--rule`. Written out
    twice (main path and `--watch` cycle path), both copies carrying the
    same survivors. Which issues reach the operator is the entire output
    of this subcommand. `>= min` includes the named level, and `>` would
    silently drop exactly the severity that was asked for; the
    `!rule_filter.is_empty()` guard is what stops an empty `--rule`
    matching nothing and reporting a clean fleet.
  - **`lint_exit_code`** — the matrix that gates CI. Every branch called
    `std::process::exit` inline. The ordering is load-bearing:
    issues-found (3) beats degraded (1) because 3 is actionable, and a
    *clean but degraded* run must not pass green — a region skipped on
    expired credentials otherwise looks identical to a passing check.
    All eight cells named in one table.

  Mutation-verified four ways: CAUGHT.

  **What remains is ~50 survivors, nearly all `!quiet` / `!json` /
  `!quiet && !json` output suppression** — a mutation changes whether a
  line prints. Low consequence individually, and reaching them means
  capturing stdout from a 622-line async function that also makes the
  AWS calls. The remaining structural work is splitting the one-shot
  body from the `--watch` loop; still wants scoping deliberately rather
  than starting mid-run.

- [x] **`app/mode_dlq_handlers.rs` — the purge type-to-confirm gate had
  nothing behind it** — 2026-08-26. The worst single finding of the
  sweep.

  `KeyCode::Enter if dlq.purge_typed.text() == dlq.env_name` is the
  type-the-env-name confirmation on a **queue purge**, and all three of
  its mutants survived. `==` → `!=` purges when the typed name is
  *wrong* and refuses when it is right; guard → `true` bypasses the gate
  entirely; guard → `false` wedges it shut. `p` on a DLQ is not
  recoverable, and the gate exists because of that.

  Covered from both sides — the exact name confirms, and four
  near-misses (trailing space, truncation, wrong case, empty) do not,
  because a near-miss is the realistic failure. Plus a second test that
  a confirmed purge actually **reaches `deny_write`**, since clearing
  the flag and dispatching are different things; that is the same
  wiring-vs-predicate distinction the rollout freeze needed an hour
  earlier.

  Also: the `y`/`Y`/`Enter` single-message delete arm (deletable, and
  the cancel-on-anything-else behaviour is what makes a stray keypress
  safe), and the DLQ cursor's wrap in both directions — ten survivors on
  two expressions, and this cursor is what `x` deletes and `r` resends.

  Mutation-verified four ways: CAUGHT.

- [ ] **`app/mode_dlq_handlers.rs` — what's left**: the replay-spec
  prompt arms, the `Ctrl-R` vs `r` modifier guard, and the `m`
  main/DLQ toggle.

- [x] **`app/spawn_refresh.rs` — the red-alert detection** —
  2026-08-26. Five survivors and no test at all, on the logic behind two
  operator-facing markers `ui/table.rs` renders: the red alert on a row
  and the `+` on a name that just appeared. A wrong condition here is
  either a missed alert or a screen full of spurious ones.

  - `is_red` is `eq_ignore_ascii_case("Red") || …("Severe")`, and `&&`
    there is **unsatisfiable** — no string is both, so every red alert
    in the tool would stop firing.
  - `if !self.prev_health.is_empty()` is what stops the first listing
    marking the whole fleet as new. Inverted, startup flags everything
    and nothing after that ever does.
  - `is_red(&e.health) && !prev_red` is a *transition*, not a state.
    Without the `!`, an env that has been red for a week never alerts
    and only a flapping one does; as `||`, every healthy env alerts.

  All four covered from both sides. Mutation-verified: CAUGHT.

  Worth recording a near-miss in the method: a first grep suggested
  `newly_red` / `newly_added` were written and never read — dead state,
  like `DeploySnapshot::env_name`. They are read, by `ui/table.rs` and
  `ui/header.rs`; the grep had been truncated by a `head -6`. Checked
  before reporting, which is the only reason it wasn't reported.

- [x] **`app/spawn_refresh.rs` — the bell, the status line and the
  history cap** — 2026-08-26.

  - **The bell rings on an INCREASE in alerts, not on their presence.**
    All four mutants of `notify_bell && new_alerts > prev_alerts`
    survived. `>=` rings on every refresh of a fleet that is merely
    still unhealthy, which trains the operator to ignore it — and an
    ignored bell is worse than none. `||` rings for anyone who switched
    it off. Extracted as `should_ring` because the write itself is the
    BEL to stderr that the rule-5 allowlist already justifies, so the
    decision is the only part a test can reach.
  - **A refresh clears the status line it set, and nothing else.**
    `!pinned && status_message == prev_status`: `!=` clobbers exactly
    the message the operator just caused, and dropping the `!` throws
    away pinned results every 15s.
  - **The health sparkline buffer** is capped and trimmed from the
    front, so `>=` leaves it one short and trimming the other end would
    freeze every sparkline at startup.

  Mutation-verified five ways: CAUGHT.

- [x] **`apply_rebuild`'s overlay-mode chain** — 2026-08-26. Eleven
  survivors on one condition. Every mode it lists holds data the context
  switch has just dropped: `Mode::Detail` with `detail == None` is the
  ghost state the comment beside it names, and `Dlq` / `Action` /
  `Form` / `Picker` are the same shape. As `&&` the chain is
  unsatisfiable and nothing ever closes; with any `==` flipped, modes
  that should survive get reset instead — including `Shell`, where that
  would strand a live PTY subprocess with no way back to it. Both halves
  covered: six modes close, six survive. Help's `pre_mode` /
  `pre_overlay` stash is separately pinned, since restoring it after a
  switch is how the ghost gets rendered anyway.
  Mutation-verified three ways: CAUGHT.

- [ ] **`app/spawn_refresh.rs` — what's left**: the throttle backoff
  arithmetic (`Instant::now() + backoff`, two survivors) and the
  whole-function seam.

- [x] **`src/control.rs` — the control socket's peer authorisation** —
  2026-08-26. The most security-relevant survivors in the sweep. The
  socket's own comment says it "drives arbitrary TUI commands including
  `readonly off`", and every operator in its authorisation check was
  alive:

  - `cred.uid() == own` flipped to `!=` admits **everyone except the
    owner**.
  - `cred.uid() == 0` flipped to `!= 0` admits every non-root uid.
  - `||` flipped to `&&` refuses the owner (fails closed — annoying,
    not dangerous).
  - and at the call site, `if !peer_is_owner(..)` had its `!` deletable,
    which inverts the gate so only *other* users are served.

  `uid_is_allowed(peer_uid, own_uid)` is the decision, extracted because
  the socket around it made it unreachable. Covered exhaustively, plus a
  `UnixStream::pair()` test that reads real peer credentials through
  `peer_is_owner` (skipping the mismatch case under root, where the
  `uid == 0` arm legitimately allows everything), plus a source guard on
  the call site.

  Mutation-verified four ways: CAUGHT — **after fixing the guard, which
  could not fail.** It anchored on `pub(crate) fn spawn_listener`; the
  function is `pub fn`, so `split_once` matched the occurrence inside
  the test itself and the slice it checked was its own assertion string.
  It now anchors at column zero and asserts the slice didn't run into
  the test module.

- [x] **`ctl key shift+tab` walked the wrong way** — FIXED 2026-08-26.
  A real bug, found by asking why 14 of `parse_key_spec`'s match arms
  were deletable.

  The arm read `"backtab" | "shift+tab"`, but the loop splits the spec
  on `+` before matching, so the `"shift+tab"` alternative was
  **unreachable**: the pieces are `shift` and `tab`, and the spec came
  out as Tab+SHIFT. That is not cosmetic — the TUI binds `KeyCode::Tab`
  forward and `KeyCode::BackTab` backward in three places (form fields,
  detail tabs, scope cycling), so a script sending `shift+tab` over the
  control socket moved **forwards**. The dead literal is the author's
  intent, sitting where it could never fire.

  Normalised after the loop instead of special-casing the string, so
  `shift+tab`, `Shift+Tab`, `tab+shift` and `SHIFT+TAB` all agree.

- [x] **`parse_key_spec` — 16 survivors, 14 of them deletable name
  arms** — 2026-08-26. Asserting twenty name→code pairs would be a copy
  of the table. Instead the vocabulary is now **documented** in
  `docs/headless.md` — it was ~20 names with two examples, so a script
  author had to read the source — and the parser is pinned to that
  documentation in both directions: every documented name parses, and
  every name the parser accepts is documented. Deleting an arm makes a
  multi-character name fall through to the single-char fallback and be
  rejected, so the first direction catches all fourteen.

  The scrape distinguishes the modifiers row, because `ctrl` alone names
  no key and is correctly rejected — and separately asserts that
  modifiers *stay* rejected alone, so testing them as `<mod>+x` can't
  hide a parser that accepts anything.

  Mutation-verified four ways: CAUGHT — a deleted arm, `|=` → `&=` on a
  modifier, undoing the shift+tab fix, and a name vanishing from the
  docs.

- [ ] **The freeze marker's liveness check is PID-reuse vulnerable, and
  the docs over-claim** — found 2026-08-26 by max code review, NOT
  fixed.

  `read_active` honours a marker whose pid is alive, and `pid_alive` is
  a bare `kill(pid, 0)` existence probe — it does not check the process
  is ebman. So a stale marker whose pid gets reused by *any* unrelated
  process reads as a live fleet freeze, refusing every write across TUI,
  CLI and MCP until someone deletes the file by hand.

  Not theoretical: this machine currently holds
  `~/.cache/ebman/freeze.json` from 22 Aug, pid 3683, five days old.
  That pid is dead right now, so the next reader will clean it up — but
  macOS allocates pids sequentially and wraps at ~99999, so over days
  reuse is likely rather than exotic.

  `docs/safety-and-privacy.md` says the pid scoping means a crashed
  TUI's marker "can't leave a phantom freeze". That claim is wider than
  the implementation supports.

  It fails CLOSED — a phantom freeze refuses writes rather than allowing
  them — so this is confusing, not dangerous. Options, none obviously
  best: hold an advisory `flock` on the marker for the session's
  lifetime (the OS releases it on death, which is the robust answer but
  changes a safety mechanism); record process start time alongside the
  pid and require both to match (portable-ish, fiddly); bound the marker
  by age; or just narrow the documented claim. Wants a deliberate
  decision.

- [x] **A flaky test that could assert against another test's data** —
  found + fixed 2026-08-26 by max code review.
  `a_dispatch_and_its_completion_agree_on_the_region` failed once in
  ~16 full-suite runs. `cache_dir()` is **one temp directory per test
  process**, so every test writing an audit line appends to the same
  file concurrently — and this test used the env name `api-prod`, which
  a dozen other tests also use. It searched the delta for the first line
  mentioning that name, and fell back to searching the *whole file* when
  `strip_prefix` failed, so a concurrent append could make it assert
  against a neighbour's region.

  Fixed by giving it a name nothing else uses, requiring `strip_prefix`
  to succeed rather than silently widening to the whole log, and
  asserting exactly one matching line. Verified it still catches the bug
  it exists for (completion naming the home region instead of the row's)
  and 10 clean full-suite runs after.

- [ ] **Five implementations of "wrap a cursor by ±1", across four
  modules** — `action_flow.rs` (action menu), `mode_dlq_handlers.rs`
  (DLQ list), `forms.rs` (field options ×2, and `form.rs::move_cursor`).
  Seven sites in total; `detail_nav.rs`'s two are a rotated *search
  order* rather than a cursor move and are correctly separate.

  Every one of them showed the same survivor pattern in the sweep, and
  this session tested four of them **separately** instead of collapsing
  them — the opposite of the `render.rs` call, where extracting
  `tree_glyph` took that file 18 → 2. One `wrap_index(cur, delta, len)`
  would do it, and the per-site tests written this session make the
  refactor safe to do.

  Not done here: four modules is past the ~3-module bar CLAUDE.md sets
  for a refactor that isn't required by the task at hand.

- [x] **`config_edit.rs` — Enter could apply a config instead of
  deleting one** — 2026-08-26. 18 survivors around the saved-configs
  delete confirm.

  Enter means *apply* when nothing is armed and *confirm delete* when
  the prompt is up; the two arms are told apart by `!confirm_delete`
  versus `confirm_delete`. Every mutant of the first guard survived, and
  deleting that `!` makes both identical — so the apply arm, being
  earlier, wins on Enter **while the delete prompt is showing**. The
  operator presses Enter to delete a saved configuration and instead
  applies it to the environment, rewriting its option settings.

  Also: the `n`/`N`/`Esc` decline arm was deletable, and `x` arms the
  confirm rather than deleting outright.

  Note for the standing guard: this is a **y/n** confirm, so
  `every_typed_confirmation_gate_names_its_test` does not reach it. That
  guard's scope is typed confirmations, which is narrower than
  "confirmation gates on irreversible operations". Widening it would
  need a way to enumerate y/n gates, which is not obviously mechanical.

  Mutation-verified four ways: CAUGHT — after widening the test, which
  initially missed one. See the commit message.

- [ ] **The SDK seam — 75 survivors in `aws/eb.rs` alone** — every one
  of the form `replace AwsClient::fetch_x with Ok(vec![])`. No test can
  kill these: the function *is* the AWS call, and `AwsClient::stub()`
  doesn't intercept at that level. This is an architecture question, not
  a test-writing one — a fake client layer, or a decision to accept the
  seam and stop counting it. Worth deciding, because it is ~3% of the
  whole tree's mutants and it silently drags the headline score down;
  quoting 53.1% without this footnote overstates the gap.

#### Console parity — BONUS

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
- [ ] **`:custom-platform-create <packer-config>`** — the last console
  write-side gap. Delete already shipped as
  `:custom-platform-delete <arn>`; create is the other half, via
  `elasticbeanstalk:CreatePlatformVersion`. Niche, but a real gap for
  operators who maintain in-house base AMIs — the console offers a
  wizard that builds a new custom AMI from a Packer template.

  **Why it keeps slipping** (skipped 2026-07-15, tagged "fine to slip to
  0.26", and it slipped): it needs S3-bundle upload plumbing plus
  minutes-scale polling of `CreatePlatformVersion`, and the polling has
  more than one reasonable shape — fire-and-forget with a toast, or a
  progress surface like the deploy watcher. None of it is verifiable
  against live EB from here, which is the actual blocker rather than the
  effort.

  *Merged 2026-08-25 from two entries that were the same feature — one
  carried why it was skipped, the other why it matters.*

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
