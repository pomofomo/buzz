# DEV.md — Where this branch stands and what to do next

_Last updated 2026-07-26, branch `claude/nostr-api-keys-refactor-06pyto`, head `3154c996`._

## TL;DR

The Nostr → API-key migration is **code-complete, tested, and pushed**, including
the operational cutover tooling. Everything in [FULL_TESTS.md](FULL_TESTS.md) §5
was executed and is green (see the status banner at the top of that file). The
only remaining work is **running the cutover on a real deployment** — a deploy
decision, not a code task — plus normal PR/review flow.

## What to do when you come back

### 1. Sanity-check the tree is still green (~15 min)

```bash
. ./bin/activate-hermit          # Rust/Node/Dart toolchains (flutter/dart live here)
docker compose up -d minio       # MinIO only; Postgres/Redis are NATIVE on this box (see below)
just ci                          # full gate: workspace + desktop + web + mobile
```

### 2. Open the PR (if not already open)

```bash
gh pr create --repo pomofomo/buzz \
  --title "Nostr substrate → API-key auth migration" \
  --base main
```

Suggested review guide for the PR body: REFACTOR.md (design + locked
decisions), NO_NOSTR.md (rationale), FULL_TESTS.md banner (verification
evidence), CUTOVER.md (operator runbook). The branch is flag-gated:
`BUZZ_AUTH_MODE=nostr` (default) is behavior-preserving; `apikey` is the new
path — reviewers should focus on the read-gate invariants
(REFACTOR.md §4) and the ingest/auth doorways.

### 3. When ready to flip a deployment: follow CUTOVER.md

The short version (each step has exact commands in [CUTOVER.md](CUTOVER.md)):

1. `buzz-admin backfill-keys --dry-run` → review → run for real → distribute keys.
2. `buzz-admin cutover-genesis --all` (pins the last Nostr-era audit head).
3. Provision an Object-Lock (WORM) bucket; set `BUZZ_AUDIT_ANCHOR_*` env.
   The relay spawns the anchor worker automatically when
   `BUZZ_AUDIT_ANCHOR_ENABLED=true`.
4. Set `BUZZ_AUTH_MODE=apikey` and restart. Keep a dual-run window if clients
   roll out asynchronously; rollback = unset the flag (nostr path is intact).

### 4. Possible follow-ups (none blocking)

- **v2 E2E encryption** (REFACTOR.md §9) if confidentiality-from-server ever
  becomes a requirement — all client crypto was deliberately removed in v1.
- Remove the `nostr` crate dependency entirely once the nostr auth doorway is
  retired post-cutover (it is the last planned step in REFACTOR.md §6 Phase 8).
- `SignInPage` on mobile takes actor-id manually; a relay "whoami" endpoint
  would remove that step (FULL_TESTS §5.C item 3, deferred by choice).

## This machine's quirks (will bite you if forgotten)

- **Postgres/Redis are NATIVE, not Docker.** A host PostgreSQL 16.14 owns
  `localhost:5432` (user/db `buzz`/`buzz_dev`) and a native Redis owns
  `localhost:6379`. The compose `postgres`/`redis` services can't bind those
  ports — don't be fooled by their containers sitting in `Created`. The
  documented URLs work as-is:
  `DATABASE_URL=postgres://buzz:buzz_dev@localhost:5432/buzz`,
  `REDIS_URL=redis://localhost:6379`. (CI uses PG17; local is PG16 — no
  version-specific failures observed.) MinIO/Keycloak/etc. run via compose
  normally. Some suites also read `BUZZ_TEST_DATABASE_URL` — set it to the same
  value.
- **Desktop Playwright needs `pnpm run build:e2e`** — a plain `pnpm run build`
  strips the mock Tauri bridge and every test fails with
  `Cannot read properties of undefined (reading 'invoke')`. Kill any stale
  server on port 4173 after rebuilding.
- **Relay-backed Playwright specs** (`stream`, `integration`,
  `dm-double-notification`, `parity-ancestor-island`) need a relay started with
  CI's env (rate limits at 100000, `BUZZ_RECONCILE_CHANNELS=true` — copy the
  block from `.github/workflows/ci.yml`, "Start relay"), then
  `bash scripts/setup-desktop-test-data.sh`. They also assume a **clean**
  fixture community (`localhost:3000`): leftover events from prior runs cause
  feed-assertion failures. Re-runs against a dirty DB are not a code signal.
- **`cargo test --workspace -- --include-ignored` is unsafe as written**: the
  `buzz-db` migration tests `DROP SCHEMA public CASCADE` and are not
  mutex-serialized. Run `cargo test -p buzz-db -- --include-ignored
  --test-threads=1` separately and re-run `cargo run -p buzz-admin -- migrate`
  afterwards.
- Parallel Postgres-gated tests can starve each other's connections when other
  suites hammer the same DB — a `.expect("requires reachable Postgres")` panic
  in an otherwise-green module usually means contention, not breakage; retry
  single-threaded before diagnosing.
- WORM smoke bucket `buzz-audit-worm` exists on local MinIO
  (creds `buzz_dev`/`buzz_dev_secret`, GOVERNANCE 30d) with test anchors in it.

## Known benign warts

- Two desktop Playwright tests were historically flaky and are now fixed via
  app-code changes (`ProfileSettingsCard` inert race, programmatic-scroll
  context-menu dismissal). If either regresses, start from commit `3154c996`.
- One mobile test is intentionally skipped (`compose_bar_test.dart`
  video-upload — relies on native transcode bridging).
- The local dev DB contains accumulated test communities/data. Harmless, but
  see the relay-backed-specs note above before trusting E2E failures.
