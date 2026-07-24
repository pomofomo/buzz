#!/usr/bin/env bash
#
# BUZZ_ACP_AGENT_COMMAND target — the sandbox choke point.
#
# The buzz-acp harness execs this exactly as if it were the agent binary. ACP is
# JSON-RPC over stdio, so we run `docker run -i` and Claude executes INSIDE a
# locked-down container while the harness pipes prompts over stdin/stdout.
#
# The harness passes this script its FULL environment (including every secret it
# holds). Nothing crosses into the container except the vars explicitly listed
# in the `-e` flags below. That is the whole point: the host stays invisible;
# the box sees only this agent's own identity + a repos checkout.
#
# Required env (set by run-agent.sh / your launcher):
#   BUZZ_PRIVATE_KEY   the agent's own nsec (its relay + git identity)
#   BUZZ_RELAY_URL     relay to connect to
#   BUZZ_SANDBOX_REPOS host dir to mount read-write at /work/REPOS
# Optional:
#   BUZZ_AUTH_TAG              NIP-OA owner-attestation credential
#   BUZZ_SANDBOX_CLAUDE_DIR    host ~/.claude (API-key source), default $HOME/.claude
#   CLAUDE_SANDBOX_IMAGE       image tag, default buzz-claude-sandbox:latest
#   HTTPS_PROXY/HTTP_PROXY/NO_PROXY   egress allowlist proxy (see README)
set -euo pipefail

IMAGE="${CLAUDE_SANDBOX_IMAGE:-buzz-claude-sandbox:latest}"
REPOS_DIR="${BUZZ_SANDBOX_REPOS:?set BUZZ_SANDBOX_REPOS to the repos dir to mount}"
CLAUDE_DIR="${BUZZ_SANDBOX_CLAUDE_DIR:-$HOME/.claude}"

# git-credential-nostr / git-sign-nostr read NOSTR_PRIVATE_KEY; mirror identity.
export NOSTR_PRIVATE_KEY="${NOSTR_PRIVATE_KEY:-${BUZZ_PRIVATE_KEY:-}}"

NAME="buzz-claude-$$"
cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT
trap 'cleanup; exit 143' TERM INT

docker run --rm --init -i --name "$NAME" \
  --user "$(id -u):$(id -g)" \
  --read-only \
  --tmpfs /state:rw,size=512m,mode=1777 \
  --tmpfs /tmp:rw,size=256m,mode=1777 \
  -e HOME=/state \
  -e CLAUDE_CONFIG_DIR=/state/.claude \
  -e CARGO_HOME=/state/.cargo \
  -e npm_config_cache=/state/.npm \
  --cap-drop ALL \
  --security-opt no-new-privileges \
  --pids-limit 512 \
  --memory 6g \
  --cpus 3 \
  -v "$REPOS_DIR":/work/REPOS \
  -v "$CLAUDE_DIR":/host-claude:ro \
  -w /work \
  -e BUZZ_PRIVATE_KEY \
  -e NOSTR_PRIVATE_KEY \
  -e BUZZ_RELAY_URL \
  -e BUZZ_AUTH_TAG \
  -e HTTPS_PROXY \
  -e HTTP_PROXY \
  -e NO_PROXY \
  --add-host host.docker.internal:host-gateway \
  "$IMAGE" &

wait $!
