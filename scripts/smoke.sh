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

# A scratch home, so running this by hand writes no log or audit line
# into the developer's own ~/.cache/ebman or ~/.config/ebman.
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
export HOME="$scratch" XDG_CONFIG_HOME="$scratch/.config" XDG_CACHE_HOME="$scratch/.cache"

got=$("$bin" --version | head -1)
[ "$got" = "ebman $want" ] || fail "--version said '$got', expected 'ebman $want'"

"$bin" --help >/dev/null || fail "--help exited non-zero"

completions=$("$bin" completions bash) || fail "completions bash exited non-zero"
[ -n "$completions" ] || fail "completions bash printed nothing"

# MCP over stdio against the demo fleet. The server answers each line and
# exits at end of input; a watchdog stops a hung one rather than the job.
out="$scratch/mcp.out"
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_environments","arguments":{}}}' \
  | "$bin" mcp serve --demo >"$out" 2>/dev/null &
pid=$!
sleep 30 &
watchdog=$!
# Whichever ends first: the server, or the watchdog's sleep.
while kill -0 "$pid" 2>/dev/null && kill -0 "$watchdog" 2>/dev/null; do sleep 0.2; done
if kill -0 "$pid" 2>/dev/null; then
  kill "$pid" 2>/dev/null || true
  fail "MCP: the server did not finish within 30s"
fi
kill "$watchdog" 2>/dev/null || true
wait "$watchdog" 2>/dev/null || true

grep -q '"id":1,' "$out" || fail "MCP: no answer to initialize"
# The list_environments answer itself, not any line that mentions it:
# an error reply naming the env would pass a whole-output grep.
reply=$(grep '"id":2,' "$out") || fail "MCP: no answer to list_environments"
case "$reply" in *'"result"'*) ;; *) fail "MCP: list_environments answered without a result" ;; esac
case "$reply" in *'"isError":true'*) fail "MCP: list_environments returned an error" ;; esac
case "$reply" in *poly-prod-api*) ;; *) fail "MCP: list_environments did not return the demo fleet" ;; esac
case "$reply" in *firing_alarms*) ;; *) fail "MCP: list_environments lacks firing_alarms" ;; esac

echo "smoke: ok — $bin is ebman $want"
