# VHS tape for ebman's README hero gif.
#
# Render with:
#   brew install vhs        # one-time
#   vhs demo.tape           # produces demo.gif next to this file
#
# Flow (≈25 s):
#   launch → main table → /staging filter → ! (`:why` diagnostic
#   overlay) → drill into Detail → Tab to Detail/Instances → `s`
#   for a fake SSM session that types itself out → detach → quit.
#
# Targets ebman --demo, so no AWS calls and the frame is
# reproducible on every render.

Output demo.gif

# Terminal box. Wide enough that the main env table doesn't truncate
# (~120 cols), tall enough for the header + table + a couple of
# status lines (~30 rows). 14pt MesloLGS Nerd Font is the one the
# operator already has installed via the brew cask.
Set Width 1280
Set Height 720
Set FontSize 14
Set FontFamily "MesloLGS Nerd Font Mono"
Set Theme "Catppuccin Mocha"
Set TypingSpeed 50ms
Set PlaybackSpeed 1.0
Set Padding 16

# Launch ebman against the synthetic six-env fleet (the fixture in
# `src/demo_fixture.rs`). `~/.cargo/bin/ebman` rather than bare
# `ebman` so VHS picks up the locally-built binary that has --demo,
# rather than the brew-installed 0.8.0 that predates the flag. Drop
# back to bare `ebman` once --demo ships in a released version.
Type "~/.cargo/bin/ebman --demo"
Sleep 400ms
Enter

# Let the splash animation play (3s minimum) + a beat for the table
# to settle. The viewer's eye lands on the table around t≈3.5s.
Sleep 4500ms

# Hover the eye on the table for a beat — gives time to register the
# Red `poly-staging-api`, the Yellow `poly-batch` with its DLQ chip,
# the COST column, the INST counts, and the Grey `ironwood` row whose
# INST/COST read `—` (an env Beanstalk can't account for).
Sleep 2500ms

# Filter to the staging envs. The fixture's sort puts staging-api
# (Red) first, so no cursor movement is needed — the table opens
# with that row highlighted.
Type "/staging"
Sleep 1200ms
Enter
Sleep 1500ms

# `!` opens the `:why` diagnostic overlay on the selected env —
# recent events / alarms / instance health / recent deploys, all in
# one screen. Demo-mode injects fixture data for all five sub-
# spawns so the overlay renders fully, not "fetching…" placeholders.
# The Red staging-api env tells the operational story: a deploy
# happened, health degraded, alarms fired.
Type "!"
Sleep 5500ms

# Close the overlay with Esc.
Escape
Sleep 1000ms

# Drill into Detail — opens to the Health rollup tab by default.
# Detail/Health shows status / health pill / cost chip + the rolled-
# up event / alarm / instance summary.
Enter
Sleep 2500ms

# Tab → Detail/Events (fixture-populated). Tab again → Instances
# (two synthetic instances). Demo mode pre-injects both lists.
Tab
Sleep 2000ms
Tab
Sleep 2000ms

# Press `s` to dispatch an SSM session into the highlighted
# instance. Demo mode opens a fake shell pane that types its
# canned content out at ~60 cps with a ~200ms beat after each
# newline. Banner + uptime + tail on the EB engine log; the
# full transcript wraps in ~6 s plus a beat to dwell on the
# final prompt.
Type "s"
Sleep 9000ms

# Esc detaches the demo shell pane (real PTY sessions don't accept
# Esc — vim and friends need it forwarded — but demo sessions have
# no subprocess to forward to, so Esc is repurposed as a detach
# alongside F12). Lands the operator back on Detail/Instances.
Escape
Sleep 1000ms

# Esc again to leave Detail back to the main env table.
Escape
Sleep 800ms

# Clear the `/staging` filter so the full fleet is back on screen for
# the closing frame — `/` enters Filter mode and empties the buffer,
# Enter commits the now-empty filter (= show all). The Grey `ironwood`
# row lands back in view as the loop point: a quiet last beat for
# anyone watching closely.
Type "/"
Sleep 400ms
Enter
Sleep 2500ms

# Clean exit so the loop point isn't a half-rendered overlay.
Type ":q"
Enter
Sleep 600ms
