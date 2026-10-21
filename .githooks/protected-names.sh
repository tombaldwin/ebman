# Shared by the commit-msg and pre-commit hooks: does any text name a
# protected client? The list is the one `cargo test`'s guard reads
# (`src/app/tests/scan.rs`): `.protected-names` at the repo root, plus
# the EBMAN_PROTECTED_NAMES variable. One name per line or
# comma-separated, `#` comment lines, matched case-insensitively as a
# substring; a name under four characters is an error.
#
# Reports which entry matched by its position, never the name: a hook's
# output ends up in terminals, logs and agent transcripts.

protected_names() {
  {
    [ -f .protected-names ] && cat .protected-names
    printf '%s\n' "${EBMAN_PROTECTED_NAMES:-}"
  } | grep -v '^[[:space:]]*#' | tr ',' '\n' \
    | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' \
    | tr '[:upper:]' '[:lower:]' | awk 'NF && !seen[$0]++'
}

# check_protected_names WHAT FILE: exit 1 if FILE names a protected
# client, naming WHAT and the entry's position.
check_protected_names() {
  what=$1
  file=$2
  names=$(protected_names) || return 2
  [ -n "$names" ] || return 0
  lower=$(tr '[:upper:]' '[:lower:]' < "$file")
  k=0
  found=0
  while IFS= read -r name; do
    k=$((k + 1))
    if [ "${#name}" -lt 4 ]; then
      echo "protected-names: entry #$k is shorter than 4 characters and would match ordinary words" >&2
      return 2
    fi
    if printf '%s' "$lower" | grep -qF -- "$name"; then
      echo "protected-names: $what names protected client #$k" >&2
      found=1
    fi
  done <<NAMES
$names
NAMES
  if [ "$found" = 1 ]; then
    echo "Use the poly-* placeholder convention, or \"a production fleet\"." >&2
    return 1
  fi
  return 0
}
