//! API-key (bearer) auth coverage — the Lane L cutover of the E2E suite from the
//! Nostr signature model to `BUZZ_AUTH_MODE=apikey`.
//!
//! This file replaces the retired NIP-42 challenge / NIP-17 gift-wrap /
//! nostr-relay-compat tests (`e2e_nostr_interop.rs`, `nip42_host_binding_live.rs`)
//! with coverage of the API-key trust model:
//!
//! - **bearer connect + server-authored publish** round-trip via the new
//!   [`BuzzTestClient::connect_bearer`] / [`BuzzTestClient::publish_intent`]
//!   primitives;
//! - the **seven read gates** (REFACTOR.md Invariant 1) hold for a bearer
//!   principal — p-gated / author-only / result-gated kinds stay unreadable by a
//!   non-recipient;
//! - **scope denial** — a `messages:read`-only key cannot write;
//! - **token revocation** — a revoked key is rejected;
//! - **multi-tenant isolation** — a key minted in community A cannot act in B.
//!
//! ## Test layers
//!
//! Most assertions are **no-infra unit tests** (they run under a plain
//! `cargo test -p buzz-test-client`): they pin the load-bearing kind-gate
//! classification the relay's `req.rs` read gates dispatch on, the intent
//! server-authoring transform, and the `buzz-auth` scope algebra. These are the
//! merge-gate for "the read log did not open" without needing a live stack.
//!
//! The remaining tests need infrastructure and are `#[ignore]`d with a reason:
//!
//! - `*_verify_*` tests provision an `api_tokens` row via `buzz-db` and drive
//!   [`buzz_auth::AuthService::verify_api_key`] directly. Run with a migrated
//!   Postgres: `cargo test -p buzz-test-client --test e2e_apikey -- --ignored`.
//! - `*_relay_*` tests drive the real bearer WebSocket doorway and need a relay
//!   started with `BUZZ_AUTH_MODE=apikey`. Provision a key (e.g. via
//!   `buzz-admin issue-key`), export `BUZZ_API_KEY` + `BUZZ_ACTOR_HEX`, then run
//!   `--ignored`.

use buzz_core::kind::{
    AUTHOR_ONLY_KINDS, KIND_AGENT_OBSERVER_FRAME, KIND_AGENT_TURN_METRIC, KIND_DM_VISIBILITY,
    KIND_EVENT_REMINDER, KIND_GIFT_WRAP, KIND_MEMBER_ADDED_NOTIFICATION,
    KIND_MEMBER_REMOVED_NOTIFICATION, KIND_PUSH_LEASE, P_GATED_KINDS, RESULT_GATED_KINDS,
};

// ---------------------------------------------------------------------------
// No-infra: read-gate classification (REFACTOR.md Invariant 1).
//
// The relay's read gates (`crates/buzz-relay/src/handlers/req.rs`, mirrored in
// `api/bridge.rs`) dispatch on these three kind sets. If a refactor lane
// accidentally drops a security-critical kind out of its set, a bearer read
// filter matching that kind would stop being closed to a non-recipient — the
// "read log opens" regression (§8 risk #1). These tests fail loudly if that
// happens, independent of any running relay.
// ---------------------------------------------------------------------------

/// The `#p`-bound read gate must cover every kind whose *existence* must not
/// leak to a non-recipient: DMs (gift wrap), DM visibility sidecars, agent turn
/// metrics, and the relay-signed membership notifications.
#[test]
fn p_gate_covers_privacy_critical_kinds() {
    for kind in [
        KIND_GIFT_WRAP,
        KIND_DM_VISIBILITY,
        KIND_AGENT_TURN_METRIC,
        KIND_AGENT_OBSERVER_FRAME,
        KIND_MEMBER_ADDED_NOTIFICATION,
        KIND_MEMBER_REMOVED_NOTIFICATION,
    ] {
        assert!(
            P_GATED_KINDS.contains(&kind),
            "kind {kind} must stay p-gated — a bearer reader without a matching #p must be denied"
        );
    }
    assert!(
        !P_GATED_KINDS.is_empty(),
        "p-gate set must never be emptied"
    );
}

/// Author-only kinds are readable only by their author. A bearer principal that
/// is not the author must be denied — assert the schedule/lease kinds stay in
/// the set the relay's author-only filter enforces.
#[test]
fn author_only_gate_covers_reminders_and_leases() {
    assert!(AUTHOR_ONLY_KINDS.contains(&KIND_EVENT_REMINDER));
    assert!(AUTHOR_ONLY_KINDS.contains(&KIND_PUSH_LEASE));
}

/// Result-gated kinds close the kindless `{ids:[…]}` read path: even a bearer
/// reader who knows an event id must match its `#p` tag. Assert the two
/// existence-sensitive kinds stay in the set that forces the per-event fallback.
#[test]
fn result_gate_covers_dm_visibility_and_turn_metrics() {
    assert!(RESULT_GATED_KINDS.contains(&KIND_DM_VISIBILITY));
    assert!(RESULT_GATED_KINDS.contains(&KIND_AGENT_TURN_METRIC));
}

/// Result-gated kinds are a subset of the p-gate (a result-level gate only
/// matters for a kind whose filter-layer `#p` gate is also on). If this drifts,
/// the kindless-ids path could leak an event the filter layer thought it closed.
#[test]
fn result_gated_kinds_are_also_p_gated() {
    for kind in RESULT_GATED_KINDS {
        assert!(
            P_GATED_KINDS.contains(kind),
            "result-gated kind {kind} must also be p-gated"
        );
    }
}

// ---------------------------------------------------------------------------
// No-infra: scope algebra (the write-membership filter Nostr lacked natively).
//
// In `apikey` mode the connection is granted *exactly* the token's scopes
// (never `Scope::all_known`), and the existing action-level `require_scope`
// gates on the hot paths bite. These tests exercise that gate directly.
// ---------------------------------------------------------------------------

/// A `messages:read`-only key can read messages but must be denied a write —
/// the scope-denial property the whole API-key model rests on.
#[test]
fn read_only_key_cannot_write() {
    use buzz_auth::{parse_scopes, require_scope, AuthError, Scope};

    let scopes = parse_scopes(&["messages:read".to_string()]);
    assert!(
        require_scope(&scopes, Scope::MessagesRead).is_ok(),
        "read scope grants read"
    );
    assert!(
        matches!(
            require_scope(&scopes, Scope::MessagesWrite),
            Err(AuthError::InsufficientScope { .. })
        ),
        "read-only key must be denied a message write"
    );
}

/// A `files:write`-only key (media upload) must be denied a message read — a
/// key is narrowed to its granted actions, not a blanket pass.
#[test]
fn files_write_key_denied_message_read() {
    use buzz_auth::{parse_scopes, require_scope, AuthError, Scope};

    let scopes = parse_scopes(&["files:write".to_string()]);
    assert!(require_scope(&scopes, Scope::FilesWrite).is_ok());
    assert!(matches!(
        require_scope(&scopes, Scope::MessagesRead),
        Err(AuthError::InsufficientScope { .. })
    ));
}

/// The defining API-key property: a scoped key is granted strictly fewer scopes
/// than the old `all_known()` blanket grant. Guards against a regression that
/// re-widens a bearer principal to every scope.
#[test]
fn scoped_key_is_strictly_narrower_than_all_known() {
    use buzz_auth::{parse_scopes, Scope};

    let scoped = parse_scopes(&["messages:read".to_string(), "messages:write".to_string()]);
    let all = Scope::all_known();
    assert!(
        scoped.len() < all.len(),
        "a scoped key is narrower than all_known"
    );
    assert!(
        !scoped.contains(&Scope::AdminUsers),
        "a messages-only key must not carry admin scope"
    );
    // Every scope the key does carry is a real, known scope.
    for s in &scoped {
        assert!(all.contains(s));
    }
}

// ---------------------------------------------------------------------------
// No-infra: the bearer intent-publish envelope (server-authored write path).
// ---------------------------------------------------------------------------

/// A channel-scoped text intent keeps its `h` tag, kind, and content through the
/// server-authoring transform, while its identity fields are replaced by the
/// actor. This is exactly what [`BuzzTestClient::send_text_intent`] relies on.
#[test]
fn channel_intent_preserves_h_tag_and_content_through_authoring() {
    use nostr::{EventBuilder, Keys, Kind, Tag};

    let actor = Keys::generate().public_key();
    let channel = "11111111-1111-1111-1111-111111111111";
    let throwaway = Keys::generate();
    let mut intent = EventBuilder::new(Kind::Custom(9), "hello channel")
        .tags([Tag::parse(["h", channel]).unwrap()])
        .sign_with_keys(&throwaway)
        .unwrap();

    buzz_core::authoring::author_event_server_side(&mut intent, actor).unwrap();

    assert_eq!(intent.pubkey, actor, "actor is stamped as author");
    assert_eq!(intent.content, "hello channel", "content survives");
    assert_eq!(intent.kind, Kind::Custom(9), "kind survives");
    let has_h = intent.tags.iter().any(|t| {
        t.as_slice().first().map(String::as_str) == Some("h")
            && t.as_slice().get(1).map(String::as_str) == Some(channel)
    });
    assert!(has_h, "the channel h-tag survives authoring");
}

// ---------------------------------------------------------------------------
// Infra (Postgres): token lifecycle at the `verify_api_key` doorway.
//
// These provision an `api_tokens` row via `buzz-db` and resolve it through the
// same `AuthService::verify_api_key` the WS/HTTP bearer doorways call, asserting
// the revocation and multi-tenant-isolation properties end to end at the auth
// layer. Ignored: requires a migrated Postgres.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod verify_infra {
    use buzz_auth::{AuthConfig, AuthError, AuthMode, AuthService, Scope};
    use buzz_core::CommunityId;
    use buzz_db::{CreateCommunityWithOwnerResult, Db};
    use sha2::{Digest, Sha256};
    use sqlx::PgPool;
    use uuid::Uuid;

    const TEST_DB_URL: &str = "postgres://buzz:buzz_dev@localhost:5432/buzz";

    fn apikey_service() -> AuthService {
        AuthService::new(AuthConfig {
            auth_mode: AuthMode::ApiKey,
            ..AuthConfig::default()
        })
    }

    async fn setup() -> (AuthService, Db) {
        let pool = PgPool::connect(TEST_DB_URL)
            .await
            .expect("connect to test DB");
        (apikey_service(), Db::from_pool(pool))
    }

    async fn community_with_owner(db: &Db) -> (CommunityId, [u8; 32]) {
        let owner = nostr::Keys::generate().public_key();
        let owner_bytes: [u8; 32] = owner.to_bytes();
        let host = format!("apikey-l-{}.example", Uuid::new_v4().simple());
        let cid = match db
            .create_community_with_owner(&host, &owner.to_hex())
            .await
            .expect("create community")
        {
            CreateCommunityWithOwnerResult::Created(rec) => rec.id,
            other => panic!("unexpected create result: {other:?}"),
        };
        db.ensure_user(cid, &owner_bytes)
            .await
            .expect("ensure user");
        (cid, owner_bytes)
    }

    fn hash_of(token: &str) -> [u8; 32] {
        Sha256::digest(token.as_bytes()).into()
    }

    /// A scoped bearer key resolves to *exactly* its stored scopes — never the
    /// old `all_known` blanket grant.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn bearer_key_grants_exactly_stored_scopes() {
        let (svc, db) = setup().await;
        let (cid, owner) = community_with_owner(&db).await;
        let token = "lane-l-scoped-token";
        db.create_api_token(
            cid,
            &hash_of(token),
            &owner,
            "lane-l",
            &["messages:read".into(), "messages:write".into()],
            None,
            None,
        )
        .await
        .expect("create token");

        let ctx = svc.verify_api_key(token, cid, &db).await.expect("resolve");
        assert!(ctx.has_scope(&Scope::MessagesRead));
        assert!(ctx.has_scope(&Scope::MessagesWrite));
        assert!(!ctx.has_scope(&Scope::AdminUsers), "no blanket admin grant");
    }

    /// A revoked key is rejected by the bearer doorway.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn revoked_bearer_key_is_rejected() {
        let (svc, db) = setup().await;
        let (cid, owner) = community_with_owner(&db).await;
        let token = "lane-l-revoked-token";
        let id = db
            .create_api_token(
                cid,
                &hash_of(token),
                &owner,
                "t",
                &["messages:read".into()],
                None,
                None,
            )
            .await
            .expect("create token");
        assert!(db
            .revoke_token(cid, id, &owner, &owner)
            .await
            .expect("revoke"));

        assert!(matches!(
            svc.verify_api_key(token, cid, &db).await,
            Err(AuthError::InvalidToken)
        ));
    }

    /// Multi-tenant isolation: a key minted in community A does not resolve when
    /// presented against community B, even with an identical token string.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn key_from_community_a_cannot_act_in_b() {
        let (svc, db) = setup().await;
        let (cid_a, owner_a) = community_with_owner(&db).await;
        let (cid_b, _owner_b) = community_with_owner(&db).await;
        let token = "lane-l-cross-tenant";
        db.create_api_token(
            cid_a,
            &hash_of(token),
            &owner_a,
            "t",
            &["messages:read".into()],
            None,
            None,
        )
        .await
        .expect("create token in A");

        assert!(matches!(
            svc.verify_api_key(token, cid_b, &db).await,
            Err(AuthError::InvalidToken)
        ));
    }
}

// ---------------------------------------------------------------------------
// Infra (relay in apikey mode): the real bearer WebSocket doorway.
//
// These drive `connect_bearer` + `publish_intent` against a running relay
// started with `BUZZ_AUTH_MODE=apikey`. They validate that a provisioned key
// authenticates at connect (no NIP-42 challenge) and that the server authors the
// published intent. Ignored: requires a relay + a provisioned key.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod relay_infra {
    use buzz_test_client::BuzzTestClient;
    use nostr::PublicKey;
    use std::time::Duration;

    fn relay_url() -> String {
        std::env::var("RELAY_URL").unwrap_or_else(|_| "ws://localhost:3000".to_string())
    }

    /// The bearer key to present, and the 32-byte actor hex it resolves to.
    /// Provision both with `buzz-admin issue-key` before running.
    fn provisioned() -> (String, PublicKey) {
        let token = std::env::var("BUZZ_API_KEY").expect("set BUZZ_API_KEY to a provisioned key");
        let actor_hex =
            std::env::var("BUZZ_ACTOR_HEX").expect("set BUZZ_ACTOR_HEX to the key's actor id");
        let actor = PublicKey::from_hex(&actor_hex).expect("valid actor hex");
        (token, actor)
    }

    /// A provisioned bearer key connects with no challenge and its unsigned
    /// text intent is accepted (server-authored).
    #[tokio::test]
    #[ignore = "requires relay started with BUZZ_AUTH_MODE=apikey and a provisioned key"]
    async fn bearer_connect_and_publish_intent_roundtrip() {
        let (token, actor) = provisioned();
        let channel = std::env::var("BUZZ_CHANNEL")
            .expect("set BUZZ_CHANNEL to a channel uuid the key can write");

        let mut client = BuzzTestClient::connect_bearer(&relay_url(), &token)
            .await
            .expect("bearer connect should authenticate at upgrade");

        let ok = client
            .send_text_intent(actor, &channel, "hello from a bearer intent", 9)
            .await
            .expect("intent publish");
        assert!(
            ok.accepted,
            "server should author + accept the intent: {}",
            ok.message
        );

        let _ = client.disconnect().await;
    }

    /// An invalid bearer token must be rejected at the WS upgrade — the socket
    /// closes, so no client is returned.
    #[tokio::test]
    #[ignore = "requires relay started with BUZZ_AUTH_MODE=apikey"]
    async fn invalid_bearer_token_is_rejected_at_connect() {
        // A syntactically fine but unprovisioned token.
        let result =
            BuzzTestClient::connect_bearer(&relay_url(), "definitely-not-a-real-token").await;
        // Either the upgrade fails, or the relay closes immediately after; give
        // the connection a beat and assert we cannot use it.
        match result {
            Err(_) => {} // upgrade rejected — expected
            Ok(mut client) => {
                // Closed right after upgrade: the next read must error/close.
                let r = client.recv_event(Duration::from_secs(2)).await;
                assert!(
                    r.is_err(),
                    "an unprovisioned bearer connection must not be usable"
                );
            }
        }
    }
}
