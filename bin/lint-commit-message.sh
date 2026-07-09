#!/usr/bin/env bash
# Validates a commit message against the house convention:
#   PREFIX: imperative English description
# Allowed prefixes: FEAT FIX CHORE DOCS OPS CI SECURITY REFACTOR PERF TEST STYLE
# Merges, reverts and fixups pass.
# Usage: lint-commit-message.sh <file-with-message>  or  echo "msg" | lint-commit-message.sh -
set -euo pipefail

if [ "${1:-}" = "-" ]; then
  subject="$(head -n1)"
else
  subject="$(head -n1 "${1:?Usage: lint-commit-message.sh <message-file>|-}")"
fi

case "$subject" in
  Merge\ *|Revert\ *|fixup!\ *|squash!\ *) exit 0 ;;
esac

if printf '%s' "$subject" | grep -Eq '^(FEAT|FIX|CHORE|DOCS|OPS|CI|SECURITY|REFACTOR|PERF|TEST|STYLE): .+' ; then
  exit 0
fi

cat >&2 <<MSG
✗ Commit message does not follow the house convention.

  Got:      $subject
  Expected: PREFIX: imperative English description

  Allowed prefixes: FEAT FIX CHORE DOCS OPS CI SECURITY REFACTOR PERF TEST STYLE
  Examples:         FEAT: add junit-xml parsing for the pytest preset
                     FIX: correct slowest-test ranking when durations tie
                     DOCS: document the pytest preset flags in README
MSG
exit 1
