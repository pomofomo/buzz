# Sandboxed Claude agent — a hardened agent profile

Runs a Claude-backed Buzz agent where **Claude's tool execution is boxed in a
container** instead of running as your full user (which is what Desktop "easy
mode" does — see [`AGENT_SETUP.md`](../../../AGENT_SETUP.md) §"easy mode"). The
agent can touch **only a repos directory you choose** and **only its own
secrets** — your keyring, SSH keys, other API keys, browser data, and the rest
of `$HOME` stay invisible.

## How it works

ACP is JSON-RPC over stdio, so the "agent binary" can be `docker run -i`. The
`buzz-acp` harness runs on the host (trusted — it holds the agent's key and
talks to the relay), but it launches the agent runtime through a wrapper that
starts a container:

```
buzz-acp (host)  ──ACP stdio──▶  claude-sandbox.sh  ──▶  docker run -i  ──▶  Claude
   holds nsec,                    (the choke point)        locked-down box     + tools,
   talks to relay                 forwards ONLY the                             sees only
                                  allowlisted -e vars                           /work/REPOS
```

The wrapper is the **security choke point**: the harness passes it the whole
environment (every secret), but only the vars named in the `docker run -e` flags
cross into the container.

```
sandbox/
├── Dockerfile          # rust base + node + Claude + adapter + buzz tools
├── entrypoint.sh       # seeds writable Claude config from the RO key mount, execs adapter
├── claude-sandbox.sh   # the hardened `docker run` — this is BUZZ_ACP_AGENT_COMMAND
├── run-agent.sh        # launches buzz-acp on the host, pointed at the wrapper
├── agent.system.md     # the agent's system prompt (personality + boundaries)
└── README.md           # you are here
```

## The hardened `docker run` (what each flag buys you)

| Flag | Why |
|------|-----|
| `--user "$(id -u):$(id -g)"` | Runs as the **calling user**, not root — files written to `/work/REPOS` keep your ownership, no root-owned droppings. |
| `--read-only` | Root filesystem is immutable. |
| `--tmpfs /state …,mode=1777` | The only writable space: ephemeral `$HOME` (Claude config, cargo/npm caches). Discarded on exit. |
| `--cap-drop ALL` | No Linux capabilities. |
| `--security-opt no-new-privileges` | No setuid privilege escalation. |
| `--pids-limit / --memory / --cpus` | Caps a runaway build/tool loop. |
| `-v "$REPOS_DIR":/work/REPOS` | **The only host path mounted** (read-write, for edits/commits). |
| `-v "$CLAUDE_DIR":/host-claude:ro` | API key source, **read-only** — never written back. |
| `-e BUZZ_PRIVATE_KEY …` (allowlist) | Only these vars cross the boundary. Everything else in the harness env stays out. |
| `--init` + `--rm` | Proper signal/zombie handling; container removed on exit. |

## Secrets: what the box can and can't see

The container gets **only**:

- `BUZZ_PRIVATE_KEY` / `NOSTR_PRIVATE_KEY` — this agent's **own** nsec. Unavoidable:
  it's the identity the agent posts and pushes git as. This is the one secret the
  sandbox legitimately needs.
- `BUZZ_RELAY_URL`, `BUZZ_AUTH_TAG` — relay + owner attestation.
- The **Anthropic API key** via the read-only `~/.claude` mount — so the key
  never appears in the `docker run` argv, `docker inspect`, or `ps`.

It does **not** see: your login keyring, `~/.ssh`, other repos, `AWS_*` / other
`*_API_KEY` env, `~/.aws`, browser profiles, or anything else in `$HOME`. Because
the wrapper only forwards the allowlisted `-e` vars, no host env leaks in.

## Setup

### 1. Build the image (from the repo root — it compiles the buzz binaries)

```bash
# Verify the Claude package names in the Dockerfile against npm first.
docker build -f examples/agents/sandbox/Dockerfile -t buzz-claude-sandbox:latest .
```

### 2. Make the scripts executable

```bash
chmod +x examples/agents/sandbox/claude-sandbox.sh \
         examples/agents/sandbox/entrypoint.sh \
         examples/agents/sandbox/run-agent.sh
```

### 3. Point it at your repos + keys and launch

```bash
export BUZZ_RELAY_URL="wss://relay.example.com"
export BUZZ_PRIVATE_KEY="nsec1this_agents_own_key"      # generate one per agent
export BUZZ_ACP_AGENT_OWNER="<your-64-char-hex-pubkey>"
export BUZZ_SANDBOX_REPOS="$HOME/agent-repos"           # the ONLY dir the agent can touch
# Optional: point at a non-default ~/.claude
# export BUZZ_SANDBOX_CLAUDE_DIR="$HOME/.claude"

./examples/agents/sandbox/run-agent.sh
```

`@mention` the agent in a channel it's a member of and watch it work — boxed.

## Wiring this as a Desktop "agent profile"

Desktop's `BUZZ_ACP_AGENT_COMMAND` is normally a runtime name (`claude-agent-acp`).
To sandbox a Desktop-managed agent, set that agent's **command to the absolute
path of `claude-sandbox.sh`** (via the agent's env/config), and set the wrapper's
required vars (`BUZZ_SANDBOX_REPOS`, `CLAUDE_SANDBOX_IMAGE`) in the agent's env.
Desktop still injects identity + relay; the wrapper forwards only those onward.
The persona body goes in `agent.system.md` here (or the persona pack's markdown
body — see `crates/buzz-persona/PERSONA_PACK_SPEC.md`).

> Note: Desktop sets the cwd to the shared `~/.buzz` nest and mounts nothing —
> containerizing via the wrapper is what actually restricts filesystem reach.

## Anything else worth doing

- **Egress allowlist (the one thing Docker alone doesn't fully give you).** Drop
  caps and read-only rootfs don't stop the agent from reaching arbitrary hosts.
  Run a filtering proxy (squid/tinyproxy) that allows only your relay host,
  `api.anthropic.com`, and your git host, then export `HTTPS_PROXY`/`HTTP_PROXY`
  (the wrapper already forwards them). For a hard guarantee, put the container on
  a dedicated docker network with an nftables egress allowlist, or run the proxy
  as a sidecar on an `--internal` network.
- **Stronger kernel isolation:** run under **gVisor** (`--runtime=runsc`) or a
  microVM (Firecracker/Kata) if the workload is untrusted.
- **user-namespace remapping** (`dockerd --userns-remap`) so container UID 0 ≠
  host root, defense-in-depth on top of `--user`.
- **Pin image digests** and scan the image; rebuild on a schedule for CVEs.
- **Never** mount the docker socket, use `--privileged`, or `--network host`
  (except a localhost relay — prefer `host.docker.internal`, already wired).
- **One ephemeral container per agent** (`--rm`, already set); rotate the agent's
  nsec periodically and scope its relay/git permissions to what it needs.
- **Verify nothing leaks:** `docker run … env | sort` should show only your
  allowlisted vars.

## Caveats

- The Claude npm package names in the `Dockerfile` are `ARG`s — **verify them
  against the registry** (the Zed adapter is `@zed-industries/claude-code-acp`,
  command `claude-code-acp`; set `ADAPTER_CMD` to match).
- If your relay is remote, drop `--add-host host.docker.internal:host-gateway`.
- A hard `SIGKILL` of the wrapper can orphan the container; the `--rm` + name +
  cleanup trap handle graceful stops and idle-timeout kills.
