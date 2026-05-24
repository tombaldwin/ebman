# VHS tape for ebman's README hero gif.
#
# Render with:
#   brew install vhs        # one-time
#   vhs demo.tape           # produces demo.gif next to this file
#
# Output target: a 15-20s loop that lands on GitHub's README inline,
# shows the headline value (the env table with health tints / INST /
# COST / DLQ chip), and demonstrates a one-keystroke drill-in.
#
# Scope: stays on the demo-mode supported surface — main env table +
# the filter mode + Detail/Health. The spawn-and-overlay tabs
# (:why, Detail/Events alarms / instances / metrics) still fire
# against the stub AwsClient and would show "fetching…" placeholders
# in demo mode; once spawn-site gating lands we can extend this tape
# to walk through :why too.

Output demo.gif

# Terminal box. Wide enough that the main env table doesn't truncate
# (~120 cols), tall enough for the header + table + a couple of
# status lines (~30 rows). 14pt JetBrains Mono is sized for screen-
# reading without making the gif feel cramped.
Set Width 1280
Set Height 720
Set FontSize 14
Set FontFamily "JetBrains Mono"
Set Theme "Catppuccin Mocha"
Set TypingSpeed 50ms
Set PlaybackSpeed 1.0
Set Padding 16

# Launch ebman against the synthetic six-env fleet (the fixture in
# `src/demo_fixture.rs`). No AWS calls, no disk reads — the same
# frame renders every time.
Type "ebman --demo"
Sleep 400ms
Enter

# Let the splash animation play (3s minimum) + a beat for the table
# to settle. The viewer's eye lands on the table around t≈3.5s.
Sleep 4500ms

# Hover the eye on the table for a beat — gives time to register the
# Red `ledgerly-staging-api`, the Yellow `ledgerly-batch` with its
# `⚠ DLQ:12` chip, the COST column, the INST counts.
Sleep 2500ms

# Filter to the red env so the eye lands on the trouble row.
Type "/staging"
Sleep 1200ms
Enter
Sleep 1800ms

# Move the cursor to the Red row (ledgerly-staging-api). The filter
# left only two rows; one Down lands on it.
Down
Sleep 800ms

# Drill into Detail — opens to the Health rollup tab by default.
# The page shows status / health / cost chip and the synthetic
# event annotation. Lingers long enough for the viewer to read.
Enter
Sleep 3500ms

# Back out to the env table.
Escape
Sleep 1200ms

# Clean exit so the loop point isn't a half-rendered overlay.
Type ":q"
Enter
Sleep 600ms
