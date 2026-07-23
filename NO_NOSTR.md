# Why Buzz Drops the Nostr Substrate

### The case for API-key identity, server-authored records, and a hash-chained audit log — for an organizational, self-hosted deployment.

This document argues, in earnest, for the rearchitecture implemented on this
branch (see [REFACTOR.md](REFACTOR.md)): removing Nostr as the **identity,
transport, and record model** and replacing it with per-actor **API keys**,
**server-authored records**, and a **hash-chained, WORM-anchored audit log** —
while keeping the Postgres schema, the query API, Redis realtime, search, and
media storage exactly as they were.

Its companion, [NOSTR.md](NOSTR.md), documents Nostr interoperability — how
third-party clients like Chachi, 0xchat, and `nak` speak to the relay. That
document is the strongest case *for* Nostr, and it is worth reading first,
because the punchline of this one is: **almost every property Nostr optimizes
for is a property a single organization does not want — and several are
properties it actively wants the opposite of.**

---

## TL;DR

Nostr is an excellent design for a **permissionless, decentralized, censorship-
resistant network of mutually-distrusting strangers**. Buzz, deployed inside a
company, is the **exact opposite environment**: one trust root (the org), a
curated and closed membership, a strong desire for central governance, and hard
compliance and audit requirements. Running a decentralization protocol on a
centralized, trusted deployment means paying a large complexity, UX, and
governance tax for guarantees you don't need — and being unable to get the
guarantees you *do* need (revocation, least-privilege, offboarding, a defensible
audit trail). The rearchitecture keeps the genuinely good ideas (a uniform
event schema, materialized views, realtime fan-out, content-addressed media) and
discards the substrate that only made sense for decentralization.

---

## 1. The threat-model mismatch

Every design decision is downstream of a threat model. Nostr's is:

- No central authority anyone trusts.
- Identity is a keypair the **user** holds; relays are interchangeable and
  potentially hostile; clients are untrusted; censorship is the adversary.
- Portability and permissionless participation are the whole point.

A self-hosted organizational Buzz has the inverted model:

- There **is** a trusted central authority — the org and its admins. That's not a
  weakness to engineer around; it's the operating reality and the source of
  legitimacy.
- Membership is closed and curated (employees, contractors, and their agents).
- The org **wants** central control: it must be able to revoke access, scope it,
  offboard people instantly, and prove what happened.

When the threat model inverts, the optimal design inverts with it. Insisting on
Nostr here is like deploying a consensus blockchain to store a company's HR
records: technically impressive, and the wrong tool.

---

## 2. Identity your organization can actually govern

**Nostr identity is a secp256k1 private key the user holds — and that is a
governance dead end.**

- You cannot **revoke** a keypair. It is the user's forever. Offboarding someone
  means removing them from membership lists while their cryptographic identity
  remains permanently valid — access control by convention, not by capability.
- Keypairs don't **expire**, **rotate**, or carry **scope**. Every key is
  all-or-nothing.
- A lost `nsec` is an unrecoverable lost identity. A leaked one is a permanent
  compromise. There is no "reset password."
- An entire subsystem existed *only* to paper over key custody: NIP-AB device
  **pairing** — a sidecar relay, an ECDH+SAS handshake, and a CLI — whose sole
  job was to smuggle a private key from one device to another.

**API keys are how organizations already run identity**, and they fix all of it:

- **Issued, scoped, expirable, revocable, rotatable** by the admin
  (`buzz-admin issue-key | revoke-key | rotate-key`). Offboarding is one
  `revoke-key` — instant and total.
- **Least privilege by construction.** A read-only dashboard key, a
  single-channel agent key, an admin key — each granted exactly the scopes it
  needs (`messages:*`, `repos:*`, `files:*`, `admin:*`, …), enforced at the gate.
- **Lost key → revoke + reissue.** Compromised agent → revoke *one* key, not
  re-key the whole org.
- The whole pairing subsystem **deletes itself**: "add a device" becomes "issue a
  new key." Less code, less protocol, less to attack.

This is not a downgrade in security — it is the difference between security you
can *administer* and security you can only *hope* holds.

---

## 3. Authorization Nostr never had

NIP-42 proves you possess a key. That is **authentication**, not
**authorization** — and Buzz always had to bolt membership checks on top because
the protocol had no native answer to "*what is this identity allowed to do?*"

The API-key model makes authorization first-class:

- **Scopes** express real capability boundaries (read vs write, per-channel,
  admin). The bearer principal's scopes are checked at the same gate that
  authenticates it.
- This matters most for **agents** — Buzz's headline use case. You want a coding
  agent to have `repos:write` + `messages:write` on *one* channel, not a full
  human-equivalent identity delegated through owner-attestation tags (NIP-OA).
  A scoped key says exactly that; a raw keypair cannot.

The old model reached for this and couldn't hold it: authentication succeeded and
*every* connection was then granted the full scope set. Real least-privilege was
structurally out of reach. Now it isn't.

---

## 4. The audit trail gets *stronger* — and simpler

The one Nostr property genuinely worth keeping is **tamper-evidence**: signed,
content-addressed events form a record you can't quietly alter. The
rearchitecture **keeps that property and improves it**, via a hash-chained
append-only audit log (`prev_hash → hash`, single-writer, `fsync`, with the head
hash anchored externally to WORM / S3 Object Lock).

Why this is better *for an org* than per-message signatures:

- Per-message non-repudiation is only as good as the assumption that keys are
  never shared or stolen. In a deployment where **the server provisions
  identity**, that assumption is already a fiction — so paying to verify a
  Schnorr signature on every event bought a guarantee the org couldn't actually
  rely on.
- An **append-only, externally-anchored ledger** is precisely the control model
  auditors, compliance, and security reviews already understand and ask for. It
  is *more* defensible in a SOC 2 / ISO review than "a pile of self-signed events
  in a database," not less.
- It is honest about the real trust boundary — trust the server/admin — and then
  makes that boundary **verifiable after the fact** (a compromised app layer
  can't rewrite history without breaking the chain, and the WORM-anchored head
  detects even a full-database rewrite).

We didn't remove tamper-evidence. We moved it to where it's enforceable and
auditable, and stopped spending CPU verifying signatures on a read path that
never needed them.

---

## 5. A dramatic cut in complexity and attack surface

Nostr dragged an enormous surface into the codebase to make a *permissionless
decentralized protocol* work: NIP-01/29/42/50/10/17, Blossom (BUD-01/11), Schnorr
verification on every event, gift-wrap encryption, the filter-matching and
subscription engine, replaceable / parameterized-replaceable event semantics, a
device-pairing sidecar relay, owner-attestation delegation, and git-signed-over-
Nostr. The overwhelming majority of that machinery exists solely to substitute
for a trusted coordinator — which a self-hosted org **has**.

Removing it is a security win, not just a tidiness win:

- **Whole crates deleted** — `git-sign-nostr`, `buzz-pair-relay`,
  `buzz-pairing-cli`, `buzz-core::pairing` — plus client-side signing code in
  *four* client platforms (desktop Rust, web TS, mobile Dart, the CLI).
- **Fewer subtle, dangerous invariants.** The read path had to juggle p-gates,
  author-only gates, result-gated kinds, and engram gates to avoid leaking
  private events through open subscription filters — complexity that exists
  because REQ filters are a powerful, general query surface exposed to clients.
  Server-authored, topic-scoped fan-out is far easier to reason about and to keep
  closed.
- **Smaller code is more secure code.** Every NIP is a spec to implement
  correctly; every signature path is a place to get verification subtly wrong.
  Deleting them removes classes of bugs, not just lines.

Crucially, we deleted the **substrate**, not the **data model**. The
"everything is an event" schema — the actually-good idea — stays. So do the
materialized views (thread counters, reactions, mentions), the Postgres FTS,
Redis pub/sub, and S3 media. The query API the clients read is unchanged.

---

## 6. Third-party client interop is a liability here, not a feature

[NOSTR.md](NOSTR.md)'s marquee capability is that any Nostr client — Chachi,
0xchat, `nak` — can connect directly to the relay. For a public social protocol,
open clients are the entire value proposition. **For a company's internal
communications and agent platform, they are a data-loss and compliance
nightmare:**

- Any unmanaged client can read and write internal data — no MDM, no client
  attestation, no DLP, no version control over what's talking to your relay.
- "Bring any client" is indistinguishable from "bring any exfiltration tool."
- Generic-relay compatibility means you inherit the union of every Nostr client's
  behavior and bugs as your effective API contract.

An organization wants a **known, first-party client surface** — its own desktop,
web, mobile, and CLI — and nothing else. Dropping generic Nostr-client
compatibility is a deliberate **reduction of an uncontrolled attack and
exfiltration surface**. The org's own clients keep working, unchanged, over the
same materialized query API.

---

## 7. Server-authored records remove client trust

In the Nostr model the **client** constructs and signs the canonical record, so
the client is trusted to form correct events — and a buggy or malicious client
can mint malformed or surprising ones. It also means maintaining event-
construction and signing logic in every client language.

Server-authoring inverts this to the org's advantage:

- The **server is the single writer of truth**. It authenticates the bearer,
  validates the intent, stamps the actor from the authenticated principal,
  computes the id, and writes the row. Clients send an *unsigned intent* — "post
  this content to this channel" — and the server decides what actually gets
  recorded.
- Cleaner invariants (the author of a row is, by construction, the authenticated
  principal — not a client-supplied claim to be checked), and **no client-side
  cryptography to keep correct across four platforms**.

Realtime doesn't regress: it's the same WebSocket and the same Redis fan-out. We
simply dropped NIP-01 REQ/subscription-filter semantics in favor of fanning out
server-authored rows over the topics that already existed.

---

## 8. Operational and UX wins that compound

- **No key-custody UX** — the single biggest failure mode of Nostr in practice
  (seed phrases, `nsec` loss, zero recovery). Users get an API key like any SaaS
  tool; admins manage it centrally; SSO/Okta can front it (the schema already
  carries `okta_user_id`).
- **Trivial agent provisioning** — issue a scoped key, inject `BUZZ_API_KEY`.
  No `BUZZ_PRIVATE_KEY`, no NIP-OA delegation chains, no owner-attestation tags.
- **Standard bearer auth = standard tooling.** `Authorization: Bearer <token>`
  works with `curl`, every HTTP library, API gateways, load balancers, and
  off-the-shelf rate limiters. Schnorr-signed NIP-98/Blossom envelopes work with
  none of them without custom code.

---

## 9. It maps onto how enterprises are actually governed

Revocation, least-privilege scopes, centralized issuance, instant offboarding,
immutable WORM-anchored audit logs, and the absence of un-revocable identities
are, almost line-for-line, what a security or compliance review asks for. The
Nostr model — user-held un-revocable keys, open client interop, no native
authorization — is genuinely hard to defend in that setting. The API-key model
**is** the established control vocabulary, so the system becomes easier to
certify, not harder.

---

## 10. The honest trade-offs

A persuasive argument that hides its costs isn't persuasive; it's marketing. Here
is what this design gives up, plainly:

1. **Per-message cryptographic non-repudiation against a compromised server or
   admin.** Attribution is now only as strong as the server, because the server
   both authenticates and writes. → For a single-org, trust-the-admin deployment
   this is the *correct* boundary, and the hash-chained WORM-anchored log keeps
   the record tamper-evident against everything below the server.
2. **Generic Nostr-client interoperability.** → As argued in §6, for an internal
   tool this is a liability removed, not a capability lost.
3. **Cross-relay portability / censorship resistance.** → These are decentralization
   properties. A company running its own relay for its own people does not want
   its internal comms to be portable to, or resistant to, *itself*.
4. **Client-side end-to-end encryption (NIP-44/NIP-17).** Dropped in v1 because
   per-actor private-key management was one of the overheads we set out to
   remove. → A deliberate v1 scope cut, revisitable later with server-managed or
   per-actor key material if confidentiality-from-the-server ever becomes a
   requirement.

Every item on this list is a decentralization guarantee. An organization that
runs the server *is* the trust root, and does not need to defend itself against
itself. Trading guarantees you don't want for governance you require is not a
regression — it is choosing the right tool.

---

## Conclusion

Nostr solved a hard problem beautifully: coordination without a trusted
authority. Buzz-in-an-organization has a trusted authority by definition, and
needs the things a trusted authority provides — revocable, scoped identity;
enforceable authorization; instant offboarding; a defensible, immutable audit
trail; and a controlled client surface. This rearchitecture keeps everything
that was good independent of the substrate — the uniform event schema, the
materialized query API, realtime fan-out, search, and media — and replaces the
decentralization machinery with the centralized-governance machinery the
deployment actually calls for. It is less code, a smaller attack surface, better
UX, and a system that a security team can actually stand behind.

The migration is flag-gated (`BUZZ_AUTH_MODE`), so this is not a leap of faith:
run both models side by side, verify, and flip when ready.
