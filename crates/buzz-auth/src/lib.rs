#![deny(unsafe_code)]
#![warn(missing_docs)]
//! `buzz-auth` — Authentication and authorization for the Buzz relay.
//!
//! ## Auth paths
//!
//! | Path | Transport | Mode | Description |
//! |------|-----------|------|-------------|
//! | NIP-42 | WebSocket | `nostr` | Challenge/response; client signs kind:22242 event |
//! | NIP-98 | HTTP | `nostr` | Signed kind:27235 event in `Authorization: Nostr` header |
//! | API key | WS + HTTP | `apikey` | `Authorization: Bearer <token>` resolved via [`AuthService::verify_api_key`] |
//!
//! The active doorway is selected by [`AuthMode`] (`BUZZ_AUTH_MODE`, default
//! `nostr`). In `apikey` mode the connection is granted exactly the token's
//! stored [`Scope`]s (never [`Scope::all_known`]) and those scopes are enforced
//! by the existing action-level gates on the read/write hot paths.
//!
//! ## Security invariants
//!
//! - **AUTH events (kind:22242) are NEVER stored or logged.**
//! - All paths produce an [`AuthContext`] bound to the connection.
//! - No JWT validation, no token management, no IdP runtime dependency.

/// Channel access checking trait and helpers.
pub mod access;
/// Authentication error types.
pub mod error;
/// NIP-42 challenge–response authentication.
pub mod nip42;
/// NIP-98 HTTP Auth verification (kind:27235).
pub mod nip98;
/// NIP-98 replay protection — shared, community-scoped, atomic seen-set.
pub mod nip98_replay;
/// Per-connection rate limiting.
pub mod rate_limit;
/// OAuth scope parsing and enforcement.
pub mod scope;

pub use access::{check_read_access, check_write_access, require_scope, ChannelAccessChecker};
pub use error::AuthError;
pub use nip42::{generate_challenge, verify_nip42_event};
pub use nip98::verify_nip98_event;
pub use nip98_replay::{
    nip98_replay_key, nip98_replay_key_for_scope, Nip98ReplayGuard, DEFAULT_REPLAY_TTL_SECS,
    MAX_REPLAY_TTL_SECS,
};
pub use rate_limit::{
    ip_rate_limit_key, rate_limit_key, LimitType, RateLimitConfig, RateLimitResult, RateLimiter,
};
pub use scope::{parse_scopes, Scope};

#[cfg(any(test, feature = "test-utils"))]
pub use access::MockAccessChecker;
#[cfg(any(test, feature = "test-utils"))]
pub use nip98_replay::AlwaysFreshReplayGuard;
#[cfg(any(test, feature = "test-utils"))]
pub use rate_limit::AlwaysAllowRateLimiter;

/// How the connection was authenticated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMethod {
    /// NIP-42 challenge/response — Schnorr signature over kind:22242.
    Nip42,
    /// NIP-98 HTTP Auth — Schnorr signature over kind:27235.
    Nip98,
    /// API-key bearer token resolved against the `api_tokens` store.
    ApiKey,
}

/// Which authentication doorway the relay presents.
///
/// Selected by the `BUZZ_AUTH_MODE` config/env flag. Defaults to [`AuthMode::Nostr`]
/// so existing deployments keep the NIP-42 (WebSocket) / NIP-98 (HTTP) crypto
/// doorways unchanged; `apikey` swaps both for bearer-token validation against
/// the `api_tokens` store and activates per-key scope enforcement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    /// NIP-42 / NIP-98 Nostr signature auth. Every authenticated connection is
    /// granted [`Scope::all_known`]; per-channel access is enforced by NIP-29
    /// membership checks. This is the pre-refactor behaviour.
    #[default]
    Nostr,
    /// API-key bearer auth. The presented token is hashed and looked up in the
    /// community-scoped `api_tokens` store; the connection is granted exactly the
    /// token's stored scopes and optional channel restriction.
    ApiKey,
}

impl AuthMode {
    /// Parse an `AuthMode` from its wire/env string (`"nostr"` | `"apikey"`).
    ///
    /// Matching is case-insensitive. Unknown values return `None` so callers can
    /// fail loudly on a misconfigured `BUZZ_AUTH_MODE` rather than silently
    /// defaulting to an unintended doorway.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "nostr" => Some(Self::Nostr),
            "apikey" | "api_key" | "api-key" => Some(Self::ApiKey),
            _ => None,
        }
    }

    /// Returns `true` if this is [`AuthMode::ApiKey`].
    pub fn is_apikey(self) -> bool {
        matches!(self, Self::ApiKey)
    }
}

/// The result of a successful authentication, bound to a connection.
#[derive(Debug, Clone)]
pub struct AuthContext {
    /// The authenticated Nostr public key.
    pub pubkey: nostr::PublicKey,
    /// Permission scopes granted to this connection.
    pub scopes: Vec<Scope>,
    /// Channel restriction (reserved for future per-channel access control).
    ///
    /// `None` means unrestricted.
    pub channel_ids: Option<Vec<uuid::Uuid>>,
    /// How the connection was authenticated.
    pub auth_method: AuthMethod,
    /// NIP-OA verified owner pubkey (if authenticated via owner attestation).
    ///
    /// `None` for direct relay members or non-NIP-OA auth paths.
    /// Set by the relay membership gate when NIP-OA fallback succeeds.
    pub agent_owner_pubkey: Option<nostr::PublicKey>,
}

impl AuthContext {
    /// Returns `true` if this context includes the given [`Scope`].
    pub fn has_scope(&self, scope: &Scope) -> bool {
        self.scopes.contains(scope)
    }
}

/// Top-level authentication configuration, typically loaded from the relay's TOML config file.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AuthConfig {
    /// Per-user and per-IP rate limit thresholds.
    #[serde(default)]
    pub rate_limits: RateLimitConfig,
    /// Which authentication doorway the relay presents (`nostr` | `apikey`).
    ///
    /// Defaults to [`AuthMode::Nostr`] — existing Nostr signature auth is
    /// unchanged unless this is explicitly set to `apikey`.
    #[serde(default)]
    pub auth_mode: AuthMode,
}

/// Simplified auth service — NIP-42 and NIP-98 only.
/// No JWT validation, no token management, no IdP runtime dependency.
#[derive(Debug, Clone)]
pub struct AuthService {
    config: AuthConfig,
}

impl AuthService {
    /// Create a new `AuthService` with the given configuration.
    pub fn new(config: AuthConfig) -> Self {
        Self { config }
    }

    /// Return a reference to the auth configuration.
    pub fn config(&self) -> &AuthConfig {
        &self.config
    }

    /// The active authentication mode (`nostr` | `apikey`).
    pub fn auth_mode(&self) -> AuthMode {
        self.config.auth_mode
    }

    /// Verify a NIP-42 AUTH event and return an [`AuthContext`].
    ///
    /// Pure cryptographic verification — no network calls, no JWT, no tokens.
    ///
    /// Only valid under [`AuthMode::Nostr`]. In `apikey` mode the NIP-42 doorway
    /// is closed and this returns [`AuthError::AuthModeMismatch`], failing closed
    /// so a signature can never mint the full [`Scope::all_known`] set once the
    /// relay has switched to bearer auth.
    pub async fn verify_auth_event(
        &self,
        auth_event: nostr::Event,
        expected_challenge: &str,
        relay_url: &str,
    ) -> Result<AuthContext, AuthError> {
        // Gate cleanly: the NIP-42 all-scopes grant below is only correct in
        // `nostr` mode. In `apikey` mode the caller must use `verify_api_key`.
        if self.config.auth_mode.is_apikey() {
            return Err(AuthError::AuthModeMismatch);
        }

        // Verify NIP-42 signature (spawn_blocking for CPU-bound Schnorr verify)
        let event_clone = auth_event.clone();
        let challenge_owned = expected_challenge.to_string();
        let relay_owned = relay_url.to_string();
        tokio::task::spawn_blocking(move || {
            verify_nip42_event(&event_clone, &challenge_owned, &relay_owned)
        })
        .await
        .map_err(|_| AuthError::Internal("spawn_blocking panicked".into()))??;

        // In pure Nostr mode, all authenticated connections get full scopes.
        // Per-channel access is enforced by the relay's membership checks (NIP-29).
        Ok(AuthContext {
            pubkey: auth_event.pubkey,
            scopes: Scope::all_known(),
            channel_ids: None,
            auth_method: AuthMethod::Nip42,
            agent_owner_pubkey: None, // Set later by relay membership gate if NIP-OA
        })
    }

    /// Resolve an API-key bearer token into an [`AuthContext`] (apikey mode).
    ///
    /// The presented `token` is SHA-256 hashed and looked up in the
    /// community-scoped `api_tokens` store via [`buzz_db::Db::get_api_token_by_hash`]
    /// (which filters revoked tokens and scopes the lookup on
    /// `(community_id, token_hash)` — the row-44 tenancy fence). Expiry is checked
    /// here because the active-lookup query does not filter `expires_at`.
    ///
    /// On success the returned context carries:
    /// - `pubkey`: the token's `owner_pubkey` reinterpreted as the opaque 32-byte
    ///   actor id (the field name is retained per refactor decision #2),
    /// - `scopes`: the token's stored scopes (via [`parse_scopes`]) — the connection
    ///   is granted **exactly** these, never [`Scope::all_known`],
    /// - `channel_ids`: the token's optional per-key channel restriction,
    /// - `auth_method`: [`AuthMethod::ApiKey`].
    ///
    /// `last_used_at` is updated best-effort; a failure to record usage never
    /// fails the authentication.
    ///
    /// # Errors
    ///
    /// - [`AuthError::AuthModeMismatch`] if the relay is not in `apikey` mode.
    /// - [`AuthError::InvalidToken`] if the token is unknown, revoked, or scoped
    ///   to a different community.
    /// - [`AuthError::TokenExpired`] if the token has passed its `expires_at`.
    /// - [`AuthError::Internal`] on a database error or a malformed stored actor id.
    pub async fn verify_api_key(
        &self,
        token: &str,
        community: buzz_core::CommunityId,
        db: &buzz_db::Db,
    ) -> Result<AuthContext, AuthError> {
        use sha2::{Digest, Sha256};

        if !self.config.auth_mode.is_apikey() {
            return Err(AuthError::AuthModeMismatch);
        }

        let hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();

        let record = db
            .get_api_token_by_hash(community, &hash)
            .await
            .map_err(|e| AuthError::Internal(format!("api token lookup failed: {e}")))?
            .ok_or(AuthError::InvalidToken)?;

        // The active lookup filters `revoked_at IS NULL` but NOT expiry — enforce
        // it here so an expired-but-unrevoked token is rejected.
        if let Some(expires_at) = record.expires_at {
            if expires_at <= chrono::Utc::now() {
                return Err(AuthError::TokenExpired);
            }
        }

        // Reinterpret the stored `owner_pubkey` bytes as the opaque actor id,
        // carried in the retained `pubkey` field (decision #2).
        let pubkey = nostr::PublicKey::from_slice(&record.owner_pubkey)
            .map_err(|e| AuthError::Internal(format!("stored actor id malformed: {e}")))?;

        let scopes = parse_scopes(&record.scopes);

        // Best-effort usage timestamp; never fail auth on a bookkeeping error.
        if let Err(e) = db.update_token_last_used(community, &hash).await {
            tracing::debug!(error = %e, "failed to update api token last_used_at (best-effort)");
        }

        Ok(AuthContext {
            pubkey,
            scopes,
            channel_ids: record.channel_ids,
            auth_method: AuthMethod::ApiKey,
            agent_owner_pubkey: None,
        })
    }
}

/// Derive a deterministic Nostr pubkey from a username string.
///
/// Uses `SHA-256("buzz-test-key:{username}")` as the secret key material.
/// This matches the derivation used by the desktop's `set_test_identity` function,
/// allowing the relay to resolve usernames to Nostr pubkeys in dev mode.
///
/// # ⚠️ SECURITY — Dev/test only
///
/// This function is gated behind `#[cfg(any(test, feature = "dev"))]`
/// and **must never be compiled into a production release build**.
///
/// - The derived keys are deterministic and predictable from the username alone.
/// - Any attacker who knows a username can compute the corresponding private key.
#[cfg(any(test, feature = "dev"))]
pub fn derive_pubkey_from_username(username: &str) -> Result<nostr::PublicKey, AuthError> {
    use sha2::{Digest, Sha256};
    let seed = format!("buzz-test-key:{username}");
    let hash: [u8; 32] = Sha256::digest(seed.as_bytes()).into();
    let secret_key = nostr::SecretKey::from_slice(&hash)
        .map_err(|e| AuthError::Internal(format!("key derivation failed: {e}")))?;
    Ok(nostr::Keys::new(secret_key).public_key())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::{EventBuilder, Keys, Kind, RelayUrl};

    fn make_auth_event(keys: &Keys, challenge: &str, relay_url: &str) -> nostr::Event {
        let url = RelayUrl::parse(relay_url).expect("valid url");
        EventBuilder::auth(challenge, url)
            .sign_with_keys(keys)
            .expect("signing failed")
    }

    fn test_service() -> AuthService {
        AuthService::new(AuthConfig::default())
    }

    #[test]
    fn auth_context_scope_check() {
        let keys = Keys::generate();
        let ctx = AuthContext {
            pubkey: keys.public_key(),
            scopes: vec![Scope::MessagesRead, Scope::ChannelsRead],
            channel_ids: None,
            auth_method: AuthMethod::Nip42,
            agent_owner_pubkey: None,
        };
        assert!(ctx.has_scope(&Scope::MessagesRead));
        assert!(!ctx.has_scope(&Scope::MessagesWrite));
    }

    #[tokio::test]
    async fn nip42_auth_succeeds() {
        let keys = Keys::generate();
        let challenge = generate_challenge();
        let relay = "wss://relay.example.com";
        let event = make_auth_event(&keys, &challenge, relay);

        let ctx = test_service()
            .verify_auth_event(event, &challenge, relay)
            .await
            .expect("NIP-42 auth should succeed");

        assert_eq!(ctx.pubkey, keys.public_key());
        assert_eq!(ctx.auth_method, AuthMethod::Nip42);
        assert!(ctx.has_scope(&Scope::MessagesRead));
        assert!(ctx.has_scope(&Scope::MessagesWrite));
    }

    #[tokio::test]
    async fn wrong_challenge_rejected() {
        let keys = Keys::generate();
        let challenge = generate_challenge();
        let relay = "wss://relay.example.com";
        let event = make_auth_event(&keys, &challenge, relay);

        let result = test_service()
            .verify_auth_event(event, "wrong-challenge", relay)
            .await;
        assert!(matches!(result, Err(AuthError::ChallengeMismatch)));
    }

    #[tokio::test]
    async fn wrong_kind_rejected() {
        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::TextNote, "not auth")
            .tags([])
            .sign_with_keys(&keys)
            .expect("sign");

        let result = test_service()
            .verify_auth_event(event, &generate_challenge(), "wss://relay.example.com")
            .await;
        assert!(matches!(result, Err(AuthError::InvalidSignature)));
    }

    fn apikey_service() -> AuthService {
        AuthService::new(AuthConfig {
            auth_mode: AuthMode::ApiKey,
            ..AuthConfig::default()
        })
    }

    #[test]
    fn auth_mode_parses_case_insensitively() {
        assert_eq!(AuthMode::parse("nostr"), Some(AuthMode::Nostr));
        assert_eq!(AuthMode::parse("APIKEY"), Some(AuthMode::ApiKey));
        assert_eq!(AuthMode::parse("api_key"), Some(AuthMode::ApiKey));
        assert_eq!(AuthMode::parse("  ApiKey "), Some(AuthMode::ApiKey));
        assert_eq!(AuthMode::parse("bogus"), None);
        assert_eq!(AuthMode::default(), AuthMode::Nostr);
    }

    /// In `apikey` mode the NIP-42 doorway is closed: `verify_auth_event` must
    /// fail closed rather than mint the full `all_known` scope set. This is the
    /// gate that keeps a signature from bypassing scope enforcement after cutover.
    #[tokio::test]
    async fn nip42_rejected_in_apikey_mode() {
        let keys = Keys::generate();
        let challenge = generate_challenge();
        let relay = "wss://relay.example.com";
        let event = make_auth_event(&keys, &challenge, relay);

        let result = apikey_service()
            .verify_auth_event(event, &challenge, relay)
            .await;
        assert!(matches!(result, Err(AuthError::AuthModeMismatch)));
    }

    /// A key granting only `files:write` must be denied a message read — the
    /// action-level scope gate (`require_scope`) is a no-op under `all_known`
    /// (nostr mode) but bites once real per-key scopes are granted (apikey mode).
    #[test]
    fn apikey_scope_denial_for_over_broad_read() {
        // Simulate the scopes an apikey `AuthContext` would carry.
        let ctx = AuthContext {
            pubkey: Keys::generate().public_key(),
            scopes: parse_scopes(&["files:write"]),
            channel_ids: None,
            auth_method: AuthMethod::ApiKey,
            agent_owner_pubkey: None,
        };
        // The message-read chokepoints require MessagesRead.
        assert!(matches!(
            require_scope(&ctx.scopes, Scope::MessagesRead),
            Err(AuthError::InsufficientScope { .. })
        ));
        // A key that does carry messages:read passes the same gate.
        let ok = parse_scopes(&["messages:read"]);
        assert!(require_scope(&ok, Scope::MessagesRead).is_ok());
    }
}

/// Postgres-backed `verify_api_key` tests. Ignored by default — they require a
/// running Postgres (same `TEST_DB_URL` the `buzz-db` suite uses) and the schema
/// migrated. Run with `cargo test -p buzz-auth -- --ignored`.
#[cfg(test)]
mod verify_api_key_tests {
    use super::*;
    use buzz_core::CommunityId;
    use buzz_db::{CreateCommunityWithOwnerResult, Db};
    use sha2::{Digest, Sha256};
    use sqlx::PgPool;
    use uuid::Uuid;

    const TEST_DB_URL: &str = "postgres://buzz:buzz_dev@localhost:5432/buzz";

    async fn setup() -> (AuthService, Db) {
        let pool = PgPool::connect(TEST_DB_URL)
            .await
            .expect("connect to test DB");
        (apikey_service(), Db::from_pool(pool))
    }

    fn apikey_service() -> AuthService {
        AuthService::new(AuthConfig {
            auth_mode: AuthMode::ApiKey,
            ..AuthConfig::default()
        })
    }

    /// Create a fresh community + owner user; return the community id and the
    /// owner's 32-byte actor id.
    async fn make_community_with_owner(db: &Db) -> (CommunityId, [u8; 32]) {
        let owner = nostr::Keys::generate().public_key();
        let owner_bytes: [u8; 32] = owner.to_bytes();
        let host = format!("apikey-{}.example", Uuid::new_v4().simple());
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

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn valid_token_resolves_to_stored_scopes() {
        let (svc, db) = setup().await;
        let (cid, owner) = make_community_with_owner(&db).await;
        let token = "valid-secret-token";
        db.create_api_token(
            cid,
            &hash_of(token),
            &owner,
            "test",
            &["messages:read".into(), "messages:write".into()],
            None,
            None,
        )
        .await
        .expect("create token");

        let ctx = svc
            .verify_api_key(token, cid, &db)
            .await
            .expect("valid token should resolve");
        assert_eq!(ctx.pubkey.to_bytes(), owner);
        assert_eq!(ctx.auth_method, AuthMethod::ApiKey);
        assert!(ctx.has_scope(&Scope::MessagesRead));
        assert!(ctx.has_scope(&Scope::MessagesWrite));
        // Real scopes, NOT all_known.
        assert!(!ctx.has_scope(&Scope::AdminUsers));
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn revoked_token_is_rejected() {
        let (svc, db) = setup().await;
        let (cid, owner) = make_community_with_owner(&db).await;
        let token = "revoked-token";
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

        let result = svc.verify_api_key(token, cid, &db).await;
        assert!(matches!(result, Err(AuthError::InvalidToken)));
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn expired_token_is_rejected() {
        let (svc, db) = setup().await;
        let (cid, owner) = make_community_with_owner(&db).await;
        let token = "expired-token";
        let past = chrono::Utc::now() - chrono::Duration::hours(1);
        db.create_api_token(
            cid,
            &hash_of(token),
            &owner,
            "t",
            &["messages:read".into()],
            None,
            Some(past),
        )
        .await
        .expect("create token");

        let result = svc.verify_api_key(token, cid, &db).await;
        assert!(matches!(result, Err(AuthError::TokenExpired)));
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn token_from_other_community_is_rejected() {
        let (svc, db) = setup().await;
        let (cid_a, owner_a) = make_community_with_owner(&db).await;
        let (cid_b, _owner_b) = make_community_with_owner(&db).await;
        let token = "cross-tenant-token";
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

        // Same token hash, looked up under community B, must not resolve.
        let result = svc.verify_api_key(token, cid_b, &db).await;
        assert!(matches!(result, Err(AuthError::InvalidToken)));
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn nostr_mode_refuses_verify_api_key() {
        let (_svc, db) = setup().await;
        let (cid, _owner) = make_community_with_owner(&db).await;
        let nostr_svc = AuthService::new(AuthConfig::default());
        let result = nostr_svc.verify_api_key("anything", cid, &db).await;
        assert!(matches!(result, Err(AuthError::AuthModeMismatch)));
    }
}
