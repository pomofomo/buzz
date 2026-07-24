# CUTOVER.md — Flipping a Buzz relay from Nostr auth to API-key auth

This is the operator runbook for cutting a live community over from
`BUZZ_AUTH_MODE=nostr` (NIP-42/NIP-98 signatures) to `BUZZ_AUTH_MODE=apikey`
(bearer tokens + server-authored records). It implements the sequence in
[REFACTOR.md](REFACTOR.md) §7 ("Data migration & cutover") using the operational
tooling in `buzz-admin`.

Read [REFACTOR.md](REFACTOR.md) §1–§4 first for the trust model. The short
version: after cutover the **server authenticates and authors every row**, so
attribution is only as strong as the server/admin. Historical Nostr-signed
events stay as immutable legacy (`events.sig` is now nullable); nothing is
re-projected.

The migration is **flag-gated and additive** — every step below is safe to run
while the relay is still serving `nostr` mode, up to the final flag flip. You can
stop after any step and the relay keeps working in its current mode.

---

## 0. Prerequisites & conventions

- `buzz-admin` runs **inside the relay container**, sharing the relay's env:
  ```bash
  docker compose exec relay buzz-admin <subcommand>
  # or, from a checkout with the env exported:
  cargo run -p buzz-admin -- <subcommand>
  ```
- `buzz-admin` is **single-community per invocation**: it resolves its target
  community from `RELAY_URL`'s host (the same host map the relay seeds on
  startup). `cutover-genesis --all` is the one exception that sweeps every
  community.
- Required env for every step: `DATABASE_URL`, `RELAY_URL`. Member-roster steps
  also need `REDIS_URL` and `BUZZ_RELAY_PRIVATE_KEY`.
- **Take a database backup before step 1.** Every step below is additive and
  reversible except the operational flag flip, but a backup is cheap insurance.

---

## 1. Backfill one API key per existing active actor

Mint a key for every current relay member so their existing `pubkey` keeps
working as an opaque actor id and all history stays attributed. "Active actor" =
the **relay membership roster** (`relay_members`) — the identities admitted to
this relay, the same set the invite-claim path self-mints a member key for.

**Dry run first** — resolves the plan without writing anything:

```bash
docker compose exec relay buzz-admin backfill-keys --dry-run
```

Then the real run. It prints a **TSV of `actor → plaintext key` to stdout, once**
(`actor  role  outcome  token_id  api_key`). Capture it to a secure location:

```bash
docker compose exec relay buzz-admin backfill-keys > backfill-keys-$(date +%Y%m%dT%H%M%SZ).tsv
```

- Scopes are role-derived: **members** get the invite self-mint set
  (`messages:{read,write}`, `channels:read`, `users:{read,write}`,
  `files:{read,write}`, `subscriptions:read`); **admins/owners** additionally get
  `channels:write`, `admin:channels`, `admin:users` so they keep their
  administrative powers post-cutover.
- **Idempotent:** actors that already hold an active (non-revoked, non-expired)
  key are skipped (`outcome = skipped-active`). Re-running is safe and mints
  nothing new.
- **Open relays** (membership not enforced) may not list every event author in
  `relay_members`; on those, issue keys individually instead:
  ```bash
  buzz-admin issue-key --actor <hex> --scope messages:read,messages:write,channels:read,users:read,users:write,files:read,files:write,subscriptions:read
  ```

The plaintext keys are shown **only here** — the store keeps only their SHA-256
hash. A lost key is re-issued with `rotate-key` / `issue-key`, not recovered.

## 2. Distribute keys to actors

Deliver each actor's plaintext key over a secure channel. Clients store it where
the nsec used to live:

- **Desktop:** Settings → paste the API key (stored in the OS keyring).
- **Mobile:** the API-key sign-in page (`SignInPage`).
- **CLI / agents:** `export BUZZ_API_KEY=<token>` (replaces `BUZZ_PRIVATE_KEY`);
  the ACP harness injects `BUZZ_API_KEY` into managed agent subprocesses.

New joiners after cutover don't need this step — claiming an invite self-mints a
member key (`crates/buzz-relay/src/api/invites.rs`).

## 3. Append the audit cutover genesis

Pin the last Nostr-era audit head into a `cutover_genesis` entry, so pre-cutover
history stays anchored to the chain that continues under API-key auth. Per
community:

```bash
docker compose exec relay buzz-admin cutover-genesis
```

Or sweep every community in the deployment:

```bash
docker compose exec relay buzz-admin cutover-genesis --all
```

- The entry chains from the current head and records the prior head hash +
  timestamp in `detail.last_nostr_head_hash`. A community with no audit history
  yet gets the genesis as its first entry.
- **Idempotent:** a community that already carries a `cutover_genesis` entry is
  refused (single-community: exit code 4; `--all`: reported and skipped). Run it
  **once** per community, at the cutover boundary.

## 4. (Recommended) Provision the WORM anchor bucket

External Object-Lock anchoring gives you tamper-evidence: once a head-hash object
is written to a WORM bucket it can't be altered for the retention window, so a
later rewrite of the Postgres chain is detectable.

Provision a bucket with Object Lock + a default retention rule (Terraform in
production). For a local/MinIO smoke:

```bash
mc alias set s3 https://s3.example.com "$ACCESS_KEY" "$SECRET_KEY"
mc mb --with-lock s3/buzz-audit-worm
mc retention set --default GOVERNANCE 30d s3/buzz-audit-worm   # or COMPLIANCE
```

## 5. Enable anchoring

Set the anchor env (see [.env.example](.env.example)) and restart the relay:

```bash
BUZZ_AUDIT_ANCHOR_ENABLED=true
BUZZ_AUDIT_ANCHOR_BUCKET=buzz-audit-worm
BUZZ_AUDIT_ANCHOR_INTERVAL_SECS=3600
# endpoint/region/creds fall back to the shared BUZZ_S3_* / AWS_REGION if unset:
BUZZ_AUDIT_ANCHOR_S3_ENDPOINT=https://s3.example.com
BUZZ_AUDIT_ANCHOR_S3_REGION=us-east-1
BUZZ_AUDIT_ANCHOR_S3_ACCESS_KEY=...     # empty ⇒ AWS default credential chain
BUZZ_AUDIT_ANCHOR_S3_SECRET_KEY=...
```

> The relay spawns the anchor worker at startup when
> `BUZZ_AUDIT_ANCHOR_ENABLED=true` (see "Lane F — audit WORM anchoring" in
> `crates/buzz-relay/src/main.rs`); it sweeps every
> `BUZZ_AUDIT_ANCHOR_INTERVAL_SECS` and performs a final sweep on
> SIGTERM/ctrl-c before the drain window closes. After enabling, verify a
> head-hash object lands:
> ```bash
> mc ls --recursive s3/buzz-audit-worm/audit-anchor/
> mc cat s3/buzz-audit-worm/audit-anchor/<community>/<seq>.json
> # → {"community_id":...,"seq":N,"hash":"<hex>","timestamp":...}
> ```

## 6. Flip the auth mode

Once keys are distributed and clients updated, disable the Nostr doorways:

```bash
BUZZ_AUTH_MODE=apikey
```

Restart the relay. From here the WS **upgrade** request and every HTTP bridge
call must present `Authorization: Bearer <token>`; the server authors each row
after the auth gate.

Smoke-test with a backfilled key before announcing:

```bash
BUZZ_API_KEY=<a-backfilled-token> BUZZ_RELAY_URL=ws://<host> \
  buzz channels list
```

## 7. Dual-run window

If clients roll out asynchronously, keep a **short dual-run window**: because the
model is flag-gated, you can flip individual relay instances (or keep one on
`nostr` behind the same DB) while clients update, then converge all instances on
`apikey`. During the window:

- Both doorways share one Postgres schema; no data diverges.
- New writes in `apikey` mode are server-authored with empty `sig`; reads
  tolerate empty `sig` on both clients.
- Keep an eye on auth-failure logs for clients still presenting NIP-42/98.

---

## Rollback

Cutover is reversible up to the point clients discard their private keys:

1. **Before the flag flip (steps 1–5):** everything is additive. To undo, revoke
   the backfilled keys and stop; the relay is still in `nostr` mode.
   ```bash
   buzz-admin revoke-all-keys --actor <hex>     # per actor, or
   buzz-admin revoke-key --actor <hex> --id <token-uuid>
   ```
   The `cutover_genesis` audit entry is append-only and harmless to leave in
   place (it does not change auth behavior); it simply records the boundary.

2. **After the flag flip (step 6):** set `BUZZ_AUTH_MODE=nostr` and restart. The
   relay re-enables the NIP-42/98 doorways. This works **only while clients still
   hold their Nostr private keys** — once a client has migrated to key-only
   storage and dropped its nsec, it can no longer sign, so plan the flip when you
   are confident forward. The backfilled API keys remain valid if you flip
   forward again.

3. **Anchoring** can be turned off at any time (`BUZZ_AUDIT_ANCHOR_ENABLED=false`
   + restart) with no data impact — already-written WORM objects stay immutable
   for their retention window regardless.

---

## Command reference

| Step | Command |
|------|---------|
| 1. Backfill (preview) | `buzz-admin backfill-keys --dry-run` |
| 1. Backfill (real) | `buzz-admin backfill-keys > keys.tsv` |
| 1. Single key (open relay) | `buzz-admin issue-key --actor <hex> --scope <csv>` |
| 3. Cutover genesis (one) | `buzz-admin cutover-genesis` |
| 3. Cutover genesis (all) | `buzz-admin cutover-genesis --all` |
| 6. Smoke test | `BUZZ_API_KEY=<t> buzz channels list` |
| Rollback | `buzz-admin revoke-key --actor <hex> --id <uuid>` |

Exit codes (`buzz-admin`): `0` ok · `1` input error · `2` not-found · `3` auth ·
`4` idempotency refusal (cutover genesis already exists) · `5` write/DB error.
