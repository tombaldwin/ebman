#!/usr/bin/env bash
# Run a whole-tree (or diff-scoped) mutation sweep on this machine.
#
#   scripts/sweep.sh                 # whole tree
#   scripts/sweep.sh <diff-file>     # only mutants touched by that diff
#
# GitHub's scheduled sweep is unreliable — runs sit queued for hours
# without a runner, and the 2026-08-27 nightly never fired at all — so
# this exists to run the same measurement locally without the two
# failure modes a naive `cargo mutants` invocation hits here.
#
# DISK IS THE BINDING CONSTRAINT, NOT CPU. Each parallel job builds its
# own copy of the tree, so the footprint scales with -j. Measured on
# 2026-08-27: `-j 8` consumed 43 GB in 13 minutes and was still climbing
# at 4 GB/min, which fills this disk in about twenty more. CLAUDE.md
# records an earlier session reaching 427 GiB and filling it for real.
# Hence: fewer jobs than cores, a `cargo clean` first, and a watchdog
# that aborts rather than letting the machine run out.
#
# IT IS ALSO SLOW, and the whole tree is NOT an overnight job here.
# Measured 2026-08-27: 0.8 mutants/min at -j 8 (eight in ten minutes of
# real work), against ~6250 mutants in the tree. That is 5.4 days at
# -j 8 and 9 days at -j 4. GitHub gets it down to ~4.4h only by sharding
# across twenty-four machines; one machine cannot.
#
# So: scope with a diff for anything you want back tomorrow. This week's
# diff is 478 mutants ~ 10-13h, which is a real overnight run. Reserve
# the whole-tree sweep for when a multi-day run is acceptable, or for
# whenever GitHub's runners come back.
set -uo pipefail

JOBS=${SWEEP_JOBS:-4}
# Abort with this much left. Not zero: the machine still needs room to
# be usable, and an abort that itself cannot write its log is no use.
MIN_FREE_GB=${SWEEP_MIN_FREE_GB:-25}
OUT=${SWEEP_OUT:-$HOME/.cache/ebman-sweep}
DIFF=${1:-}

mkdir -p "$OUT"
LOG="$OUT/sweep.log"
: > "$LOG"

free_gb() { df -g "$HOME" | tail -1 | awk '{print $4}'; }

say() { echo "[$(date -u +%H:%M:%S)] $*" | tee -a "$LOG"; }

say "sweep starting: jobs=$JOBS min_free=${MIN_FREE_GB}G out=$OUT"
say "free before: $(free_gb)G"

# `target/` was 59G before the last attempt; reclaiming it is the
# cheapest way to buy the sweep room, and the sweep rebuilds from
# scratch in its own trees regardless.
if [ "$(du -sg target 2>/dev/null | cut -f1 || echo 0)" -gt 15 ]; then
  say "target/ is $(du -sh target 2>/dev/null | cut -f1) — cleaning first"
  cargo clean
  say "free after clean: $(free_gb)G"
fi

if [ -n "$DIFF" ]; then
  say "scope: diff $DIFF"
  set -- --in-diff "$DIFF"
else
  say "scope: whole tree"
  set --
fi

# `--exclude src/main.rs`: `cargo mutants -- --lib` does not compile the
# binary, so every mutant there is unconditionally MISSED regardless of
# coverage. Same reason `scripts/mutate.sh` refuses that file outright.
cargo mutants "$@" \
  --no-shuffle --timeout 120 -j "$JOBS" \
  --exclude src/main.rs \
  -o "$OUT" -- --lib >> "$LOG" 2>&1 &
SWEEP_PID=$!
say "sweep pid $SWEEP_PID"

# Watchdog. Checks every 30s; kills the sweep if the disk gets low, and
# says so in the log — an aborted sweep that looks like a finished one
# is how a partial result gets read as a full measurement.
while kill -0 "$SWEEP_PID" 2>/dev/null; do
  f=$(free_gb)
  if [ "${f:-0}" -lt "$MIN_FREE_GB" ]; then
    say "ABORT: only ${f}G free, below the ${MIN_FREE_GB}G floor"
    say "ABORT: results in $OUT are PARTIAL — do not read them as a full sweep"
    kill "$SWEEP_PID" 2>/dev/null
    sleep 10
    pkill -9 -f cargo-mutants 2>/dev/null
    exit 2
  fi
  sleep 30
done

wait "$SWEEP_PID"
rc=$?
say "sweep finished rc=$rc, free now $(free_gb)G"
for f in caught missed timeout unviable; do
  n=$(wc -l < "$OUT/mutants.out/$f.txt" 2>/dev/null || echo 0)
  say "  $f: $n"
done
exit $rc
