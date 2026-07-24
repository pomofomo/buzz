#!/usr/bin/env bash
# Container entrypoint for the sandboxed Claude agent.
#
# The harness talks ACP (JSON-RPC over stdio) to this process, so the last line
# MUST exec the adapter with stdin/stdout untouched.
set -euo pipefail

# 1. Seed a WRITABLE Claude config from the READ-ONLY host mount.
#    We mount ~/.claude read-only at /host-claude purely as the credential
#    source, then copy it into a writable tmpfs dir so Claude can still write
#    session/todo state without touching (or leaking back into) the host.
if [ -d /host-claude ]; then
  mkdir -p "$CLAUDE_CONFIG_DIR"
  cp -a /host-claude/. "$CLAUDE_CONFIG_DIR"/ 2>/dev/null || true
fi

# 2. Nostr-authenticated git for cloning/pushing to the relay's git server.
#    Writes to $HOME/.gitconfig, which lives on the writable /state tmpfs.
if command -v git-credential-nostr >/dev/null 2>&1; then
  git config --global credential.helper "$(command -v git-credential-nostr)"
  git config --global credential.useHttpPath true
fi

# 3. Hand off to the Claude ACP adapter. `exec` so signals + stdio pass through.
exec "${ADAPTER_CMD:-claude-agent-acp}" "$@"
