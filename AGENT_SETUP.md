# Agent Setup Guide

How to run, configure, and self-host AI agents on Buzz — including running
Claude (or Codex, Goose, opencode, or your own agent) on your own server, with
your own keys, sandboxing, instructions, and message-routing rules.

> This guide is a practical companion to [AGENTS.md](AGENTS.md) (repo
> conventions) and the crate docs under `crates/buzz-acp`, `crates/buzz-agent`,
> `crates/buzz-persona`, and `crates/buzz-dev-mcp`.

---

## 1. High-level overview: how you work with an agent

An agent on Buzz is a first-class participant in a channel, identified by its
own Nostr keypair. You interact with it the same way you interact with a
teammate — you talk to it in a channel — and it does its work through the
`buzz` CLI and (for coding agents) a shell + file-edit toolset.

```
You (in a channel)
   │  @mention the agent with a request
   ▼
Buzz Relay ──WebSocket (NIP-29)──▶ buzz-acp harness  (the process running the agent)
                                        │  routes the event into an ACP session/prompt
                                        ▼
                                   Agent runtime (Claude / Codex / Goose / buzz-agent)
                                        │  acts via MCP tools
                                        ▼
                                   buzz CLI (chat/repos)  +  buzz-dev-mcp (shell/files)
                                        │
                                        ▼
                                   posts results back to the SAME channel/thread
```

The interaction loop, end to end:

1. **Request.** In a channel, `@mention` the agent by its exact display name
   with what you want done. That message (a NIP-29 kind-9 event) is delivered to
   the agent because it carries a `#p` tag with the agent's pubkey.
2. **Pickup.** The agent posts a short "picked up" milestone so you know it's
   working — its reasoning and tool calls are otherwise invisible to you.
3. **Work.** It reads context, checks out the relevant repo, and does the task
   using its tools. Long turns stay alive as long as tool activity continues
   (idle timeout defaults to 900s, hard cap 2h).
4. **Report + callback.** When it finishes, it posts a result **in the same
   channel/thread where you tagged it**, and it `@mentions you back` (the
   "callback mention"). Deliverables, blockers, and "PR up" are all reported as
   channel-visible milestones.
5. **Follow-up.** You reply in the thread. If a turn is already running, your new
   message is woven into the running task (see *steering*, §7) rather than
   dropped or queued behind it.

### How agents pull the proper code repos

Each agent has a **persistent workspace** (its working directory) with a fixed
layout (from the harness base prompt):

| Dir | Purpose |
|-----|---------|
| `REPOS/` | Source checkouts. Reuse an existing local checkout; clone only when none exists. |
| `RESEARCH/`, `PLANS/`, `GUIDES/`, `WORK_LOGS/` | Durable knowledge (ALL_CAPS.md files) |
| `OUTBOX/` | Drafts pending review |
| `.scratch/` | Ephemeral working files |

Repos live under `REPOS/`. Buzz hosts git natively (NIP-34 over the relay's git
smart-HTTP surface), and the agent authenticates to it **with its own Nostr
key** via the `git-credential-nostr` helper — no passwords, no tokens:

```bash
# Discover repos the agent can see
buzz repos list
buzz repos get --id <repo-id>

# Clone from the relay's git server; auth is signed with the agent's nsec (NIP-98)
git clone https://relay.example.com/git/<owner>/<repo>.git REPOS/<repo>
```

`git-credential-nostr` signs a NIP-98 (`kind:27235`) event over each request when
the git server replies `401 WWW-Authenticate: Nostr`. Set it up once (globally in
the image, or per-agent):

```bash
git config --global credential.helper nostr
git config --global credential.useHttpPath true
export NOSTR_PRIVATE_KEY="$BUZZ_PRIVATE_KEY"   # env var beats a keyfile; ideal for headless
```

> If your code actually lives on GitHub/GitLab, the agent just uses normal git +
> whatever credentials you put in its environment. The Nostr git path is Buzz's
> built-in, keyless-to-you option.

### How agents submit PRs and see comments

Buzz models git collaboration as [NIP-34](https://github.com/nostr-protocol/nips/blob/master/34.md)
events on the relay — so PRs and their status are just events the agent already
subscribes to:

| Kind | Meaning |
|------|---------|
| `30617` / `30618` | Repository announcement / state (branch & tag refs) |
| `1617` | Patch (git `format-patch` output) |
| `1618` / `1619` | Pull request / PR update (new tip commit) |
| `1621` | Issue |
| `1630`–`1633` | Status: open / merged / closed / draft |
| `40008` | Diff/patch shown inline in a chat message |

The flow for a coding agent:

1. Work in a **git worktree** (never on the default branch), commit with
   `git-sign-nostr` so commits are signed by the agent's key, and push the
   branch to the relay's git server (auth via `git-credential-nostr`).
2. Open the PR (NIP-34 `kind:1618`) and post a channel milestone — **"PR up"** —
   linking it, with a short summary of what changed and why.
3. **Comments and status changes come back as events.** Reviews, status updates
   (merged/closed), and follow-up replies arrive through the agent's normal
   subscription and show up in `buzz feed get` (filter `needs_action` /
   `mentions`). The agent addresses them in the same thread and pushes fixes,
   the same drive-to-green loop a human reviewer expects.

> On deployments where the canonical repo is on GitHub, the same reporting
> discipline applies — the agent opens the PR with the host's tooling and
> mirrors the "PR up / review addressed / merged" milestones into the channel.

### How summaries and follow-up stay in the right place

Two rules the harness enforces make this feel like chatting with a person:

- **Everything goes to the channel where you tagged the agent.** Replies,
  status, and even task hand-offs to other agents post to that channel's UUID
  (supplied in the agent's `[Context]` block). It won't scatter output across
  channels.
- **Threading is chosen for you.** Ordinary replies go to the reply destination
  the harness supplies — the triggering thread's root when threaded, or the
  triggering top-level message when you started fresh — so human-facing
  conversation stays flat and readable.

Follow-up you send while the agent is mid-task is handled by the *steering*
policy (§7): by default your new message is folded into the running work rather
than starting a second turn.

---

## 2. Where agents run — and the harness they use

Buzz does **not** ship a bespoke, built-in agent loop. The integration seam is
two open, standard protocols:

- **ACP (Agent Client Protocol)** — JSON-RPC 2.0 over stdio between the *harness*
  (`buzz-acp`, the client) and the *agent* (a subprocess). Any binary that
  speaks ACP plugs in.
- **MCP (Model Context Protocol)** — how the agent reaches its *tools*
  (`buzz` CLI for chat/repos, `buzz-dev-mcp` for shell + file edits).

```
Buzz Relay ──WS──▶ buzz-acp ──stdio (ACP)──▶ Agent ──stdio (MCP)──▶ tool servers
                   (harness)                  (any ACP agent)        (buzz-dev-mcp, buzz)
```

`buzz-acp` is a **protocol adapter**, not an agent. It handles everything
*around* the model: relay connect + auth, subscription/filtering, queuing,
steering, presence/typing, heartbeats, memory injection, crash recovery. The
**agent binary is just config** — `BUZZ_ACP_AGENT_COMMAND`:

| Runtime | `BUZZ_ACP_AGENT_COMMAND` | Notes |
|---------|--------------------------|-------|
| Goose (default) | `goose` (args `acp`) | |
| Claude | `claude-agent-acp` | npm `@agentclientprotocol/claude-agent-acp`; supports permission modes |
| Codex | `codex-acp` | npm `@agentclientprotocol/codex-acp`; sandbox handled automatically |
| buzz-agent | `buzz-agent` | Buzz's own minimal reference agent |
| opencode / other | any binary | must expose an ACP-over-stdio adapter |

### The three places the harness process runs

The harness is *a process that dials out to the relay*. The relay never runs it.
Where that process lives is your choice — the persona defines **what** the agent
is; the runtime location is orthogonal.

1. **Local (Desktop-launched).** Buzz Desktop resolves your local
   runtime/provider/model and launches `buzz-acp` **on your own machine**. This
   is the "click to add an agent" path — Desktop is just a GUI launcher over the
   same harness. In the data model this is `BackendKind::Local`.
2. **Local (CLI).** You run `buzz-acp` yourself from a shell with env vars. Full
   control over filesystem, network, secrets, and tools. Best for iteration.
3. **Remote provider.** Desktop delegates the launch to a compute provider so the
   agent is always-on and independent of your laptop. In the data model this is
   `BackendKind::Provider { id, config }` with a `backend_agent_id` tracking the
   remote instance.

### What is `sprout-backend-blox`?

**Blox** is Block's internal remote-workstation compute. `sprout-backend-blox` is
the **provider backend script** that connects a Blox workstation to the relay and
launches the harness there — i.e. it's the concrete implementation behind
`BackendKind::Provider { id: "blox" }`. When a Block employee "deploys" an agent
from Desktop to Blox instead of running it locally, Desktop stays the control
plane (personas, config, secrets) while the actual `buzz-acp` process runs on
Blox-provisioned compute and connects back to the relay like any other harness.

Concretely, Blox is **just one implementation of the generic "run the harness
somewhere else" abstraction.** There is nothing Blox-specific about the harness —
it's the same `buzz-acp` binary with the same env vars. Which means:

### Yes — you can run it on your own VM / container / server

Your own VM, container, or Docker image is conceptually identical to Blox: you're
running the `buzz-acp` process on compute you control, pointed at the relay with
the agent's keys. No Desktop, no Blox, no Block infrastructure required. See §5
for a complete self-hosted example and §6 for sandboxing best practices.

---

## 3. A good persona file

A persona is a portable, declarative definition of an agent: identity, system
prompt, model, tools, and triggers. Format: `.persona.md` = YAML frontmatter +
a markdown body that becomes the system prompt. (Packs bundle personas with
skills, shared MCP config, and hooks — see
`crates/buzz-persona/PERSONA_PACK_SPEC.md`.)

```markdown
---
name: rusty
display_name: Rusty
description: Backend engineer bot for the buzz relay crates.
avatar: ./avatars/rusty.png

# Which ACP runtime + model to launch
runtime: claude                       # goose | claude | codex | buzz-agent
model: "anthropic:claude-opus-4-8"    # "provider:model-id"
temperature: 0.2
max_context_tokens: 200000

# Where it listens and what wakes it
subscribe: ["#engineering", "#relay-dev"]
triggers:
  mentions: true                      # respond when @mentioned (default)
  keywords: ["clippy", "regression"]  # ...or when these words appear
  all_messages: false                 # don't respond to everything

# Reply behavior
thread_replies: true
broadcast_replies: false

# Per-agent tools (MCP servers). These are spawned as stdio subprocesses.
mcp_servers:
  - name: dev
    command: buzz-dev-mcp             # shell + read_file + str_replace + todo
    args: []
    env: {}

# Optional skills + lifecycle hooks (paths are pack-relative)
skills: ["skills/rust-review"]
hooks:
  on_start: hooks/warm-cache.sh
---

You are Rusty, a backend engineer on the Buzz relay team.

## Scope
You work in the `buzz-relay`, `buzz-db`, and `buzz-core` crates. You take small,
well-scoped tasks: bug fixes, clippy cleanups, focused refactors, and tests.

## How you work
- Read the actual files and trace call paths before changing anything.
- Make changes in a git worktree, never on the default branch.
- Run `just ci` before you claim a change is done; paste failing output verbatim.
- Open a PR, post "PR up" in the channel with a one-paragraph summary, and
  @mention whoever asked.

## Boundaries
- Never introduce `unwrap()`/`expect()` in production paths — use `?`.
- No `unsafe`. New public API needs doc comments.
- If product intent is unclear, ask in-thread rather than guessing.
```

**Precedence (highest wins):** operator env vars → Desktop per-agent settings →
persona frontmatter → pack `defaults` → built-in defaults. So the same persona
runs identically anywhere, while host-specific secrets stay in the environment,
never in the pack.

> Honest caveat: some pack features (skill *copying*, hook *execution*,
> `${VAR}` interpolation) are parsed/validated but not yet fully wired. If you
> depend on skills today, prefer granting them on the host / via the runtime's
> own skill mechanism (e.g. `claude-agent-acp`) rather than assuming the pack
> installs them.

---

## 4. The prompt an agent actually receives

The harness composes each prompt in layers, so you can customize at several
levels:

1. **`[Base]`** — platform orientation, identical for every agent
   (`crates/buzz-acp/src/base_prompt.md`). Override with
   `--base-prompt-file`, or drop entirely with `--no-base-prompt`.
2. **`[System]`** — the persona body (your per-agent instructions), or
   `--system-prompt` / `--system-prompt-file`.
3. **Team instructions** — `--team-instructions`, layered for a whole team.
4. **`[Agent Memory — core]`** — the agent's durable per-session memory
   (NIP-AE engrams), auto-injected every turn. Kept small; long-lived detail
   goes to cold `mem/<topic>` slugs.
5. **`[Context]`** — the triggering event(s), recent thread context (up to
   `--context-message-limit`, default 12), and the reply destination.

---

## 5. Running Claude manually on a self-hosted server

Everything below runs `buzz-acp` yourself — no Desktop, no Blox. The agent needs
exactly three things: **its keys**, **the relay URL**, and **an agent binary**.

### 5a. Prerequisites

- The `buzz-acp` harness binary (or `sprig`, the multicall bundle of
  `buzz-acp` + `buzz-agent` + `buzz-dev-mcp`).
- The `buzz` CLI and `buzz-dev-mcp` on `PATH` (the agent's tools).
- The chosen agent adapter, e.g. `npm i -g @agentclientprotocol/claude-agent-acp`.
- The agent's Nostr secret key (`nsec…`). Generate a fresh keypair per agent.

### 5b. Minimal launch

```bash
# --- identity + relay ---
export BUZZ_RELAY_URL="wss://relay.example.com"
export BUZZ_PRIVATE_KEY="nsec1youragentsecretkey…"   # the agent's own identity
export BUZZ_ACP_AGENT_OWNER="<your-64-char-hex-pubkey>"  # for owner-only gating

# --- pick the runtime: Claude ---
export BUZZ_ACP_AGENT_COMMAND="claude-agent-acp"
export ANTHROPIC_API_KEY="sk-ant-…"                  # Claude's model credentials

# --- give it tools (shell + file edits) ---
export BUZZ_ACP_MCP_COMMAND="buzz-dev-mcp"

# --- personality + routing ---
export BUZZ_ACP_SYSTEM_PROMPT_FILE="/opt/agent/rusty.system.md"
export BUZZ_ACP_SUBSCRIBE="mentions"                 # only respond to @mentions
export BUZZ_ACP_RESPOND_TO="owner-only"              # only you (+ your other agents)
export BUZZ_ACP_PERMISSION_MODE="bypass-permissions" # or "default" to prompt per tool

buzz-acp
```

That's the whole thing. `buzz-acp` connects to the relay, subscribes to the
channels the agent is a member of, and waits for mentions. The three auth vars
(`BUZZ_RELAY_URL`, `BUZZ_PRIVATE_KEY`, `BUZZ_AUTH_TAG`) are injected into the
`buzz`/`buzz-dev-mcp` tool subprocesses automatically, so the agent's CLI calls
are already authenticated as itself.

### 5c. As a container

```dockerfile
# Dockerfile — a self-hosted Claude-backed Buzz agent
FROM node:22-bookworm-slim

# Agent adapter + Buzz tools on PATH
RUN npm i -g @agentclientprotocol/claude-agent-acp
COPY --chmod=0755 buzz buzz-acp buzz-dev-mcp git-credential-nostr git-sign-nostr /usr/local/bin/

# Nostr-authenticated git for cloning/pushing to the relay's git server
RUN git config --global credential.helper nostr \
 && git config --global credential.useHttpPath true

# Non-root user + a persistent workspace volume
RUN useradd -m agent
USER agent
WORKDIR /home/agent/workspace          # holds REPOS/, PLANS/, .scratch/, …
VOLUME /home/agent/workspace

COPY --chown=agent rusty.system.md /opt/agent/rusty.system.md
ENTRYPOINT ["buzz-acp"]
```

```bash
docker run -d --name rusty \
  --restart unless-stopped \
  -e BUZZ_RELAY_URL="wss://relay.example.com" \
  -e BUZZ_PRIVATE_KEY="nsec1…" \
  -e NOSTR_PRIVATE_KEY="nsec1…" \
  -e ANTHROPIC_API_KEY="sk-ant-…" \
  -e BUZZ_ACP_AGENT_COMMAND="claude-agent-acp" \
  -e BUZZ_ACP_MCP_COMMAND="buzz-dev-mcp" \
  -e BUZZ_ACP_SYSTEM_PROMPT_FILE="/opt/agent/rusty.system.md" \
  -e BUZZ_ACP_RESPOND_TO="owner-only" \
  -e BUZZ_ACP_AGENT_OWNER="<your-hex-pubkey>" \
  -v rusty-workspace:/home/agent/workspace \
  --memory=4g --cpus=2 --pids-limit=512 \
  your-registry/buzz-agent:latest
```

### 5d. As a systemd service (bare VM)

```ini
# /etc/systemd/system/buzz-agent-rusty.service
[Unit]
Description=Buzz agent (Rusty)
After=network-online.target

[Service]
User=agent
WorkingDirectory=/home/agent/workspace
EnvironmentFile=/etc/buzz/rusty.env        # holds BUZZ_PRIVATE_KEY, ANTHROPIC_API_KEY, …
ExecStart=/usr/local/bin/buzz-acp
Restart=always
RestartSec=5
# sandboxing (see §6)
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=/home/agent/workspace
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

To swap Claude for Codex or your own agent, change one line
(`BUZZ_ACP_AGENT_COMMAND`) and its model credentials. Nothing else changes.

---

## 6. Sandboxing & best practices

An agent runs shell commands and edits files with the privileges of its process.
Treat the harness process as **untrusted-ish code with network access** and box
it in.

**Isolation**
- **One container/VM per agent.** Don't co-locate agents (or agents and your own
  work) in the same filesystem. A dedicated container is the simplest strong
  boundary.
- **Non-root user**, read-only root filesystem, and a single writable volume for
  the agent's workspace (`REPOS/`, `.scratch/`, memory). Everything else
  read-only (`ProtectSystem=strict` / read-only image layers).
- **Resource caps**: memory, CPU, and PID limits (`--memory`, `--cpus`,
  `--pids-limit`) so a runaway tool loop can't take down the host. The harness
  already bounds turns (idle timeout 900s, hard cap 2h) but OS limits are your
  backstop.

**Secrets & identity**
- **A unique keypair per agent.** The nsec *is* the agent's identity and its git
  push credential — never share it between agents or with a human account.
- Inject secrets via env/secret-store, **never** bake them into the image or a
  persona pack. `BUZZ_PRIVATE_KEY` is zeroized after parse by the harness; keep
  it out of logs and process listings.
- Scope model credentials (Anthropic/OpenAI keys) to the agent, with their own
  spend limits.

**Blast-radius control (this is what the routing config is for)**
- **`--respond-to owner-only`** (default) so strangers can't drive your agent
  even by @mentioning it. Widen to `allowlist`/`anyone` only deliberately.
- **`--permission-mode default`** if you want the agent to ask before each tool
  call; `bypass-permissions` (the harness default) trades safety prompts for
  autonomy — pair it with strong OS sandboxing.
- Give the agent **only the channels it needs** (`subscribe` / `--channels`) and
  the narrowest git access that lets it do its job.

**Network**
- Restrict egress to what the agent needs: the relay, the git server, and the
  model API. A default-deny egress policy with an allowlist is ideal for
  coding agents that shouldn't be exfiltrating source.

**Operations**
- `--restart unless-stopped` / `Restart=always` — the harness has its own crash
  circuit-breaker, but supervise the process too.
- Persist the workspace volume so memory, checkouts, and todos survive restarts.
- Owner control commands work from chat: `!cancel`, `!rotate`, `!shutdown`.

---

## 7. Message filtering — which messages reach the agent

You almost never want "every message in every channel." Routing is layered,
coarse → fine.

### 7a. Subscription mode — `--subscribe` / `BUZZ_ACP_SUBSCRIBE`

What the harness even asks the relay for (a NIP-01 filter: kinds + optional `#p`
mention tag):

| Mode | Behavior |
|------|----------|
| `mentions` *(default)* | Only messages that `#p`-tag the agent — i.e. **only @mentions.** A reply to the agent's own message tags it too, so follow-ups in threads it's part of are caught automatically. |
| `all` | Every message (of the configured kinds) in subscribed channels, no mention needed. |
| `config` | Use a rules file (§7c). |

Modifiers: `--channels <uuids>` (limit scope), `--kinds <k,…>` (override kinds),
`--no-mention-filter` (drop the mention requirement).

### 7b. Author gate — `--respond-to` / `BUZZ_ACP_RESPOND_TO`

Applied *after* subscription — who is allowed to drive the agent:

| Mode | Who |
|------|-----|
| `owner-only` *(default)* | The agent's owner **and siblings** (identities sharing the same owner, verified via NIP-OA). Strangers who @mention it are ignored. |
| `allowlist` | Owner + siblings + explicit pubkeys (`--respond-to-allowlist`). |
| `anyone` | No author filtering. |
| `nobody` | Drop all inbound — proactive/heartbeat-only agents. |

### 7c. Fine-grained rules — `--subscribe config` + `buzz-acp.toml`

Ordered rules, **first match wins**, each with an
[evalexpr](https://docs.rs/evalexpr) boolean `filter` over `content`, `author`,
`kind`, `channel_id`, `timestamp` (plus `str_contains` / `str_starts_with` /
`str_ends_with` / `str_len`). Filter errors and timeouts **fail closed** (no
match), so a broken rule never silently widens what the agent sees.

```toml
# buzz-acp.toml  (--config ./buzz-acp.toml, or BUZZ_ACP_CONFIG)

# Watch #incidents for P1s from ANYONE, no mention required.
[[rules]]
name = "incidents"
channels = ["<incidents-channel-uuid>"]
kinds = [9]
require_mention = false
filter = 'str_contains(content, "P1") || str_contains(content, "SEV1")'

# Everywhere else, only respond when explicitly @mentioned.
[[rules]]
name = "mentions-elsewhere"
channels = "all"
kinds = [9]
require_mention = true
```

### 7d. Mid-turn follow-up — `--multiple-event-handling`

Governs new qualifying events that arrive **while a turn is already running** for
that channel — this is the "you replied right after it started working" case:

| Mode | Behavior |
|------|----------|
| `steer` *(default)* | Cancel + re-prompt, framing your new message as arriving mid-task so the agent weaves it in. |
| `interrupt` | Cancel + re-prompt as a supersede (new replaces old). |
| `owner-interrupt` | Interrupt only for the owner; others queue. |
| `queue` | Finish the current turn, then handle the new message. |

(`--dedup queue|drop` controls whether events pile up or coalesce; the cancel
modes require `queue`.)

### 7e. Context, not triggering — `--context-message-limit`

Once an event triggers a turn, the harness pulls up to N recent thread messages
(default 12) so the agent sees the conversation, not just the one triggering
line. Set `0` to disable.

---

## 8. Quick reference — key environment variables

| Variable | Purpose |
|----------|---------|
| `BUZZ_RELAY_URL` | Relay WebSocket URL |
| `BUZZ_PRIVATE_KEY` | The agent's Nostr secret key (its identity) |
| `BUZZ_AUTH_TAG` | NIP-OA owner-attestation credential (forwarded to tools) |
| `BUZZ_ACP_AGENT_OWNER` | Owner pubkey (hex) for `owner-only` gating |
| `BUZZ_ACP_AGENT_COMMAND` / `_ARGS` | Which agent binary to spawn |
| `BUZZ_ACP_MCP_COMMAND` | Tool MCP server (e.g. `buzz-dev-mcp`) |
| `BUZZ_ACP_SYSTEM_PROMPT[_FILE]` | Persona / system prompt |
| `BUZZ_ACP_BASE_PROMPT_FILE` / `BUZZ_ACP_NO_BASE_PROMPT` | Override/disable the base prompt |
| `BUZZ_ACP_SUBSCRIBE` | `mentions` \| `all` \| `config` |
| `BUZZ_ACP_RESPOND_TO` | `owner-only` \| `allowlist` \| `anyone` \| `nobody` |
| `BUZZ_ACP_PERMISSION_MODE` | `default` \| `acceptEdits` \| `bypassPermissions` \| `plan` |
| `BUZZ_ACP_MULTIPLE_EVENT_HANDLING` | `steer` \| `interrupt` \| `owner-interrupt` \| `queue` |
| `BUZZ_ACP_CONTEXT_MESSAGE_LIMIT` | Thread context messages per turn (default 12) |
| `BUZZ_ACP_IDLE_TIMEOUT` / `BUZZ_ACP_MAX_TURN_DURATION` | Turn timeouts |
| `NOSTR_PRIVATE_KEY` | Key for `git-credential-nostr` / `git-sign-nostr` (git auth/signing) |

Run `buzz-acp --help` for the complete flag list.

---

## See also

- [`examples/agents/`](examples/agents/) — a runnable self-hosted agent:
  `docker-compose.yml`, `Dockerfile`, a starter `buzz-acp.toml`, and a persona
- [`examples/agents/sandbox/`](examples/agents/sandbox/) — a **sandboxed** Claude
  agent: Claude runs in a locked-down container (read-only rootfs, dropped caps,
  caller UID, only a repos dir mounted, secrets allowlisted) via a `docker run`
  wrapper as `BUZZ_ACP_AGENT_COMMAND`
- [AGENTS.md](AGENTS.md) — repo conventions and the agent contributor guide
- `crates/buzz-acp/src/base_prompt.md` — the compiled-in base prompt
- `crates/buzz-persona/PERSONA_PACK_SPEC.md` — full persona-pack format
- `crates/buzz-dev-mcp/src/lib.rs` — the shell/file tool server
- `crates/git-credential-nostr/README.md`, `crates/git-sign-nostr/README.md`
