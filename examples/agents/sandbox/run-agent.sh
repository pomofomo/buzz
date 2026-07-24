#!/usr/bin/env bash
#
# Launch the buzz-acp harness on the HOST, pointing the agent runtime at the
# docker wrapper so Claude runs sandboxed. The harness itself is trusted (it
# holds the agent's key and speaks to the relay); only Claude's tool execution
# is boxed.
#
# Prereqs:
#   - `buzz-acp` on PATH (build: cargo build --release -p buzz-acp)
#   - the sandbox image built:  see README ("Build the image")
#   - env below exported (or sourced from a .env you keep out of git)
set -euo pipefail

: "${BUZZ_RELAY_URL:?relay URL}"
: "${BUZZ_PRIVATE_KEY:?the agent nsec (its own identity)}"
: "${BUZZ_ACP_AGENT_OWNER:?your 64-char hex pubkey (owner-only gate)}"
: "${BUZZ_SANDBOX_REPOS:?host dir to mount into the sandbox at /work/REPOS}"

here="$(cd "$(dirname "$0")" && pwd)"
export BUZZ_SANDBOX_REPOS
export CLAUDE_SANDBOX_IMAGE="${CLAUDE_SANDBOX_IMAGE:-buzz-claude-sandbox:latest}"

exec buzz-acp \
  --agent-command "$here/claude-sandbox.sh" \
  --system-prompt-file "$here/agent.system.md" \
  --respond-to owner-only \
  --subscribe mentions \
  --permission-mode bypassPermissions
# Note: bypassPermissions is intentional and SAFE here — the container is the
# safety boundary, not a per-tool prompt (no human is present to answer one).
# The agent can touch only /work/REPOS and this agent own secrets; everything
# else on the host is invisible to it.
