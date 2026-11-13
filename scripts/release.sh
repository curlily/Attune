#!/usr/bin/env bash
# Release a new version of the desktop app.
#
#   scripts/release.sh 0.2.0
#
# Sets the version everywhere, commits, makes an annotated tag (your editor
# opens for the release notes: the first line is the title, the rest are
# the notes shown in the update offer) and pushes master and the tag. The
# Release workflow then builds, signs and publishes it.
set -euo pipefail

VERSION="${1:-}"
if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Usage: scripts/release.sh <major.minor.patch>" >&2
  exit 1
fi
cd "$(dirname "$0")/.."

if [ "$(git branch --show-current)" != "master" ]; then
  echo "Release from master." >&2
  exit 1
fi
if [ -n "$(git status --porcelain)" ]; then
  echo "Commit or stash your changes first." >&2
  exit 1
fi
if git rev-parse -q --verify "refs/tags/v$VERSION" >/dev/null; then
  echo "v$VERSION already exists." >&2
  exit 1
fi

echo "==> Setting the version to $VERSION"
node -e '
  const fs = require("fs");
  const path = "src-tauri/tauri.conf.json";
  const config = JSON.parse(fs.readFileSync(path, "utf8"));
  config.version = process.argv[1];
  fs.writeFileSync(path, JSON.stringify(config, null, 2) + "\n");
' "$VERSION"
npm version "$VERSION" --no-git-tag-version --allow-same-version >/dev/null
sed -i "0,/^version = \".*\"/s//version = \"$VERSION\"/" src-tauri/Cargo.toml
cargo metadata --format-version 1 >/dev/null # updates Cargo.lock

git add src-tauri/tauri.conf.json src-tauri/Cargo.toml package.json package-lock.json Cargo.lock
git commit -q -m "Release v$VERSION"
git tag -a "v$VERSION" -e -m "Attune $VERSION"

echo "==> Pushing"
git push origin master "v$VERSION"
echo "Pushed v$VERSION. Watch the build: gh run watch"
