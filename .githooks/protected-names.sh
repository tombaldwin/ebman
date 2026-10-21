# Shared by the commit-msg and pre-commit hooks: does any text name a
# protected client? The list is the one `cargo test`'s guard reads
# (`src/app/tests/scan.rs`): `.protected-names` at the repo root, plus
# the EBMAN_PROTECTED_NAMES variable. One name per line or
# comma-separated, `#` comment lines, matched case-insensitively as a
# substring; a name under four characters is an error.
#
# Reports which entry matched by its position, never the name: a hook's
# output ends up in terminals, logs and agent transcripts.

# The list file: this checkout's, else the main checkout's. A linked
# worktree has no copy (the file is gitignored), and without the
# fallback every check there passed on an empty list.
protected_names_file() {
  top=$(git rev-parse --show-toplevel 2>/dev/null) || top=.
  if [ -f "$top/.protected-names" ]; then
    echo "$top/.protected-names"
    return
  fi
  common=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || return
  if [ -f "$common/../.protected-names" ]; then
    echo "$common/../.protected-names"
  fi
}

protected_names() {
  {
    file=$(protected_names_file)
    [ -n "$file" ] && cat "$file"
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
  if [ -z "$names" ]; then
    # Not an error (a contributor has no list), but never silent: a
    # check that ran on nothing must not read as a check that passed.
    echo "protected-names: no list (.protected-names or EBMAN_PROTECTED_NAMES): $what NOT checked" >&2
    return 0
  fi
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
