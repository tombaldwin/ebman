#!/usr/bin/env bash
# Bump Homebrew formulae for a new ebman release.
#
# Usage:
#   scripts/update-formula.sh v0.8.0
#
# For the given tag, downloads the four release tarballs published by
# .github/workflows/release.yml from the matching GitHub Release, computes
# SHA-256s, and rewrites the `version` + four `sha256` fields in:
#   - ./Formula/ebman.rb                       (in this repo)
#   - ../homebrew-tap/Formula/ebman.rb         (sibling tap repo)
#
# Both files share an identical shape, so a single set of sed expressions
# updates both. The tap repo isn't committed/pushed automatically — the
# caller is expected to review the diff and `cd ../homebrew-tap && git
# commit && git push` once happy.
#
# Pre-reqs: gh CLI authed, the v<X.Y.Z> GitHub Release exists with all
# four tarballs attached (the release workflow does this on tag push).

set -euo pipefail

if [ $# -ne 1 ]; then
  echo "usage: $0 vX.Y.Z" >&2
  exit 2
fi

TAG="$1"
VERSION="${TAG#v}"

if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: expected vX.Y.Z, got $TAG" >&2
  exit 2
fi

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TAP_FORMULA="$REPO_ROOT/../homebrew-tap/Formula/ebman.rb"
REPO_FORMULA="$REPO_ROOT/Formula/ebman.rb"

if [ ! -f "$TAP_FORMULA" ]; then
  echo "error: tap formula not found at $TAP_FORMULA" >&2
  echo "       expected sibling clone of tombaldwin/homebrew-tap" >&2
  exit 1
fi

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

# Bash 3.2 (macOS default) doesn't have associative arrays, so the three
# target SHAs are kept as plain variables.
fetch_sha() {
  local target="$1"
  local tarball="ebman-${TAG}-${target}.tar.gz"
  echo "fetching $tarball …" >&2
  if ! gh release download "$TAG" --repo tombaldwin/ebman --pattern "$tarball" --dir "$tmpdir" 2>/dev/null; then
    echo "error: $tarball not found on release $TAG" >&2
    echo "       has the release workflow finished building all targets?" >&2
    exit 1
  fi
  local sha
  sha="$(shasum -a 256 "$tmpdir/$tarball" | awk '{print $1}')"
  echo "  $sha  $tarball" >&2
  printf '%s' "$sha"
}

SHA_AARCH64_DARWIN="$(fetch_sha aarch64-apple-darwin)"
SHA_X86_DARWIN="$(fetch_sha x86_64-apple-darwin)"
SHA_X86_LINUX="$(fetch_sha x86_64-unknown-linux-gnu)"
SHA_AARCH64_LINUX="$(fetch_sha aarch64-unknown-linux-gnu)"

# Rewrite the version line + each target's sha256. The url line uses
# `#{version}` interpolation so it doesn't need touching.
rewrite() {
  local f="$1"
  # macOS sed needs `-i ''` (BSD); GNU sed wants `-i`. Detect.
  local sed_inplace=(-i "")
  if sed --version >/dev/null 2>&1; then
    sed_inplace=(-i)
  fi
  sed "${sed_inplace[@]}" -E "s/^  version \"[^\"]+\"$/  version \"${VERSION}\"/" "$f"
  # Replace each target's sha256. The line preceding the sha256 contains
  # the target triple, so anchor on that with awk for safety rather than
  # relying on sed's address ranges.
  awk -v aarch="$SHA_AARCH64_DARWIN" \
      -v xdarwin="$SHA_X86_DARWIN" \
      -v xlinux="$SHA_X86_LINUX" \
      -v alinux="$SHA_AARCH64_LINUX" '
    /aarch64-apple-darwin.tar.gz/     { print; getline; sub(/sha256 "[^"]+"/, "sha256 \"" aarch "\""); print; next }
    /x86_64-apple-darwin.tar.gz/      { print; getline; sub(/sha256 "[^"]+"/, "sha256 \"" xdarwin "\""); print; next }
    /aarch64-unknown-linux-gnu.tar.gz/ { print; getline; sub(/sha256 "[^"]+"/, "sha256 \"" alinux "\""); print; next }
    /x86_64-unknown-linux-gnu.tar.gz/ { print; getline; sub(/sha256 "[^"]+"/, "sha256 \"" xlinux "\""); print; next }
    { print }
  ' "$f" > "$f.tmp" && mv "$f.tmp" "$f"
}

rewrite "$REPO_FORMULA"
rewrite "$TAP_FORMULA"

echo
echo "updated $REPO_FORMULA"
echo "updated $TAP_FORMULA"
echo
echo "next steps:"
echo "  git -C $REPO_ROOT diff Formula/ebman.rb"
echo "  git -C $(dirname "$(dirname "$TAP_FORMULA")") diff Formula/ebman.rb"
echo "  # if happy, commit + push both repos"
