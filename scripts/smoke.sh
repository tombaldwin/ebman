#!/usr/bin/env bash
# Smoke-test a built ebman binary, offline: the version it reports, its
# help and shell completions, and one MCP session against the demo
# fleet (initialize, then list_environments).
#
#   scripts/smoke.sh <binary> <expected-version>
#
# release.yml runs this on every binary it is about to upload: the
# release build differs from what CI tested (LTO, panic=abort, another
# target), and nothing ran the artifact before it was attached to a
# release. CI runs it on its own build too, so a break shows up before a
# tag rather than at one.
set -euo pipefail
bin=${1:?usage: smoke.sh <binary> <expected-version>}
want=${2:?usage: smoke.sh <binary> <expected-version>}
fail() { echo "smoke: FAIL — $*" >&2; exit 1; }

got=$("$bin" --version | head -1)
[ "$got" = "ebman $want" ] || fail "--version said '$got', expected 'ebman $want'"

"$bin" --help >/dev/null || fail "--help exited non-zero"

completions=$("$bin" completions bash) || fail "completions bash exited non-zero"
[ -n "$completions" ] || fail "completions bash printed nothing"

# MCP over stdio against the demo fleet. The server answers each line and
# exits at end of input; a watchdog stops a hung one rather than the job.
out=$(mktemp)
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_environments","arguments":{}}}' \
  | "$bin" mcp serve --demo >"$out" 2>/dev/null &
pid=$!
( sleep 30; kill "$pid" 2>/dev/null ) &
watchdog=$!
wait "$pid" || true
kill "$watchdog" 2>/dev/null || true
wait "$watchdog" 2>/dev/null || true
grep -q '"id":1' "$out" || fail "MCP: no answer to initialize"
grep -q '"id":2' "$out" || fail "MCP: no answer to list_environments"
grep -q 'poly-prod-api' "$out" || fail "MCP: list_environments did not return the demo fleet"
grep -q 'firing_alarms' "$out" || fail "MCP: list_environments lacks firing_alarms"
rm -f "$out"

echo "smoke: ok — $bin is ebman $want"
