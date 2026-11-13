#!/usr/bin/env bash
# Deploy the committed HEAD of attune-server to the relay host.
#
#   scripts/deploy.sh            build HEAD on the server, swap it in, restart
#   scripts/deploy.sh rollback   put the previous binary back and restart
#
# Settings: ATTUNE_HOST=user@host, from the environment or from the ignored
# file scripts/deploy.env (one line: ATTUNE_HOST=user@host).
#
# The server keeps running while the new build compiles; only the restart
# interrupts it, and that ends every live session (sessions are in memory).
set -euo pipefail

cd "$(dirname "$0")/.."
if [ -z "${ATTUNE_HOST:-}" ] && [ -f scripts/deploy.env ]; then
  # shellcheck disable=SC1091
  source scripts/deploy.env
fi
if [ -z "${ATTUNE_HOST:-}" ]; then
  echo "Set ATTUNE_HOST=user@host (or put it in scripts/deploy.env)." >&2
  exit 1
fi
HOST="$ATTUNE_HOST"
HEALTH_URL="http://${HOST#*@}:8787/healthz"

health() {
  for _ in 1 2 3 4 5; do
    if [ "$(curl -s -m 3 "$HEALTH_URL" || true)" = "ok" ]; then
      echo "Healthy: $HEALTH_URL"
      return 0
    fi
    sleep 1
  done
  echo "Health check FAILED: $HEALTH_URL (see: ssh $HOST tail ~/attune/server.log)" >&2
  return 1
}

if [ "${1:-}" = "rollback" ]; then
  ssh "$HOST" 'set -e; cd ~/attune; test -f attune-server.prev
    cp attune-server.prev attune-server
    systemctl --user restart attune'
  health
  exit
fi

if [ -n "$(git status --porcelain)" ]; then
  echo "Note: uncommitted changes are NOT deployed; deploying HEAD only." >&2
fi
COMMIT="$(git rev-parse --short HEAD)"
TAR="$(mktemp -t attune-src.XXXXXX.tar)"
trap 'rm -f "$TAR"' EXIT

echo "==> Packaging $COMMIT"
git archive --format=tar -o "$TAR" HEAD

echo "==> Uploading to $HOST"
scp -q "$TAR" "$HOST:attune/src.tar"

echo "==> Building on the server (old server keeps running)"
ssh "$HOST" 'set -e; cd ~/attune
  # Fresh source tree, but keep target/ so the build is incremental.
  if [ -d src/target ]; then mv src/target target.keep; fi
  rm -rf src; mkdir src; tar -xf src.tar -C src
  if [ -d target.keep ]; then mv target.keep src/target; fi
  source ~/.cargo/env
  cd src && cargo build --release -p attune-server'

echo "==> Installing and restarting"
ssh "$HOST" "set -e; cd ~/attune
  mkdir -p ~/.config/systemd/user
  cp src/server/deploy/attune.service ~/.config/systemd/user/attune.service
  systemctl --user daemon-reload
  systemctl --user enable attune >/dev/null 2>&1
  # Stop a copy started by hand (before the service existed).
  if ! systemctl --user is-active --quiet attune; then pkill -x attune-server || true; sleep 1; fi
  if [ -f attune-server ]; then cp attune-server attune-server.prev; fi
  cp src/target/release/attune-server attune-server.new
  mv attune-server.new attune-server
  echo $COMMIT > DEPLOYED_COMMIT
  systemctl --user restart attune"

health
echo "Deployed $COMMIT. Roll back with: scripts/deploy.sh rollback"
