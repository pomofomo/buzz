# Self-hosted Buzz agent image: Claude runtime + Buzz tools.
#
# Build context is the REPO ROOT (see docker-compose.yml `context: ../..`), so
# the harness and tool binaries are built from source in this repo.

# ---- Stage 1: build the Rust binaries ----
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release \
      -p buzz-cli \
      -p buzz-acp \
      -p buzz-dev-mcp \
      -p git-credential-nostr \
      -p git-sign-nostr

# ---- Stage 2: runtime ----
FROM node:22-bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates ripgrep \
 && rm -rf /var/lib/apt/lists/*

# ACP adapter for Claude (swap for @agentclientprotocol/codex-acp to run Codex)
RUN npm i -g @agentclientprotocol/claude-agent-acp

# Buzz harness + tools on PATH
COPY --from=build /src/target/release/buzz              /usr/local/bin/buzz
COPY --from=build /src/target/release/buzz-acp          /usr/local/bin/buzz-acp
COPY --from=build /src/target/release/buzz-dev-mcp      /usr/local/bin/buzz-dev-mcp
COPY --from=build /src/target/release/git-credential-nostr /usr/local/bin/git-credential-nostr
COPY --from=build /src/target/release/git-sign-nostr    /usr/local/bin/git-sign-nostr

# Nostr-authenticated git (clone/push to the relay's git server, keyless to you)
RUN git config --system credential.helper nostr \
 && git config --system credential.useHttpPath true

# Non-root user + persistent workspace
RUN useradd -m agent
USER agent
WORKDIR /home/agent/workspace
VOLUME ["/home/agent/workspace"]

ENTRYPOINT ["buzz-acp"]
