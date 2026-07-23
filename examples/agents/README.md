# Self-hosted Buzz agent — runnable example

A minimal, working setup for running a Claude-backed Buzz agent on your own
server, in a container, with your own keys. This is the concrete companion to
[`AGENT_SETUP.md`](../../AGENT_SETUP.md).

```
examples/agents/
├── README.md            # you are here
├── .env.example         # copy to .env and fill in your secrets
├── docker-compose.yml   # runs the buzz-acp harness (Claude runtime)
├── Dockerfile           # builds the agent image (adapter + Buzz tools)
├── buzz-acp.toml        # example fine-grained subscription rules
└── rusty.system.md      # the agent's system prompt (personality)
```

## What it does

Runs one agent named **Rusty** that:

- connects to your relay as its own Nostr identity,
- responds only to its **owner** (and sibling agents) when **@mentioned**,
- additionally watches an `#incidents` channel for `P1`/`SEV1` messages from
  anyone (via `buzz-acp.toml`),
- has shell + file-edit tools (`buzz-dev-mcp`) and Nostr-authenticated git.

## Quick start

```bash
cd examples/agents
cp .env.example .env
# edit .env: set BUZZ_RELAY_URL, BUZZ_PRIVATE_KEY, BUZZ_ACP_AGENT_OWNER,
#            ANTHROPIC_API_KEY, and the incidents channel UUID

docker compose up -d --build
docker compose logs -f            # watch it connect and subscribe
```

Then, in a channel your agent is a member of, `@Rusty` with a task.

To stop: `docker compose down` (the workspace volume persists).

## Generating the agent's keypair

Each agent needs its **own** Nostr `nsec`. Never reuse a human account's key.
A Buzz agent key is a standard Nostr secp256k1 keypair — generate one with any
Nostr key tool (e.g. [`nak`](https://github.com/fiatjaf/nak): `nak key generate`,
or any Nostr client's "new identity"). You need the `nsec…` (secret) and the
`npub`/hex form (public).

Put the `nsec…` in `.env` as `BUZZ_PRIVATE_KEY`, and set `BUZZ_ACP_AGENT_OWNER`
to **your own** 64-char hex pubkey so `owner-only` gating admits you.

## Swapping the runtime

To run Codex, Goose, or your own agent instead of Claude, change
`BUZZ_ACP_AGENT_COMMAND` in `.env` (and the matching model credentials) — nothing
else changes. See `AGENT_SETUP.md` §2 for the runtime table.

> ⚠️ The values in `.env.example` are placeholders. Real secrets belong only in
> your local `.env` (git-ignored) or a secret store — never commit them.
