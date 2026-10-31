#!/usr/bin/env bash
# Judge the changed-lines mutation run from its shards' artifacts.
#
#   scripts/mutants-verdict.sh <dir-of-shard-artifacts> <shard-count>
#
# Each shard uploads `mutants.out/` as `mutants-in-diff-<n>`, holding
# `complete` once it finished (with mutants or without). Fails when:
#
#   * a shard has no `complete`. A shard killed at its timeout still
#     uploads, and an absent survivor list from a killed run must not
#     read as clean. That is how 2026-10-29's 47- and 70-mutant pushes
#     judged nothing.
#   * any mutant survived or timed out on the changed lines.
#   * the push had no base to diff against (`no-base`), so nothing was
#     judged.
#
# One job decides, on push and pull request alike. Survivors on a push
# used to be a `::warning::` on a green run, and a run's status is what
# `gh run watch` and the failure email carry: 052bb0b's survivor was
# found only by reading the log.
#
# The escape for a mutant no test can distinguish is an `exclude_re`
# entry in `.cargo/mutants.toml`, and only the maintainer adds one: see
# that file.
set -uo pipefail

DIR=${1:?usage: mutants-verdict.sh <dir> <shard-count>}
SHARDS=${2:?usage: mutants-verdict.sh <dir> <shard-count>}

fail=0
# A push with no usable base (a force push, a new branch) had nothing to
# diff against, so nothing was judged: that is not a clean pass, and a
# green run saying "no mutant survived" over it was the hole. The next
# push with a base judges its own lines.
if ls "$DIR"/mutants-in-diff-*/no-base >/dev/null 2>&1; then
  echo "::error::this push had no base commit to diff against (a force push?), so its changed lines were not judged"
  fail=1
fi
for ((s = 0; s < SHARDS; s++)); do
  if [ ! -f "$DIR/mutants-in-diff-$s/complete" ]; then
    echo "::error::shard $s/$SHARDS did not finish, so its mutants were not judged"
    fail=1
  fi
done

survivors=$(cat "$DIR"/mutants-in-diff-*/missed.txt "$DIR"/mutants-in-diff-*/timeout.txt 2>/dev/null)
n=0
if [ -n "$survivors" ]; then
  n=$(printf '%s\n' "$survivors" | wc -l | tr -d ' ')
fi

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "### $n mutants survived on the changed lines"
    if [ "$n" != 0 ]; then
      echo
      echo '```'
      printf '%s\n' "$survivors"
      echo '```'
    fi
  } >> "$GITHUB_STEP_SUMMARY"
fi

if [ "$n" != 0 ]; then
  # `src/x.rs:199:25: replace ...` -> an annotation on that line.
  while IFS= read -r line; do
    if [[ $line =~ ^([^:]+):([0-9]+):([0-9]+):\ (.*)$ ]]; then
      echo "::error file=${BASH_REMATCH[1]},line=${BASH_REMATCH[2]},col=${BASH_REMATCH[3]}::mutant survived: ${BASH_REMATCH[4]}"
    else
      echo "::error::mutant survived: $line"
    fi
  done <<< "$survivors"
  echo "$n mutants survived on the changed lines"
  fail=1
fi

if [ "$fail" = 0 ]; then
  echo "every shard finished; no mutant survived on the changed lines"
fi
if [ -n "${GITHUB_STEP_SUMMARY:-}" ] && ls "$DIR"/mutants-in-diff-*/no-base >/dev/null 2>&1; then
  echo "**Not judged:** no base commit to diff against." >> "$GITHUB_STEP_SUMMARY"
fi
exit "$fail"
