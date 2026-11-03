#!/bin/sh
# PS-ENG-02 guard: the pure engine crates must never depend on tokio/gpui/
# reqwest/IO stacks, directly or through another crate. It reads the resolved
# dependency graph (cargo tree), so a comment, a description or a string that
# merely mentions a name is not a dependency, and a dependency is matched by
# its exact package name. A graph that cannot be read fails the check.
set -eu
cd "$(dirname "$0")/.."
BAD='^(tokio|gpui|reqwest|hyper|async-std|smol|mio)$'
for crate in awili-engine awili-engine-authz; do
  if ! tree=$(cargo tree --locked -q -p "$crate" -e normal,build --prefix none --format '{p}'); then
    echo "purity: the dependency graph of $crate could not be read" >&2
    exit 1
  fi
  names=$(printf '%s\n' "$tree" | awk 'NF { print $1 }')
  if [ -z "$names" ]; then
    echo "purity: the dependency graph of $crate is empty" >&2
    exit 1
  fi
  if printf '%s\n' "$names" | grep -Eq "$BAD"; then
    printf '%s\n' "$names" | grep -E "$BAD" >&2
    echo "PURITY VIOLATION: forbidden dependency in the pure engine crate $crate" >&2
    exit 1
  fi
done
echo "awili-engine purity: OK"
