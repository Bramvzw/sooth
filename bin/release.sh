#!/usr/bin/env bash
#
# Roll CHANGELOG.md's [Unreleased] section into a new version, tag it, and
# push. The crate publish itself happens in CI (.github/workflows/release.yml)
# once the tag lands — never run `cargo publish` from a local machine.
#
#   make release             # bump patch (0.1.0 -> 0.1.1)
#   make release bump=minor  # bump minor (0.1.0 -> 0.2.0)
#   make release bump=major  # bump major (0.1.0 -> 1.0.0)
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

BUMP="${1:-patch}"

step() { printf '\n\033[36m==> %s\033[0m\n' "$1"; }

if [ -n "$(git status --porcelain)" ]; then
  echo "✗ Working tree is not clean. Commit or stash first." >&2
  exit 1
fi

LAST_TAG="$(git describe --tags --abbrev=0 --match 'v*' 2>/dev/null || echo v0.0.0)"
VERSION="$(printf '%s' "$LAST_TAG" | awk -F. -v bump="$BUMP" '{
  sub(/^v/, "", $1);
  major = $1; minor = $2; patch = $3;
  if (bump == "major") { major++; minor = 0; patch = 0; }
  else if (bump == "minor") { minor++; patch = 0; }
  else { patch++; }
  printf "v%d.%d.%d", major, minor, patch;
}')"

step "1/4  Rolling CHANGELOG.md into $VERSION"
if awk '/^## \[Unreleased\]/{found=1; next} /^## \[/{exit} found && NF {exit 42}' CHANGELOG.md; then
  echo "    WARNING: no entries under [Unreleased] — release notes will be empty"
fi
perl -0pi -e "s/^## \[Unreleased\]/## [Unreleased]\n\n## [${VERSION#v}] - $(date '+%Y-%m-%d')/m" CHANGELOG.md

step "2/4  Bumping Cargo.toml version to ${VERSION#v}"
perl -pi -e "s/^version = \".*\"\$/version = \"${VERSION#v}\"/ if $. <= 5" Cargo.toml
cargo update --workspace --offline >/dev/null 2>&1 || true

step "3/4  Committing"
git add CHANGELOG.md Cargo.toml Cargo.lock
git commit -m "CHORE: release $VERSION"

step "4/4  Tagging $VERSION and pushing"
git tag -a "$VERSION" -m "$VERSION"
git push origin HEAD "$VERSION"

if command -v gh >/dev/null 2>&1; then
  notes="$(awk "/^## \[${VERSION#v}\]/{found=1; next} found && /^## \[/{exit} found" CHANGELOG.md)"
  gh release create "$VERSION" --title "$VERSION" --notes "$notes" || echo "    WARNING: gh release failed (continuing)"
fi

echo
echo "Tag $VERSION pushed. CI publishes the crate to crates.io from the tag."
