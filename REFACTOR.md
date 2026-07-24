# REFACTOR: Remove Nostr substrate → API-key auth + server-authored records

**Status:** Scoping complete. Ready for multi-agent execution.
**Branch:** `claude/nostr-api-keys-refactor-06pyto`
**Goal:** Remove Nostr as the identity, transport, and record model. Preserve the
Postgres materialized-view schema and the query/read API as literally as possible
so the clients need only **auth changes + write-path changes**, not a read-layer
rewrite. Keep the one property worth keeping — a tamper-evident audit trail.

---

## 0. Read this first — the shape of the job

This is **not** a green-field rebuild. Reconnaissance found that most of the
target architecture is already scaffolded in the codebase and merely bypassed.
The refactor largely **activates existing machinery** and **swaps the auth
doorway**, rather than inventing new subsystems.

**Already present (do not rebuild — wire in):**

- **Hashed API-key store**: `api_tokens` table (`migrations/0001_initial_schema.sql:472-491`)
  with `token_hash` (SHA-256), `owner_pubkey`, `name`, `scopes JSONB`,
  `channel_ids JSONB`, `expires_at`, `revoked_at`, a 10-key-per-owner limit, and
  full CRUD in `crates/buzz-db/src/api_token.rs` (create / create-if-under-limit /
  lookup-by-hash / list-by-owner / revoke / revoke-all).
- **Scope model**: `Scope` enum + `parse_scopes` + `require_scope` +
  `check_read_access` / `check_write_access` + `ChannelAccessChecker` trait
  (`crates/buzz-auth/src/scope.rs`, `access.rs`). Vocabulary already includes
  `messages:{read,write}`, `channels:{read,write}`, `files:{read,write}`,
  `repos:{read,write}`, `jobs:{read,write}`, `users:{read,write}`,
  `subscriptions:{read,write}`, `admin:{channels,users}`. **Currently bypassed:**
  `AuthService::verify_auth_event` grants `Scope::all_known()` to every
  authenticated connection ("pure Nostr mode", `crates/buzz-auth/src/lib.rs:134-142`).
- **Transport-neutral auth seam**: `IngestAuth` enum
  (`crates/buzz-relay/src/handlers/ingest.rs:62-131`) already abstracts the write
  path over `.pubkey()` / `.scopes()`. Add an API-key-resolved variant here.
- **Audit hash-chain**: per-community `prev_hash → hash` chain with genesis
  sentinel, canonical-JSON hashing, and single-writer serialization via a
  per-community Postgres advisory lock (`crates/buzz-audit/`, `audit_log` table
  `migrations/0001_initial_schema.sql:606-619`). Audit already records the
  **authenticated principal**, not the event signer.
- **Redis pub/sub fanout**: server-authored fanout with a send-time access
  re-check already exists (`crates/buzz-relay/src/handlers/event.rs:115-199`,
  `258-315`, `373-538`; `crates/buzz-pubsub/`).
- **Bearer patterns in embryo**: `BUZZ_API_TOKEN` is already used for outbound
  workflow HTTP (`crates/buzz-workflow/src/executor.rs:898-901`);
  `BUZZ_ACP_API_TOKEN → BUZZ_API_TOKEN` mapping exists
  (`crates/buzz-acp/src/config.rs:715-726`); `StaticTokenSource` bearer pattern in
  `crates/buzz-agent/src/auth.rs:76-89`.

**The read/query path is already signature-independent.** `sig` is stored only to
rebuild the `nostr::Event` JSON on read (`crates/buzz-db/src/event.rs:527-535`);
no DB read verifies a signature. `pubkey` is used only as an author-filter and a
key column. SDK builders are already sign-free (build/sign are separate crates).

---

## 1. Locked decisions (from scoping interview)

| # | Decision | Choice |
|---|----------|--------|
| 1 | **Tenancy** | **Keep multi-tenant.** `community_id` stays on every table/PK/index; the conformance harness stays. The API-key trust model applies *within* each community. No tenancy-driven schema churn. |
| 2 | **Wire & identity shape** | **Keep the event envelope; repurpose `pubkey` as an opaque 32-byte actor id.** Server authors rows keeping `{id, kind, tags, content, created_at}`. `sig` becomes vestigial (dropped from verification; emitted empty where the wire type still requires it). Read consumers, kind mirrors, and CLI builders survive. |
| 3 | **Git / NIP-34** | **Keep NIP-34 kinds as server-authored records; swap only git transport auth + commit signing to bearer.** Desktop `projects` / web `repos` keep working with minimal change. |
| 4 | **Audit** | **Keep the Postgres hash-chain; add periodic head-hash export to S3 Object Lock / WORM** and a fresh genesis entry at cutover referencing the last Nostr-era state. |
| 5 | **Client-side encryption** | **Drop E2E (NIP-44/NIP-17) for v1.** Server holds plaintext, gated by scope + membership. Removing per-actor private-key management was an explicit goal. **v2 nice-to-have**, out of scope here. |
| 6 | **Authz granularity** | **`channel_members` stays the write-membership filter** (preserves the DB/query model). API-key **scopes** cover action-level (read/write/admin/repos); per-key `channel_ids` is optional narrowing. |
| 7 | **Key provisioning** | **Admin/owner issues human keys via `buzz-admin` (+ admin API); desktop stores in OS keyring as today. Agents get `BUZZ_API_KEY` injected by the ACP harness** (replacing `BUZZ_PRIVATE_KEY`). Rotation = revoke + reissue. |
| — | **Read transport** | **REQ-shaped filtered reads retained.** Endpoints/auth change; response data types stay near-identical (filter → row array + a channel-topic live subscription returning the same row shape). No read-consumer rewrite. |

---

## 2. Trust boundary — flag to stakeholders before starting

Single-org-per-community, **trust-the-admin, trust-the-server**. Attribution is
only as good as the server, because the server both authenticates and authors the
row. Consequences to state explicitly:

- **No per-message non-repudiation.** No client signature means a compromised app
  layer or admin can author rows as any actor.
- **No client-side confidentiality from the server** in v1 (E2E dropped). DMs and
  formerly owner-encrypted kinds (agent engram memory, agent turn metrics, DM
  visibility, reminders, push leases) become server-side-private rows gated by
  scope + membership — the server/admin can read them.
- This is an intentional trade for operational simplicity (no per-actor private
  key management, no key-transfer/pairing), **not** a regression to fix later
  except where noted as v2.

---

## 3. Target architecture

- **Identity:** one API key per actor (human or agent). Stored **hashed**
  (`api_tokens.token_hash`, SHA-256). Each key carries an authorization scope set
  and optional channel restriction. The resolved principal is an **opaque 32-byte
  `actor` id** occupying the same column shape the `pubkey` column has today.
- **Auth:** bearer token at the gate for **authentication**; scope + membership
  check for **authorization** (this is the write-membership filter Nostr lacked
  natively — now real, no longer `all_known()`).
- **Records:** server-authored rows in Postgres. "Everything is an event" schema
  stays. Events are no longer per-message signed; the server writes them **after**
  the auth gate.
- **Audit:** existing Postgres `prev_hash → hash` chain, single-writer, plus a new
  periodic **head-hash anchor** exported to S3 Object Lock/WORM (out-of-band).
- **Live updates:** WebSocket stays for streaming. NIP-42 challenge/response is
  replaced by bearer validation at connect. REQ semantics are retained in a
  reduced form (filter → historical rows + channel-topic live stream); Redis
  pub/sub fanout is unchanged, and the **send-time access re-check is preserved**.

---

## 4. Invariants that MUST NOT break

These are the load-bearing safety properties. Any lane touching reads/fanout must
preserve them for the **bearer principal**, or the log opens.

1. **Reads are gated at the handler level, not by authentication.** Authentication
   is a prerequisite; the actual read gates live in
   `crates/buzz-relay/src/handlers/req.rs` (WS) and mirrored in
   `crates/buzz-relay/src/api/bridge.rs` (HTTP `/query`, `/count`):
   - **Channel-membership scoping** via `accessible_channels`
     (`req.rs:94-105`, drop at `req.rs:372-376`).
   - **p-gate** (`P_GATED_KINDS`, `crates/buzz-core/src/kind.rs:146-156`;
     enforcement `req.rs:1042-1076`) — kindless filters must carry `#p == self`.
   - **author-only** (`AUTHOR_ONLY_KINDS`, `kind.rs:120`; `req.rs:198-204`,
     `387-389`).
   - **result-gated** (`RESULT_GATED_KINDS`, `kind.rs:129`;
     `reader_authorized_for_event`, `req.rs:381-384`).
   - **engram gate** (`req.rs:191-197`).
   Reproduce every one of these for the API-key principal.
2. **Send-time fanout re-check** (`filter_fanout_by_access`,
   `handlers/event.rs:115-199`) must remain, so a stale subscription cannot leak
   after an open→private flip.
3. **Multi-tenant isolation.** Every query stays scoped by `community_id`; the
   conformance harness (`crates/buzz-test-client/tests/conformance_multitenant.rs`)
   must stay green. API-key lookup stays keyed on `(community_id, token_hash)`
   (`api_token.rs` — row-44 conformance).
4. **Relay-only kinds** (`is_relay_only_kind`, `kind.rs:682-692`) remain
   server-authored and client-unsubmittable.
5. **Thread counters** (`reply_count`, `descendant_count` on `thread_metadata`)
   stay materialized transactionally on insert/delete
   (`crates/buzz-db/src/event.rs:1137-1161`, `777-797`).

---

## 5. Workstreams (lanes)

Lanes are sized for parallel agents. Dependencies are called out; the **core
identity/auth/ingest lanes (A–D) are tightly coupled** and should land as a
coordinated group behind a feature flag before the peripheral lanes flip over.

Suggested global flag: `BUZZ_AUTH_MODE = nostr | apikey` (default `nostr` until
cutover), letting the relay run both auth doorways during migration.

### Lane A — Identity & schema (foundation)
**Depends on:** none. **Blocks:** B, C, D, E, F, G.

- New migration: add `actor` semantics to the identity column. **Preferred:**
  keep the physical `pubkey BYTEA` columns and treat their contents as an opaque
  32-byte actor id (derived from the key, e.g. `SHA-256(key_id)` truncated, or the
  key's UUID zero-padded) to avoid renaming ~15 tables and their indexes. Document
  the reinterpretation; do **not** rename columns in v1 unless a lane proves it
  cheap. Revisit `CHECK (LENGTH(pubkey)=32)` constraints (`users`, moderation,
  push) — keep 32 bytes so the actor id fits without constraint churn.
- `events` table: keep `id`, `pubkey` (now actor), `kind`, `tags`, `content`,
  `created_at`, `d_tag`. **Drop the `sig NOT NULL` requirement** — make it
  nullable or default empty; stop selecting it for verification. Rehydration
  (`row_to_stored_event`, `event.rs:511-552`) stops requiring a `sig` field (emit
  `""` if the wire type still needs it).
- Rebuild the three pubkey-bearing event indexes on the reinterpreted column only
  if a rename happens (otherwise no-op): `idx_events_community_pubkey_kind_created`,
  `idx_events_addressable`, `idx_events_parameterized`.
- NIP-33/NIP-RS replacement keys (`parameterized_event_watermarks`, guard triggers
  in migrations `0007/0009/0010/0011`) continue to key on the actor column —
  semantics unchanged.
- Add an `api_keys` view/columns if needed (the `api_tokens` table already
  suffices; `owner_pubkey` becomes the actor id).

**Acceptance:** schema migrates forward on a fresh DB and on a backfilled
single-community DB; `just test` DB-dependent suites pass; no read query selects
`sig` for verification.

### Lane B — Auth gate (bearer + scope activation)
**Depends on:** A. **Blocks:** C, D, E, G.

- Replace `AuthMethod::{Nip42,Nip98}` with `AuthMethod::ApiKey` (keep the enum;
  add the variant, retire the others behind the flag).
- `AuthService`: add `verify_api_key(token) -> AuthContext` — hash the presented
  token (SHA-256), look up via `Db::get_api_token_by_hash` (community-scoped),
  reject revoked/expired, populate `AuthContext { actor, scopes, channel_ids }`
  from the stored record. **Stop granting `Scope::all_known()`** — grant the key's
  actual scopes (`crates/buzz-auth/src/lib.rs:134-142`).
- Wire `require_scope` / `check_read_access` / `check_write_access` /
  `ChannelAccessChecker` (`access.rs`) into the hot paths that currently inline
  membership checks (they already exist but are not on the live path).
- WS: replace the NIP-42 challenge/timeout machinery
  (`crates/buzz-relay/src/connection.rs:36-47`, `230-249`;
  `handlers/auth.rs`) with a bearer check at connect (header or first frame).
- HTTP: the dev `X-Pubkey` fallback (`api/bridge.rs:62-128`) is the natural seam —
  replace `verify_bridge_auth` with `Authorization: Bearer` validation producing
  `(actor, scopes)`.
- Rate limiting: re-key from pubkey to the actor id — structurally a no-op
  (`rate_limit.rs:201-208` keys on `(community, principal)`).
- Retire the NIP-98 replay guard (`buzz-pubsub/src/nip98_replay.rs`) or repurpose
  its Redis `SET NX` primitive for bearer nonce/idempotency if needed.

**Acceptance:** a scoped key authenticates over WS and HTTP; an over-broad read is
denied by scope; the seven read gates (Invariant 1) hold for the bearer principal
(add tests mirroring the existing p-gate/author-only tests).

### Lane C — Relay ingest / server-authored rows
**Depends on:** A, B.

- Add `IngestAuth::ApiKey { actor, scopes, channel_ids }`
  (`handlers/ingest.rs:62-131`).
- Remove the two crypto chokepoints from the write path:
  - Event-body Schnorr verify (`ingest.rs:1463-1478`) — delete.
  - `event.pubkey == auth.pubkey()` equality (`ingest.rs:1499-1503`) — replace
    with **server authoring**: stamp the row's actor from the authenticated
    principal (client no longer supplies a signed author). Compute `id` as a
    content hash server-side (or a UUID) — keep it 32 bytes for column-shape
    stability.
- Keep the rest of the pipeline intact: scope check (`required_scope_for_kind`,
  `ingest.rs:198-305`), command routing, ban/timeout backstop, channel resolution
  (`extract_channel_id`, h-tag scoping), membership gate
  (`check_channel_membership`, `ingest.rs:493-523`), per-kind validators, DB write
  + thread counters + `dispatch_persistent_event` fanout + audit.
- `buzz-core`: delete `verification.rs` (`verify_event`) from the write path; keep
  `StoredEvent` as the record wrapper (drop the `verified` flag or hard-set true).

**Acceptance:** a client POSTs an unsigned intent; the server authors and stores
the row with the authenticated actor; thread counters, fanout, and audit all fire;
relay-only and scope rules still reject disallowed writes.

### Lane D — Relay reads / live (REQ-shaped, preserve gates)
**Depends on:** A, B.

- Keep `POST /query` and `POST /count` returning the same row shape (filter → row
  array). Only auth changes (bearer). The NIP-01 `Filter → EventQuery` translation
  (`handlers/req.rs::build_event_query_from_filter`) stays.
- WS: reduce REQ to a **channel-topic + filter subscription** returning the same
  row shape and a terminal EOSE, then a live stream over the existing Redis
  fanout. Retain `SubscriptionRegistry` enough to route channel/global topics;
  filter-matching (`buzz_core::filter::filters_match`) can stay as the post-filter.
- **Preserve all read gates (Invariant 1) and the send-time re-check (Invariant
  2)** for the bearer principal.
- `sig` in responses: emit `""`/omit; document the wire change. Clients already
  tolerate empty `sig` (optimistic rows; OK-placeholder sets `sig: ""`).

**Acceptance:** desktop + mobile timeline reads return near-identical row arrays;
p-gated/author-only/result-gated kinds remain unreadable by non-recipients;
conformance suite green.

### Lane E — Media (Blossom → bearer)
**Depends on:** B. **NOTE: brief mis-scoped this as "untouched" — it is not.**

- `crates/buzz-media/src/auth.rs`: the entire module is Schnorr/Blossom-native
  (`auth_event.verify()`, kind 24242, BUD-01/11 tag semantics). Replace upload
  auth (`verify_blossom_upload_auth`) and optional download auth
  (`authenticate_media_read`, `api/media.rs:489-514`) with bearer validation +
  `files:{read,write}` scope + membership on the resolved actor.
- Keep S3 storage (`storage.rs`) and SHA-256 content-addressing (`upload.rs`) —
  identity-agnostic, survive as-is.
- Upload attribution (`MediaUploaded` audit, `api/media.rs:429`) records the actor.
- Concurrency/quota keys move from pubkey to actor id (`api/media.rs:91-126`).

**Acceptance:** authenticated bearer upload/download works; `x`-tag body-hash
binding replaced by direct body hashing; media audit entries carry the actor.

### Lane F — Audit external anchoring
**Depends on:** A. **Mostly additive.**

- Keep `crates/buzz-audit/` chain logic (`compute_hash`, genesis, verify) and the
  Postgres storage.
- `actor_pubkey` column → actor id (opaque bytes already; field-semantics only).
- **New:** periodic head-hash export — read `MAX(seq)` head `hash` per community,
  write it to S3 with Object Lock (WORM) / out-of-band anchor, on an interval and
  at shutdown. New small module + config (`BUZZ_AUDIT_ANCHOR_*`).
- **Cutover genesis:** at flag flip, append a genesis entry whose `detail`
  references the last Nostr-era head hash + timestamp.

**Acceptance:** `verify_chain` still passes; head hash appears in the WORM bucket;
tamper test still detects mutation.

### Lane G — Git (bearer transport + server-authored NIP-34)
**Depends on:** A, B, C. **Largest peripheral lane.**

- **Transport auth:** replace `GitAuth::from_request_parts` NIP-98 verification
  (`crates/buzz-relay/src/api/git/transport.rs:69-218`) with `Authorization:
  Bearer` validation → actor. Replace the policy-hook pubkey→role lookup
  (`api/git/policy.rs:173-414`) with actor→role. Object-storage keys currently use
  the 64-hex owner pubkey as a path segment (`manifest.rs:181`,
  `transport.rs:259-267`) — key them on the actor id hex instead (keep 64 hex chars
  for path-shape stability).
- **Commit signing:** drop `git-sign-nostr` auto-config from the dev-mcp shim
  (`crates/buzz-dev-mcp/src/shim.rs:184-196`); it is advisory-only and never
  verified server-side — safe to delete. Delete the `git-sign-nostr` crate.
- **Credential helper:** replace `git-credential-nostr` (signs kind:27235) with a
  bearer credential helper (emit `Authorization: Bearer <key>`); rewrite
  `shim.rs::build_git_env` (`178-216`).
- **NIP-34 record model stays** (decision 3): kinds 30617/30618/1617-1633 remain,
  now **server-authored** like every other kind. Repo creation still triggers on a
  30617 write (`handlers/side_effects.rs:2385-2403`); ref-state 30618 still emitted
  per push. Desktop `projects` (~15k LOC) and web `repos` keep reading these via
  the retained REQ-shaped path — **no data-layer rewrite**.

**Acceptance:** clone/push authenticate via bearer; repo create/list, ref-state,
PRs/issues/patches/status still round-trip through desktop `projects` + web
`repos` + CLI (`repos.rs`, `patches.rs`, `pr.rs`, `issues.rs`).

### Lane H — SDK + CLI + agent surface
**Depends on:** B, C, D.

- `buzz-sdk`: builders already sign-free; keep the validation/tag logic. Change the
  "caller signs" contract to "caller sends bearer-authed intent". No builder
  rewrites needed if the envelope stays (decision 2).
- `buzz-cli`: the swap concentrates in `crates/buzz-cli/src/client.rs`:
  - Replace `sign_nip98` / `sign_blossom_get` / `sign_blossom_upload` with a
    `Authorization: Bearer` header helper; delete `with_auth_tag`/`x-auth-tag`.
  - `sign_event` (`client.rs:588`) becomes a no-op envelope builder (server
    authors). The **110 `EventBuilder` call sites across `commands/*.rs` stay
    as-is** (they build tags, not signatures) — this is the payoff of decision 2.
  - `BUZZ_PRIVATE_KEY` → `BUZZ_API_KEY` (`crates/buzz-cli/src/lib.rs:73`,
    `1731-1735`).
- ACP harness env injection: `BUZZ_PRIVATE_KEY` → `BUZZ_API_KEY` in the three
  slots (`buzz-acp/src/config.rs:243`, inherited-env spawn path,
  `buzz-acp/src/lib.rs:4056-4081`). `BUZZ_AUTH_TAG` (NIP-OA delegation) collapses
  into the key's stored scope — remove forwarding + owner-resolution
  (`lib.rs:120-135`, `setup_mode.rs:318`).
- `buzz-dev-mcp`: `view_image.rs:299` Blossom GET → bearer; git shim per Lane G.
- **Untouched:** `buzz-persona` (no identity/crypto), `buzz-agent` LLM-provider
  OAuth (`auth.rs` — orthogonal), `sprig` (dispatcher only). `buzz-workflow` is a
  **rename only** — `author`/`owner_pubkey` become opaque actor strings, drop the
  `npub` display filter (`executor.rs:191-196`); no signature logic exists to
  replace, approval gates are unbuilt stubs.

**Acceptance:** CLI authenticates with a bearer key and all subcommands work
against the relay; agents launch with `BUZZ_API_KEY` and post/read.

### Lane I — Clients (desktop, web, mobile)
**Depends on:** B, C, D, E. **Reads change least; auth + writes change.**

- **Desktop (easiest):**
  - Auth: replace NIP-42 handshake (`relayClientSession.ts:845`,
    `readOnlyRelayClient.ts:259`) + `create_auth_event`/`sign_event`
    (`src-tauri/.../identity.rs`) with bearer at connect; store the API key in the
    keyring where the nsec lives today (`identity.rs:170-196`).
  - Writes: most mutations already go through server-mediated intent Tauri commands
    (`tauri.ts`) — those change only their auth header. The few WS
    `sign_event`+publish paths (plain message, typing, presence, status) become
    intent POSTs. Delete `sign_event`/`create_auth_event`/`nip44_*` from
    `identity.rs`.
  - Reads: `POST /query` (feed/search/threads via `tauri.ts`) changes only auth;
    the REQ timeline path (`relayClientSession.ts`, `relayChannelFilters.ts`)
    adapts its transport but returns the same rows — the timeline cache/render
    layer is untouched. Keep `desktop/src/shared/constants/kinds.ts` (kind ints
    retained). Remove the lone client-side `verifyEvent` guard
    (`shared/lib/authors.ts:44`) — moot under server-authored rows.
- **Web (trivial):** swap NIP-07/ephemeral signing (`nostr-signer.ts`,
  `nostr-client.ts::makeAuthEvent`) + NIP-98 (`nip98.ts`) for bearer. Repo browser
  reads (`use-repos.ts`, `use-repo-refs.ts`) change only auth.
- **Mobile (heaviest):** remove Dart-side signing (`signed_event_relay.dart`),
  local nsec storage (`auth_provider.dart`), NIP-42 socket auth
  (`relay_socket.dart`), NIP-98 header builder
  (`relay_session.dart::buildNip98AuthHeader`). Every feature write provider that
  calls `submit(...)` becomes a bearer-authed intent POST. Reads
  (`fetchHistory`/`subscribe`/`queryRelay`) adapt transport, keep row shape. Keep
  `nostr_models.dart` kind ints. Drop the Dart `nostr` signing dependency and the
  pairing feature (Lane K).

**Acceptance:** each client logs in with a provisioned key, reads timelines
(near-identical rows), and posts/reacts/edits via intent; no client holds a
private key.

### Lane J — Admin (key issuance / revocation)
**Depends on:** A, B.

- `buzz-admin` gains `issue-key --actor --scope [--channel ...] [--expires]`
  (generate random key, store `hash(key)` + scope + actor, print plaintext once),
  `revoke-key`, `list-keys`, `rotate-key` (revoke+reissue). Reuses
  `buzz-db/src/api_token.rs`. Replaces `generate-key`.
- Optional admin HTTP surface for the desktop "issue key" UX (self-mint path
  already exists: `created_by_self_mint`).
- `add-member`/`remove-member` key on actor id; role logic (`validate_role`)
  unchanged; relay-signed membership rosters (kind:13534) carry actor identifiers.

**Acceptance:** admin issues a scoped key, an actor uses it, admin revokes it and
the actor is denied.

### Lane K — Delete pairing (NIP-AB)
**Depends on:** I (mobile/desktop pairing UX removal). **Pure deletion.**

- Remove `crates/buzz-pair-relay`, `crates/buzz-pairing-cli`,
  `crates/buzz-core/src/pairing/`, kind `KIND_PAIRING` (24134), and the
  desktop/mobile pairing surfaces. Its only purpose was transferring a private key
  between devices — deleted by the API-key model. "Add a device" becomes "issue a
  new key" (Lane J).

**Acceptance:** repo builds without the pairing crates; no dangling references.

### Lane L — Tests / E2E
**Depends on:** all. **Runs continuously.**

- `buzz-test-client`: add a bearer connect/publish variant (mirrors CLI
  `client.rs`). Retire NIP-42 challenge tests, NIP-17 gift-wrap interop, and
  generic nostr-relay-compat tests (`e2e_nostr_interop.rs`) — obsolete under the
  new model. Port `e2e_relay.rs`/`e2e_media*.rs` to bearer.
- Add tests for the seven read gates under bearer, scope-denial, revocation,
  multi-tenant isolation of keys.
- Keep the multi-tenant conformance harness green throughout.

---

## 6. Sequencing / critical path

```
Phase 0  A (schema/actor id, sig-optional)                [foundation]
Phase 1  B (bearer + scope activation)  ── behind BUZZ_AUTH_MODE flag
Phase 2  C (server-authored ingest)  +  D (reads/live, gates)   [core group]
Phase 3  E (media)   F (audit anchor)   J (admin keys)    [parallel]
Phase 4  H (sdk/cli/agent surface)                        [depends B/C/D]
Phase 5  G (git bearer + server-authored NIP-34)          [large, parallel w/ H]
Phase 6  I (clients: desktop → web → mobile)              [depends B–E]
Phase 7  K (delete pairing)   L (test cutover)            [cleanup]
Phase 8  Cutover: flip BUZZ_AUTH_MODE=apikey, append audit genesis,
                  remove nostr auth doorway + nostr crate usage
```

Lanes A–D are the tightly-coupled core (shared event/identity model) and are hard
to stage incrementally — land them as a coordinated group behind the flag. E, F,
G, H, I, J parallelize once B/C/D stabilize. The `nostr` crate dependency
(`Cargo.toml:61`) is the last thing removed, after all `nostr::{Event, Filter,
Keys}` type usages are replaced or made vestigial.

---

## 7. Data migration & cutover

- **Historical events:** leave as-is (verify-once already happened at original
  ingest; treat as immutable legacy). Do **not** re-project. Making `sig` nullable
  is backward-compatible with existing rows.
- **Identity backfill:** existing `pubkey` values remain valid opaque actor ids;
  mint an API key per existing active actor (admin batch) mapping to their existing
  pubkey-as-actor so history stays attributed.
- **Audit:** append a cutover **genesis** entry per community referencing the last
  Nostr-era head hash; begin external anchoring immediately after.
- **Flag flip:** `BUZZ_AUTH_MODE=apikey` disables the NIP-42/98 doorways. Keep a
  short dual-run window if clients roll out asynchronously.

---

## 8. Cross-cutting risks

1. **Opening the read log** (highest). Forgetting to re-derive `accessible_channels`
   or the p-gate/author-only/result-gated/engram gates for the bearer principal
   leaks private events. Mitigation: Invariant-1 test suite is a merge gate for
   Lanes B and D.
2. **Media mis-scoped** — do not skip Lane E; Blossom auth is Nostr-native.
3. **Git ownership re-keying** — object-storage paths and repo-name reservation use
   the pubkey; keep the actor id 64-hex to preserve path shape and avoid a storage
   migration.
4. **Mobile scope** — it is a full Dart Nostr client; budget it as the largest
   client lane.
5. **Wire `sig` removal** — confirm every client tolerates empty/absent `sig`
   (desktop/mobile already emit `sig:""` in places; audit web).
6. **Multi-tenant regressions** — keep the conformance harness green on every lane.

---

## 9. Out of scope (v2)

- **Client-side E2E encryption** (NIP-44/NIP-17) for DMs and owner-encrypted kinds.
  Removed in v1 by decision; revisit as a v2 feature with server-managed or
  per-actor key material if confidentiality-from-server becomes a requirement.
- Full JSONL audit-file rewrite (kept Postgres chain + WORM anchor instead).
- Single-org collapse of the multi-tenant schema (kept multi-tenant).
- Full REST re-model of NIP-34 git objects (kept server-authored event kinds).

---

## 10. Key file index (for agents)

- Identity/records: `crates/buzz-core/src/{event.rs,verification.rs,filter.rs,kind.rs}`
- Auth: `crates/buzz-auth/src/{lib.rs,scope.rs,access.rs,nip42.rs,nip98.rs}`
- DB: `crates/buzz-db/src/{event.rs,api_token.rs,lib.rs}`, `migrations/0001_initial_schema.sql`
- Relay ingest/read/fanout: `crates/buzz-relay/src/handlers/{ingest.rs,event.rs,req.rs,count.rs}`,
  `crates/buzz-relay/src/api/{bridge.rs,media.rs}`, `crates/buzz-relay/src/{connection.rs,protocol.rs,subscription.rs}`
- Git: `crates/buzz-relay/src/api/git/{transport.rs,policy.rs,manifest.rs}`,
  `crates/{git-sign-nostr,git-credential-nostr}`, `crates/buzz-dev-mcp/src/shim.rs`
- Audit: `crates/buzz-audit/src/{service.rs,entry.rs,hash.rs}`
- SDK/CLI/agents: `crates/buzz-sdk/src/builders.rs`, `crates/buzz-cli/src/{client.rs,lib.rs}`,
  `crates/buzz-acp/src/{config.rs,lib.rs}`, `crates/buzz-agent/src/{lib.rs,auth.rs}`
- Admin: `crates/buzz-admin/src/main.rs`
- Clients: `desktop/src/shared/api/{relayClientSession.ts,relayChannelFilters.ts,tauri.ts}`,
  `desktop/src-tauri/src/commands/identity.rs`, `web/src/shared/lib/{nostr-client.ts,nostr-signer.ts}`,
  `mobile/lib/shared/relay/{relay_session.dart,signed_event_relay.dart}`, `mobile/lib/shared/auth/auth_provider.dart`
- Pairing (delete): `crates/{buzz-pair-relay,buzz-pairing-cli}`, `crates/buzz-core/src/pairing/`
