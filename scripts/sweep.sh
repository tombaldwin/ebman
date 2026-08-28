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
# IT IS SLOW, BUT MEASURE IT PROPERLY. A completed run of 478 mutants
# took 78 minutes at -j 6: 6.14 mutants/min, so the ~6200-mutant tree is
# about 17 hours. That is an overnight job.
#
# Do NOT estimate from the first few mutants. Timing the opening eight
# of a run gave 0.8/min — they are dominated by build warm-up — and that
# figure said "5 days, don't bother", which was wrong by 7.7x and got
# written into the backlog before anyone checked it against a run that
# actually finished.
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

# SHARDING IS A DISK REQUIREMENT, NOT A SPEED ONE.
#
# Each mutant leaves incremental build artefacts behind in its job's
# tree, and cargo never collects them: measured 2026-08-28 at 0.34 GB
# per mutant, sustained, not plateauing. Unsharded, the ~6200-mutant
# tree therefore wants about 2.1 TB and dies partway through whatever
# disk you give it. That is the same failure CLAUDE.md records at 427
# GiB, just further along.
#
# A shard is a separate `cargo mutants` PROCESS, so its trees are freed
# when it exits and the disk resets before the next one starts. Peak
# usage becomes (mutants per shard x 0.34 GB) instead of the whole run.
# 30 shards is ~207 mutants each, ~70 GB peak — comfortably inside this
# machine with the floor still enforced. It is also why CI shards 24
# ways; that number was measured against its own runners, not guessed.
#
# Total time is unchanged: the shards run one after another.
SHARDS=${SWEEP_SHARDS:-30}

if [ -n "$DIFF" ]; then
  say "scope: diff $DIFF"
  set -- --in-diff "$DIFF"
  SHARDS=1
else
  say "scope: whole tree, $SHARDS shards run sequentially"
  set --
fi

# `--exclude src/main.rs`: `cargo mutants -- --lib` does not compile the
# binary, so every mutant there is unconditionally MISSED regardless of
# coverage. Same reason `scripts/mutate.sh` refuses that file outright.
run_shard() {
  local n=$1
  local shard_args=()
  [ "$SHARDS" -gt 1 ] && shard_args=(--shard "$n/$SHARDS")
  cargo mutants "$@" "${shard_args[@]}" \
    --no-shuffle --timeout 120 -j "$JOBS" \
    --exclude src/main.rs \
    -o "$OUT/shard-$n" -- --lib >> "$LOG" 2>&1 &
  SWEEP_PID=$!
}

# Merged view across shards, written as each one lands so a run killed
# part-way still leaves a readable (and clearly partial) result.
merge() {
  for f in caught missed timeout unviable; do
    cat "$OUT"/shard-*/mutants.out/"$f".txt 2>/dev/null | sort -u > "$OUT/$f.txt" || true
  done
}

run_shard 0
say "sweep pid $SWEEP_PID (shard 0/$SHARDS)"

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
merge
say "shard 0/$SHARDS done rc=$rc, free $(free_gb)G"

# Remaining shards, each a fresh process so its build trees are freed
# before the next allocates any.
n=1
while [ "$n" -lt "$SHARDS" ]; do
  run_shard "$n"
  say "shard $n/$SHARDS started (pid $SWEEP_PID), free $(free_gb)G"
  while kill -0 "$SWEEP_PID" 2>/dev/null; do
    f=$(free_gb)
    if [ "${f:-0}" -lt "$MIN_FREE_GB" ]; then
      say "ABORT: only ${f}G free, below the ${MIN_FREE_GB}G floor"
      say "ABORT: results in $OUT are PARTIAL — do not read them as a full sweep"
      kill "$SWEEP_PID" 2>/dev/null; sleep 10; pkill -9 -f cargo-mutants 2>/dev/null
      # A killed run does NOT clean up after itself — verified
      # 2026-08-28, when stopping one by hand left 64 GB of build trees
      # in TMPDIR. Aborting for low disk and then leaving the disk full
      # is the worst of both.
      sleep 5; rm -rf "${TMPDIR:-/tmp}"/cargo-mutants* 2>/dev/null || true
      say "reclaimed the abandoned build trees; free now $(free_gb)G"
      merge
      exit 2
    fi
    sleep 30
  done
  wait "$SWEEP_PID"; rc=$?
  merge
  say "shard $n/$SHARDS done rc=$rc, free $(free_gb)G"
  n=$((n + 1))
done

merge
say "sweep finished, free now $(free_gb)G"
for f in caught missed timeout unviable; do
  say "  $f: $(wc -l < "$OUT/$f.txt" 2>/dev/null || echo 0)"
done
exit 0
