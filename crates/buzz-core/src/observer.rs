//! Agent observer frame helpers.
//!
//! Observer frames are transient, owner-scoped agent telemetry/control messages.
//! They use a Buzz ephemeral event kind and carry **plaintext JSON** in the
//! event content. Frame routing, direction, and read authorization live entirely
//! in the event tags (`p` recipient, `agent`, `frame`) and the relay's `#p` read
//! gates — not in the content — so no client-side encryption is used. The relay
//! reveals a frame only to the pubkey named in its `p` tag (see
//! [`crate::kind::P_GATED_KINDS`]); confidentiality is a server-side property in
//! both auth modes (nostr NIP-42 and API-key bearer).

use nostr::Event;
use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;

/// Tag name that identifies the agent pubkey the observer frame belongs to.
pub const OBSERVER_AGENT_TAG: &str = "agent";
/// Tag name that identifies the cleartext frame direction.
pub const OBSERVER_FRAME_TAG: &str = "frame";
/// Frame value for agent-to-owner observer telemetry.
pub const OBSERVER_FRAME_TELEMETRY: &str = "telemetry";
/// Frame value for owner-to-agent observer control commands.
pub const OBSERVER_FRAME_CONTROL: &str = "control";
/// Maximum observer plaintext JSON size accepted by helpers.
pub const OBSERVER_MAX_PLAINTEXT_LEN: usize = 65_535;

/// Errors returned by observer payload encode/decode helpers.
#[derive(Debug, Error)]
pub enum ObserverPayloadError {
    /// JSON serialization or deserialization failed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// Plaintext JSON exceeded the observer plaintext size limit.
    #[error("observer plaintext exceeds {max} bytes (got {got})")]
    PlaintextTooLarge {
        /// Maximum accepted plaintext bytes.
        max: usize,
        /// Actual plaintext byte count.
        got: usize,
    },
    /// A payload field violated a NIP-AM numeric constraint.
    #[error("invalid payload field: {0}")]
    InvalidPayload(String),
}

/// Returns true when `content` is a plausible plaintext observer frame payload:
/// non-empty and within the observer plaintext size budget.
///
/// Client-side encryption was removed (server-trust model); frame content is now
/// plaintext JSON and routing/authorization live in the event tags and the
/// relay's read gates. This is a cheap shape check used by frame builders and the
/// relay route validator — it does not parse the JSON.
pub fn content_fits_observer_frame(content: &str) -> bool {
    !content.is_empty() && content.len() <= OBSERVER_MAX_PLAINTEXT_LEN
}

/// Serialize an observer payload to plaintext JSON for the frame content.
///
/// The server-trust model dropped client-side encryption: observer frames carry
/// plaintext JSON, protected in transit and at rest by the relay's `#p` read
/// gates rather than by NIP-44. Returns
/// [`ObserverPayloadError::PlaintextTooLarge`] when the serialized JSON exceeds
/// [`OBSERVER_MAX_PLAINTEXT_LEN`].
pub fn encode_observer_payload<T: Serialize>(payload: &T) -> Result<String, ObserverPayloadError> {
    let json = serde_json::to_string(payload)?;
    if json.len() > OBSERVER_MAX_PLAINTEXT_LEN {
        return Err(ObserverPayloadError::PlaintextTooLarge {
            max: OBSERVER_MAX_PLAINTEXT_LEN,
            got: json.len(),
        });
    }
    Ok(json)
}

/// Deserialize an observer payload from an event's plaintext JSON content.
///
/// Content that is not valid JSON for `T` — including a legacy NIP-44 ciphertext
/// written before encryption was removed — yields
/// [`ObserverPayloadError::Json`]; callers treat that as an absent/skippable
/// frame rather than a hard error. Content over [`OBSERVER_MAX_PLAINTEXT_LEN`]
/// is rejected before parsing.
pub fn decode_observer_payload<T: DeserializeOwned>(
    event: &Event,
) -> Result<T, ObserverPayloadError> {
    if event.content.len() > OBSERVER_MAX_PLAINTEXT_LEN {
        return Err(ObserverPayloadError::PlaintextTooLarge {
            max: OBSERVER_MAX_PLAINTEXT_LEN,
            got: event.content.len(),
        });
    }
    Ok(serde_json::from_str(&event.content)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::{EventBuilder, Keys, Kind, Tag};

    #[test]
    fn observer_payload_round_trips_as_plaintext() {
        let sender = Keys::generate();
        let recipient = Keys::generate();
        let payload = serde_json::json!({
            "type": "turn_started",
            "turnId": "turn-1"
        });
        let encoded = encode_observer_payload(&payload).expect("encode payload");
        assert!(content_fits_observer_frame(&encoded));

        let event = EventBuilder::new(
            Kind::Custom(crate::kind::KIND_AGENT_OBSERVER_FRAME as u16),
            encoded,
        )
        .tags([Tag::public_key(recipient.public_key())])
        .sign_with_keys(&sender)
        .expect("sign event");
        let decoded: serde_json::Value = decode_observer_payload(&event).expect("decode payload");
        assert_eq!(decoded, payload);
    }

    #[test]
    fn decode_rejects_non_json_content() {
        // A legacy ciphertext (or any non-JSON blob) must fail to parse rather
        // than panic, so consumers can skip it as an absent frame.
        let sender = Keys::generate();
        let event = EventBuilder::new(
            Kind::Custom(crate::kind::KIND_AGENT_OBSERVER_FRAME as u16),
            "not json at all",
        )
        .tags([Tag::public_key(sender.public_key())])
        .sign_with_keys(&sender)
        .expect("sign event");

        assert!(matches!(
            decode_observer_payload::<serde_json::Value>(&event),
            Err(ObserverPayloadError::Json(_))
        ));
    }

    #[test]
    fn empty_content_does_not_fit_observer_frame() {
        assert!(!content_fits_observer_frame(""));
        assert!(content_fits_observer_frame("{}"));
    }
}
