#!/usr/bin/env bash
# Fail on a `.cargo/mutants.toml` `exclude_re` entry that matches no
# mutant in the tree.
#
#   scripts/mutants-exclusions.sh [config] [mutant-list]
#
# An exclusion outliving the code it excused is an allowlist that only
# grows. Matches against `cargo mutants --list --no-config` (the tree's
# mutants with no exclusions applied) unless a list file is given.
set -euo pipefail

CONFIG=${1:-.cargo/mutants.toml}
LIST=${2:-}
if [ -z "$LIST" ]; then
  LIST=$(mktemp)
  trap 'rm -f "$LIST"' EXIT
  cargo mutants --list --no-config > "$LIST"
fi

python3 -I - "$CONFIG" "$LIST" <<'PY'
import re, sys, tomllib

config, listing = sys.argv[1], sys.argv[2]
with open(config, "rb") as f:
    entries = tomllib.load(f).get("exclude_re", [])
with open(listing) as f:
    names = f.read().splitlines()

stale = [e for e in entries if not any(re.search(e, n) for n in names)]
for e in stale:
    print(f"::error file={config}::exclude_re entry matches no mutant: {e}")
print(f"{len(entries)} exclude_re entries, {len(stale)} matching nothing")
sys.exit(1 if stale else 0)
PY
