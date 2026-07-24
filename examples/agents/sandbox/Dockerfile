# A sandboxed Claude agent image for Buzz.
#
# Rust base (so the agent can build/test Rust in the Buzz workspace) + Node
# (for the Claude ACP adapter) + the Buzz CLI tools. At runtime this image is
# launched by claude-sandbox.sh with a locked-down `docker run` (read-only
# rootfs, dropped caps, caller UID, only the repos dir mounted).
#
# Build context is the REPO ROOT (see README), so the buzz binaries are built
# from source in this repo.

# ---- Stage 1: build the buzz binaries the agent needs inside the sandbox ----
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release \
      -p buzz-cli \
      -p buzz-dev-mcp \
      -p git-credential-nostr \
      -p git-sign-nostr

# ---- Stage 2: runtime ----
FROM rust:1-bookworm

# NOTE: verify these package names against the npm registry before building.
#   - Claude Code CLI:  @anthropic-ai/claude-code   (command: claude)
#   - ACP adapter:      @agentclientprotocol/claude-agent-acp  (command: claude-agent-acp)
#     The Zed adapter is an alternative: @zed-industries/claude-code-acp
#     (command: claude-code-acp) — set ADAPTER_CMD to match if you use it.
ARG CLAUDE_CLI_PKG="@anthropic-ai/claude-code"
ARG ACP_ADAPTER_PKG="@agentclientprotocol/claude-agent-acp"
ENV ADAPTER_CMD="claude-agent-acp"

RUN apt-get update && apt-get install -y --no-install-recommends \
      curl git ca-certificates ripgrep jq \
 && curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
 && apt-get install -y --no-install-recommends nodejs \
 && rm -rf /var/lib/apt/lists/*

RUN npm install -g "$CLAUDE_CLI_PKG" "$ACP_ADAPTER_PKG"

# Buzz tools: `buzz` (relay/chat/repos), dev-MCP, and the Nostr git helpers.
COPY --from=build /src/target/release/buzz                 /usr/local/bin/buzz
COPY --from=build /src/target/release/buzz-dev-mcp         /usr/local/bin/buzz-dev-mcp
COPY --from=build /src/target/release/git-credential-nostr /usr/local/bin/git-credential-nostr
COPY --from=build /src/target/release/git-sign-nostr       /usr/local/bin/git-sign-nostr

COPY examples/agents/sandbox/entrypoint.sh /usr/local/bin/entrypoint
RUN chmod +x /usr/local/bin/entrypoint

# A default user for the `--user` is-not-passed case. When claude-sandbox.sh runs
# with `--user $(id -u):$(id -g)` this is bypassed and HOME is forced to /state.
RUN useradd -m agent
USER agent
WORKDIR /work
ENTRYPOINT ["/usr/local/bin/entrypoint"]
