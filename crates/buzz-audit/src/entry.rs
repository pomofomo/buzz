use buzz_core::CommunityId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::action::AuditAction;

/// A materialised audit log entry as stored in `audit_log`.
///
/// Rows are keyed `(community_id, seq)`: `seq` is monotonic *within one
/// community*, and `prev_hash` chains to the previous entry *of the same
/// community*. The chain is independent per tenant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Server-resolved community this entry belongs to. Leads the primary key.
    pub community_id: Uuid,
    /// Sequence number, monotonic within `community_id` (starts at 1).
    pub seq: i64,
    /// SHA-256 of this entry's fields including `community_id` and `prev_hash`.
    pub hash: Vec<u8>,
    /// SHA-256 of the previous entry in *this community's* chain, or `None` for
    /// the community's first entry (hashed as [`crate::hash::GENESIS_HASH`]).
    pub prev_hash: Option<Vec<u8>>,
    /// Action that was performed.
    pub action: AuditAction,
    /// Opaque actor id — the raw bytes of the authenticated principal, if the
    /// action has one. Post-cutover this holds the opaque actor id derived from
    /// the presented API key; for legacy (Nostr-era) rows it is the actor's
    /// Nostr pubkey. The column name is retained for schema stability
    /// (decision #4: no DDL change); only the field's semantics widened. The
    /// audit crate treats it as opaque bytes and folds it into the chain hash.
    pub actor_pubkey: Option<Vec<u8>>,
    /// Generic identifier of the object acted upon (event id hex, channel UUID,
    /// media sha256, …), if any. The relay resolves it under `community_id`;
    /// it never names an object in another community.
    pub object_id: Option<String>,
    /// Arbitrary JSON context. **Included in the hash** (serialized with sorted
    /// keys for determinism) so tampering with it is detectable.
    pub detail: serde_json::Value,
    /// When the entry was recorded.
    pub created_at: DateTime<Utc>,
}

/// Input for appending a new audit entry. `seq`, `prev_hash`, `hash`, and
/// `created_at` are assigned by [`crate::service::AuditService::log`].
///
/// `community_id` is the **server-resolved** tenant (from the request's
/// `TenantContext`), never a client-supplied value — the same provenance rule
/// the whole multi-tenant model rests on.
///
/// Not `Serialize`/`Deserialize`: this is an in-process input struct (consumed
/// by `AuditService::log`, threaded through the in-memory audit sink), never
/// crossing a wire or DB boundary as a whole. Keeping it non-deserializable
/// reinforces the fence — there is no path by which a client-supplied blob
/// becomes a `NewAuditEntry` (and thus a `CommunityId`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAuditEntry {
    /// Server-resolved community this entry belongs to. Typed as [`CommunityId`]
    /// (not a raw `Uuid`) so the provenance rule is visible in the signature:
    /// the only ways to obtain one are host resolution or a server-scoped DB
    /// row — never a value parsed from client input.
    pub community_id: CommunityId,
    /// Action that was performed.
    pub action: AuditAction,
    /// Opaque actor id — the raw bytes of the authenticated principal, if the
    /// action has one. Post-cutover this is the opaque actor id derived from
    /// the API key; for legacy rows it is the actor's Nostr pubkey. Opaque
    /// bytes to the audit crate; folded into the chain hash.
    pub actor_pubkey: Option<Vec<u8>>,
    /// Generic identifier of the object acted upon, if any.
    pub object_id: Option<String>,
    /// Arbitrary JSON context included in the hash.
    ///
    /// **Never bearer-token material.** This field is opaque to the audit
    /// crate and persisted verbatim; callers must not write tokens, passwords,
    /// or other secrets here. `AuthSuccess`/`AuthFailure` entries carry only
    /// outcome metadata — the token has no slot in this type, and `detail` must
    /// not become one.
    pub detail: serde_json::Value,
}
