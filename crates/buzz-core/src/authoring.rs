//! Server-side event authoring for API-key auth mode.
//!
//! In `apikey` mode the client submits an UNSIGNED "intent" envelope — a Nostr
//! event whose `kind`, `tags`, `content`, and `created_at` carry the request,
//! but whose `id`, `pubkey`, and `sig` are placeholders the caller need not
//! populate meaningfully. After authenticating the bearer principal, the relay
//! *authors* the row: it stamps the authenticated actor as the event author,
//! recomputes the NIP-01 event id over the server-controlled fields, and clears
//! the signature.
//!
//! This is the write-path counterpart to [`crate::verification::verify_event`],
//! which still guards the `nostr`-mode (signed) write path. The two are mutually
//! exclusive: `nostr` mode verifies a client signature, `apikey` mode authors
//! the row and never verifies (there is no signature).

use nostr::secp256k1::schnorr::Signature;
use nostr::{Event, EventId, PublicKey};

use crate::error::VerificationError;

/// The number of bytes in a Schnorr signature (BIP-340).
const SCHNORR_SIG_LEN: usize = 64;

/// Construct the all-zero placeholder signature used for server-authored
/// (unsigned) rows.
///
/// Server-authored events carry no client signature. The `nostr::Event` wire
/// type nonetheless requires a syntactically valid 64-byte `Signature`, so an
/// all-zero value stands in. It is never verified on any read or write path in
/// `apikey` mode (Lane A made the stored `sig` column optional and rehydrates
/// empty/placeholder signatures identically).
fn empty_signature() -> Result<Signature, VerificationError> {
    // `from_slice` only checks the length (64 bytes) — no point validation — so
    // this cannot fail for a fixed 64-byte input. Propagated via `?` rather than
    // unwrapped to keep the production path free of `unwrap`/`expect`.
    Ok(Signature::from_slice(&[0u8; SCHNORR_SIG_LEN])?)
}

/// Server-author an unsigned intent event (API-key auth mode).
///
/// Mutates `intent` in place, replacing the client-supplied identity fields with
/// server-controlled values:
///
/// - `pubkey` is set to the authenticated `actor` id. Per refactor decision #2
///   the retained 32-byte `pubkey` column now carries an opaque actor id.
/// - `id` is recomputed as the NIP-01 event id — `SHA-256` over the canonical
///   `[0, pubkey, created_at, kind, tags, content]` serialization the `nostr`
///   crate produces ([`EventId::new`]) — so it is deterministic, exactly 32
///   bytes (preserving the `events.id` column shape), and bound to the
///   server-stamped author. Any client-supplied `id` is ignored.
/// - `sig` is cleared to an all-zero placeholder (see [`empty_signature`]).
///
/// `created_at`, `kind`, `tags`, and `content` are taken from the intent
/// unchanged — they are the client's request. This function never verifies a
/// signature (there is none) and must only be called in `apikey` mode; in
/// `nostr` mode the signed write path is guarded by
/// [`crate::verification::verify_event`] instead.
///
/// # Errors
///
/// Returns [`VerificationError::Secp`] only if the fixed 64-byte placeholder
/// signature cannot be constructed — an internal invariant independent of any
/// input.
pub fn author_event_server_side(
    intent: &mut Event,
    actor: PublicKey,
) -> Result<(), VerificationError> {
    intent.pubkey = actor;
    intent.id = EventId::new(
        &intent.pubkey,
        &intent.created_at,
        &intent.kind,
        &intent.tags,
        &intent.content,
    );
    intent.sig = empty_signature()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::{EventBuilder, Keys, Kind, Tag};

    /// Build a signed event whose fields we then treat as an "intent" whose
    /// identity fields the server will overwrite.
    fn signed_intent(kind: u16, content: &str, tags: Vec<Tag>) -> Event {
        let keys = Keys::generate();
        EventBuilder::new(Kind::Custom(kind), content)
            .tags(tags)
            .sign_with_keys(&keys)
            .expect("sign")
    }

    #[test]
    fn stamps_actor_as_author() {
        let actor = Keys::generate().public_key();
        let mut event = signed_intent(9, "hello", vec![]);
        let original_author = event.pubkey;
        assert_ne!(original_author, actor);

        author_event_server_side(&mut event, actor).expect("author");
        assert_eq!(event.pubkey, actor, "author must be stamped to the actor");
    }

    #[test]
    fn computes_deterministic_32_byte_id() {
        let actor = Keys::generate().public_key();
        let mut a = signed_intent(9, "same", vec![]);
        let mut b = signed_intent(9, "same", vec![]);
        // Different client-supplied ids/authors up front.
        assert_ne!(a.id, b.id);
        // Align created_at so the canonical serialization matches.
        b.created_at = a.created_at;

        author_event_server_side(&mut a, actor).expect("author a");
        author_event_server_side(&mut b, actor).expect("author b");

        // 32-byte column shape preserved.
        assert_eq!(a.id.as_bytes().len(), 32);
        // Same actor + created_at + kind + tags + content ⇒ identical id.
        assert_eq!(a.id, b.id, "id must be a deterministic content hash");

        // The stamped id is exactly the NIP-01 id over the server-stamped fields.
        let expected = EventId::new(&actor, &a.created_at, &a.kind, &a.tags, &a.content);
        assert_eq!(a.id, expected);
    }

    #[test]
    fn different_actor_yields_different_id() {
        let mut a = signed_intent(9, "same", vec![]);
        let mut b = a.clone();
        author_event_server_side(&mut a, Keys::generate().public_key()).expect("author a");
        author_event_server_side(&mut b, Keys::generate().public_key()).expect("author b");
        assert_ne!(a.id, b.id, "author is bound into the id");
    }

    #[test]
    fn clears_signature_to_empty_placeholder() {
        let actor = Keys::generate().public_key();
        let mut event = signed_intent(9, "hi", vec![]);
        author_event_server_side(&mut event, actor).expect("author");
        assert_eq!(
            event.sig.serialize(),
            [0u8; SCHNORR_SIG_LEN],
            "server-authored rows carry an all-zero placeholder signature"
        );
    }

    #[test]
    fn preserves_content_kind_and_tags() {
        let actor = Keys::generate().public_key();
        let tags = vec![Tag::parse(["h", "channel-uuid"]).expect("tag")];
        let mut event = signed_intent(9, "body text", tags.clone());
        let created_at = event.created_at;
        author_event_server_side(&mut event, actor).expect("author");

        assert_eq!(event.content, "body text");
        assert_eq!(event.kind, Kind::Custom(9));
        assert_eq!(event.created_at, created_at);
        assert_eq!(event.tags.to_vec(), tags);
    }
}
