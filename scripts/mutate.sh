#!/usr/bin/env bash
# Run one mutation experiment safely.
#
#   scripts/mutate.sh <file> <sed-expression> [test-filter]
#
# Encodes the rules this project learned the hard way. Each of these
# cost a real mistake:
#
#   * Back up with `cp`, never `git checkout <file>`. That is a silent
#     destructive revert of every unstaged change in the file, not an
#     undo of the last edit — it destroyed uncommitted work once.
#   * PROVE the mutation applied. `cargo fmt` reshapes code, so an edit
#     written against the pre-fmt shape silently no-ops and the test
#     "passes" the mutation. A fix was reported as shipped while the
#     old code path was still live.
#   * A test run that produced no result line is not a pass. A mutation
#     that doesn't compile prints `error:` and no `test result:` — twice
#     in one session that was misread as "the test passed".
#   * Restore, always, including on failure.
#   * Clean up. Rebuilding per experiment accumulates artefacts cargo
#     never garbage-collects: one session left 427 GiB in target/ and
#     filled the disk.
set -uo pipefail

FILE=${1:?usage: mutate.sh <file> <sed-expr> [test-filter]}
EXPR=${2:?usage: mutate.sh <file> <sed-expr> [test-filter]}
FILTER=${3:-}
BAK=$(mktemp -t "$(basename "$FILE")")
cp "$FILE" "$BAK"
# proptest records a seed for every failure it sees. A failure this
# script MANUFACTURED is not a regression, and committing its seed would
# imply a bug that never existed — so remember what was there and put it
# back.
PROPTEST_BEFORE=$(ls proptest-regressions 2>/dev/null | sort)
trap 'cp "$BAK" "$FILE"; rm -f "$BAK";
      if [ -d proptest-regressions ]; then
        for f in proptest-regressions/*; do
          [ -e "$f" ] || continue
          grep -qx "$(basename "$f")" <<<"$PROPTEST_BEFORE" || rm -f "$f"
        done
        rmdir proptest-regressions 2>/dev/null
      fi' EXIT
before=$(md5 -q "$FILE" 2>/dev/null || md5sum "$FILE" | cut -d' ' -f1)
sed -i '' "$EXPR" "$FILE"
after=$(md5 -q "$FILE" 2>/dev/null || md5sum "$FILE" | cut -d' ' -f1)

if [ "$before" = "$after" ]; then
    echo "MUTATION DID NOT APPLY — the file is unchanged."
    echo "  Nothing below would mean anything. Check the expression against"
    echo "  the CURRENT (post-fmt) source shape."
    exit 2
fi
echo "mutation applied to $FILE"

out=$(cargo test --lib ${FILTER:+"$FILTER"} 2>&1)
if ! grep -q "^test result:" <<<"$out"; then
    echo "INCONCLUSIVE — no test result line. The mutation probably did not"
    echo "compile, which is not the same as the test passing:"
    grep -E "^error" <<<"$out" | head -5
    exit 2
fi
# A filter that matches NOTHING produces `test result: ok. 0 passed`,
# which is indistinguishable from a real pass unless you look at the
# count. That is how a mutation gets reported NOT CAUGHT when in fact it
# was never exercised — cargo's filter is a substring match, so a regex
# like `foo|bar` silently selects zero tests. Caught in the 0.34.0 swap
# work, one line away from concluding a real safety gate was untested.
if grep -qE "^test result: ok\. 0 passed" <<<"$out"; then
    echo "INCONCLUSIVE — the filter matched NO tests, so nothing ran."
    echo "  \`cargo test\` filters by SUBSTRING, not regex: \`a|b\` matches"
    echo "  neither. Zero tests passing is not the mutation surviving."
    echo "  filter was: ${FILTER:-<none>}"
    exit 2
fi
if grep -q "test result: FAILED" <<<"$out"; then
    echo "CAUGHT — a test fails under this mutation (what you want):"
    grep -E "^test .*FAILED|panicked at|assertion" <<<"$out" | head -5
else
    echo "NOT CAUGHT — the suite is green with this mutation applied."
    echo "  Whatever you were about to claim this code is covered by, it isn't."
fi

# Disk. Each experiment is a fresh compile, and cargo never collects the
# dep artefacts it leaves behind: a single session of this accumulated
# 427 GiB and filled the disk mid-run. Warn early rather than fail late.
size_gb=$(du -sg target 2>/dev/null | cut -f1)
if [ -n "${size_gb:-}" ] && [ "$size_gb" -gt 40 ]; then
    echo
    echo "target/ is ${size_gb}G — run \`cargo clean\` before continuing."
    echo "  Mutation sweeps accumulate dep artefacts that cargo does not"
    echo "  garbage-collect. A fresh target is about 7G."
fi
