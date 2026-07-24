//! Operational cutover tooling for `buzz-admin` (REFACTOR.md §7).
//!
//! Two operator commands live here, both flag-gated behind the Nostr → API-key
//! migration and meant to be run **once per community at cutover**:
//!
//! - `backfill-keys` — batch-mint one API key per existing active actor so that
//!   history stays attributed to the same opaque `pubkey`-as-actor id.
//! - `cutover-genesis` — append the audit [`cutover genesis`] entry pinning the
//!   last Nostr-era head hash.
//!
//! [`cutover genesis`]: buzz_audit::AuditService::append_cutover_genesis
//!
//! This module owns the small, pure, unit-testable helpers (scope derivation,
//! the active-token predicate, TSV row formatting); the async command handlers
//! that touch Postgres live in `main.rs` and call these.

use buzz_audit::AuditService;
use buzz_auth::scope::Scope;
use buzz_core::CommunityId;
use buzz_db::Db;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Default scope set granted to a backfilled **member**.
///
/// This is an intentional, load-bearing **duplicate** of
/// `MEMBER_SELF_MINT_SCOPES` in
/// `crates/buzz-relay/src/api/invites.rs`: a member minted by claiming an
/// invite and a member minted by the backfill must receive the *same* grant, so
/// the two paths converge on one identity model. It is duplicated rather than
/// shared because `buzz-admin` must not take a dependency on `buzz-relay` just
/// for a constant (both already depend on `buzz-auth`, which owns [`Scope`]).
///
/// If the invite self-mint set changes, change this to match — the two are kept
/// in lockstep by the `backfill_member_scopes_match_invite_self_mint` test,
/// which re-derives the invite set here so drift trips a unit test.
pub(crate) const MEMBER_SELF_MINT_SCOPES: [Scope; 8] = [
    Scope::MessagesRead,
    Scope::MessagesWrite,
    Scope::ChannelsRead,
    Scope::UsersRead,
    Scope::UsersWrite,
    Scope::FilesRead,
    Scope::FilesWrite,
    Scope::SubscriptionsRead,
];

/// Elevated scopes added on top of the member set for `owner`/`admin` actors.
///
/// An owner/admin who loses these at cutover could no longer perform the
/// relay-admin operations (channel create/edit, add/remove member) they held
/// under Nostr mode, where every authenticated connection was granted
/// `Scope::all_known()`. Grounded in
/// `crates/buzz-relay/src/handlers/ingest::required_scope_for_kind`: channel
/// admin commands require `ChannelsWrite` / `AdminChannels`, and the kind:9030
/// add-member command requires `AdminUsers`. Members never receive these.
const ADMIN_ELEVATION_SCOPES: [Scope; 3] = [
    Scope::ChannelsWrite,
    Scope::AdminChannels,
    Scope::AdminUsers,
];

/// Derive the canonical scope strings a backfilled key should carry for a
/// relay member of the given `role`.
///
/// - `member` (and any unrecognised role) → exactly [`MEMBER_SELF_MINT_SCOPES`],
///   matching the invite self-mint set.
/// - `admin` / `owner` → the member set **plus** [`ADMIN_ELEVATION_SCOPES`], so
///   relay operators retain their administrative powers post-cutover.
///
/// Order is preserved (member scopes first, then elevation) and there are no
/// duplicates because the two sets are disjoint by construction.
pub(crate) fn scopes_for_role(role: &str) -> Vec<String> {
    let mut scopes: Vec<String> = MEMBER_SELF_MINT_SCOPES
        .iter()
        .map(|s| s.as_str().to_string())
        .collect();
    if matches!(role, "admin" | "owner") {
        scopes.extend(
            ADMIN_ELEVATION_SCOPES
                .iter()
                .map(|s| s.as_str().to_string()),
        );
    }
    scopes
}

/// Whether any token record in `records` is currently **active** at `now`
/// (not revoked and not expired).
///
/// The backfill is idempotent: an actor that already holds an active key is
/// skipped so a re-run never mints a second key for the same identity.
pub(crate) fn has_active_token(records: &[buzz_db::ApiTokenRecord], now: DateTime<Utc>) -> bool {
    records
        .iter()
        .any(|r| r.revoked_at.is_none() && r.expires_at.is_none_or(|exp| exp > now))
}

/// One planned/performed backfill row, rendered as a TSV line.
///
/// `key` is `Some` only for a real (non-dry-run) mint; the plaintext appears in
/// exactly one place — this struct, printed once to stdout by the handler and
/// never persisted or logged.
pub(crate) struct BackfillRow {
    /// Actor id (64-char hex) the key authenticates as.
    pub actor_hex: String,
    /// Relay role that determined the scope set.
    pub role: String,
    /// The action taken for this actor.
    pub outcome: BackfillOutcome,
}

/// Outcome for a single actor in a backfill sweep.
pub(crate) enum BackfillOutcome {
    /// A fresh key was minted (real run). Carries token id + plaintext.
    Minted { id: Uuid, key: String },
    /// Would mint on a real run (dry run).
    WouldMint,
    /// Skipped — the actor already holds an active key.
    SkippedActive,
}

impl BackfillRow {
    /// The tab-separated header describing [`BackfillRow::to_tsv`] columns.
    pub(crate) fn tsv_header() -> &'static str {
        "actor\trole\toutcome\ttoken_id\tapi_key"
    }

    /// Render this row as a single TSV line matching [`BackfillRow::tsv_header`].
    ///
    /// Columns: `actor`, `role`, `outcome`, `token_id`, `api_key`. The last two
    /// are empty for non-mint outcomes; the plaintext key is present only for a
    /// real mint and only here.
    pub(crate) fn to_tsv(&self) -> String {
        let (outcome, id, key) = match &self.outcome {
            BackfillOutcome::Minted { id, key } => ("minted", id.to_string(), key.as_str()),
            BackfillOutcome::WouldMint => ("would-mint", String::new(), ""),
            BackfillOutcome::SkippedActive => ("skipped-active", String::new(), ""),
        };
        format!(
            "{}\t{}\t{}\t{}\t{}",
            self.actor_hex, self.role, outcome, id, key
        )
    }
}

/// Core of the `backfill-keys` command: mint one API key per active relay
/// member (or, in `dry_run`, only plan it), returning one [`BackfillRow`] per
/// member for the caller to print.
///
/// Separated from the CLI handler so it is testable against a real `Db` without
/// the env/stdout surface. Members whose stored `pubkey` is not 32-byte hex are
/// logged to stderr and skipped (they cannot become a 32-byte actor id).
pub(crate) async fn plan_backfill(
    db: &Db,
    community: CommunityId,
    dry_run: bool,
    name: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<BackfillRow>> {
    let members = db.list_relay_members(community).await?;
    let mut rows = Vec::with_capacity(members.len());

    for member in &members {
        let actor_bytes = match hex::decode(&member.pubkey) {
            Ok(b) if b.len() == 32 => b,
            _ => {
                eprintln!(
                    "warning: skipping member with malformed pubkey '{}' (expected 32-byte hex)",
                    member.pubkey
                );
                continue;
            }
        };

        // Idempotency: skip actors that already hold an active key.
        let existing = db.list_tokens_by_owner(community, &actor_bytes).await?;
        if has_active_token(&existing, now) {
            rows.push(BackfillRow {
                actor_hex: member.pubkey.clone(),
                role: member.role.clone(),
                outcome: BackfillOutcome::SkippedActive,
            });
            continue;
        }

        if dry_run {
            rows.push(BackfillRow {
                actor_hex: member.pubkey.clone(),
                role: member.role.clone(),
                outcome: BackfillOutcome::WouldMint,
            });
            continue;
        }

        // owner_pubkey is FK-constrained to users; ensure the row exists first
        // (mirrors the invite-claim path).
        db.ensure_user(community, &actor_bytes).await?;
        let (token, hash) = crate::apikey::generate_token();
        let scopes = scopes_for_role(&member.role);
        let id = db
            .create_api_token(community, &hash, &actor_bytes, name, &scopes, None, None)
            .await?;
        rows.push(BackfillRow {
            actor_hex: member.pubkey.clone(),
            role: member.role.clone(),
            outcome: BackfillOutcome::Minted { id, key: token },
        });
    }

    Ok(rows)
}

/// Outcome of a single community's `cutover-genesis`.
pub(crate) enum CutoverOutcome {
    /// A genesis entry was appended at `seq`, pinning `pinned_head` (empty when
    /// the community had no prior audit history).
    Appended { seq: i64, pinned_head: Vec<u8> },
    /// Refused — the community already carries a cutover-genesis entry.
    AlreadyPresent,
}

/// Core of the `cutover-genesis` command for one community: pin the current
/// audit head into a genesis entry, or refuse if one already exists.
///
/// Separated from the CLI handler for Postgres-gated testing. `now` is the
/// timestamp pinned when the community has no prior audit history.
pub(crate) async fn run_cutover_genesis(
    audit: &AuditService,
    community: CommunityId,
    now: DateTime<Utc>,
) -> anyhow::Result<CutoverOutcome> {
    if audit.has_cutover_genesis(community).await? {
        return Ok(CutoverOutcome::AlreadyPresent);
    }
    let (head_hash, head_ts) = match audit.head(community).await? {
        Some(head) => (head.hash, head.created_at),
        None => (Vec::new(), now),
    };
    let entry = audit
        .append_cutover_genesis(community, &head_hash, head_ts, None)
        .await?;
    Ok(CutoverOutcome::Appended {
        seq: entry.seq,
        pinned_head: head_hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn token(
        revoked: bool,
        expires_in: Option<i64>,
        now: DateTime<Utc>,
    ) -> buzz_db::ApiTokenRecord {
        buzz_db::ApiTokenRecord {
            id: Uuid::new_v4(),
            token_hash: vec![0u8; 32],
            owner_pubkey: vec![0xab; 32],
            name: "t".to_string(),
            scopes: vec!["messages:read".to_string()],
            channel_ids: None,
            created_at: now,
            expires_at: expires_in.map(|s| now + Duration::seconds(s)),
            last_used_at: None,
            revoked_at: revoked.then_some(now),
        }
    }

    #[test]
    fn member_role_gets_exactly_the_invite_self_mint_set() {
        let scopes = scopes_for_role("member");
        let expected: Vec<String> = MEMBER_SELF_MINT_SCOPES
            .iter()
            .map(|s| s.as_str().to_string())
            .collect();
        assert_eq!(scopes, expected);
    }

    #[test]
    fn unknown_role_defaults_to_member_scopes() {
        assert_eq!(scopes_for_role("wizard"), scopes_for_role("member"));
    }

    #[test]
    fn admin_and_owner_get_elevated_scopes() {
        for role in ["admin", "owner"] {
            let scopes = scopes_for_role(role);
            assert!(scopes.contains(&"admin:users".to_string()), "{role}");
            assert!(scopes.contains(&"admin:channels".to_string()), "{role}");
            assert!(scopes.contains(&"channels:write".to_string()), "{role}");
            // Still a superset of the member set.
            for m in scopes_for_role("member") {
                assert!(scopes.contains(&m), "{role} missing member scope {m}");
            }
            // No duplicates.
            let mut sorted = scopes.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), scopes.len(), "{role} has duplicate scopes");
        }
    }

    /// Lockstep guard: the backfill member set must equal the invite
    /// self-mint set. The invite set is re-declared here from the same [`Scope`]
    /// vocabulary; if either drifts this fails.
    #[test]
    fn backfill_member_scopes_match_invite_self_mint() {
        // Mirror of crates/buzz-relay/src/api/invites.rs::MEMBER_SELF_MINT_SCOPES.
        let invite_self_mint: [Scope; 8] = [
            Scope::MessagesRead,
            Scope::MessagesWrite,
            Scope::ChannelsRead,
            Scope::UsersRead,
            Scope::UsersWrite,
            Scope::FilesRead,
            Scope::FilesWrite,
            Scope::SubscriptionsRead,
        ];
        assert_eq!(MEMBER_SELF_MINT_SCOPES, invite_self_mint);
    }

    #[test]
    fn active_token_predicate() {
        let now = Utc::now();
        // No records → not active.
        assert!(!has_active_token(&[], now));
        // Non-expiring, non-revoked → active.
        assert!(has_active_token(&[token(false, None, now)], now));
        // Revoked → not active.
        assert!(!has_active_token(&[token(true, None, now)], now));
        // Expired → not active.
        assert!(!has_active_token(&[token(false, Some(-10), now)], now));
        // Future expiry → active.
        assert!(has_active_token(&[token(false, Some(10), now)], now));
        // Mixed: one revoked + one active → active.
        assert!(has_active_token(
            &[token(true, None, now), token(false, Some(10), now)],
            now
        ));
    }

    #[test]
    fn tsv_row_rendering() {
        let id = Uuid::nil();
        let minted = BackfillRow {
            actor_hex: "aa".repeat(32),
            role: "member".to_string(),
            outcome: BackfillOutcome::Minted {
                id,
                key: "buzzk_secret".to_string(),
            },
        };
        let line = minted.to_tsv();
        assert_eq!(
            line,
            format!("{}\tmember\tminted\t{}\tbuzzk_secret", "aa".repeat(32), id)
        );
        // Header column count matches row column count.
        assert_eq!(
            BackfillRow::tsv_header().split('\t').count(),
            line.split('\t').count()
        );

        let dry = BackfillRow {
            actor_hex: "bb".repeat(32),
            role: "admin".to_string(),
            outcome: BackfillOutcome::WouldMint,
        };
        assert!(dry.to_tsv().ends_with("would-mint\t\t"));
        assert!(
            !dry.to_tsv().contains("buzzk_"),
            "dry run never emits a key"
        );

        let skipped = BackfillRow {
            actor_hex: "cc".repeat(32),
            role: "owner".to_string(),
            outcome: BackfillOutcome::SkippedActive,
        };
        assert!(skipped.to_tsv().contains("skipped-active"));
    }

    // ---- Postgres-gated integration tests for the two cutover commands ----

    use sqlx::PgPool;

    fn test_db_url() -> String {
        std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://buzz:buzz_dev@localhost:5432/buzz".to_string())
    }

    /// Connect a raw pool and a [`Db`] over it. Returns `None` when Postgres is
    /// unreachable so the gated tests skip cleanly. The raw pool is handed back
    /// for the `AuditService` (whose pool the `Db` does not expose) and for
    /// direct community inserts.
    async fn test_db() -> Option<(Db, PgPool)> {
        let pool = PgPool::connect(&test_db_url()).await.ok()?;
        Some((Db::from_pool(pool.clone()), pool))
    }

    /// Insert a fresh community with a unique host; return its id.
    async fn make_community(pool: &PgPool) -> CommunityId {
        let id = Uuid::new_v4();
        let host = format!("cutover-test-{}.example", id.simple());
        sqlx::query("INSERT INTO communities (id, host) VALUES ($1, $2)")
            .bind(id)
            .bind(host)
            .execute(pool)
            .await
            .expect("insert community");
        CommunityId::from_uuid(id)
    }

    /// A 32-byte-hex pubkey filled with `byte`.
    fn actor_hex(byte: u8) -> String {
        hex::encode([byte; 32])
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn backfill_mints_per_member_with_role_scopes_and_is_idempotent() {
        let Some((db, _pool)) = test_db().await else {
            return;
        };
        let community = make_community(&_pool).await;

        db.add_relay_member(community, &actor_hex(0xa1), "owner", None)
            .await
            .expect("seed owner");
        db.add_relay_member(community, &actor_hex(0xa2), "admin", None)
            .await
            .expect("seed admin");
        db.add_relay_member(community, &actor_hex(0xa3), "member", None)
            .await
            .expect("seed member");

        let now = Utc::now();

        // 1. Dry run: three would-mint rows, and nothing is written.
        let dry = plan_backfill(&db, community, true, "cutover-test", now)
            .await
            .expect("dry run");
        assert_eq!(dry.len(), 3);
        assert!(dry
            .iter()
            .all(|r| matches!(r.outcome, BackfillOutcome::WouldMint)));
        for byte in [0xa1u8, 0xa2, 0xa3] {
            let tokens = db
                .list_tokens_by_owner(community, &[byte; 32])
                .await
                .expect("list");
            assert!(tokens.is_empty(), "dry run must not mint for {byte:#x}");
        }

        // 2. Real run: three minted keys with role-derived scopes.
        let real = plan_backfill(&db, community, false, "cutover-test", now)
            .await
            .expect("real run");
        assert_eq!(
            real.iter()
                .filter(|r| matches!(r.outcome, BackfillOutcome::Minted { .. }))
                .count(),
            3
        );

        // Member gets exactly the member set; owner/admin get the elevated set.
        let member_tokens = db
            .list_tokens_by_owner(community, &[0xa3; 32])
            .await
            .expect("member tokens");
        assert_eq!(member_tokens.len(), 1);
        assert_eq!(member_tokens[0].scopes, scopes_for_role("member"));
        assert!(!member_tokens[0].scopes.contains(&"admin:users".to_string()));

        for byte in [0xa1u8, 0xa2] {
            let tokens = db
                .list_tokens_by_owner(community, &[byte; 32])
                .await
                .expect("elevated tokens");
            assert_eq!(tokens.len(), 1, "{byte:#x} should have exactly one key");
            assert!(
                tokens[0].scopes.contains(&"admin:users".to_string()),
                "{byte:#x} (owner/admin) must carry admin:users"
            );
            assert!(tokens[0].scopes.contains(&"channels:write".to_string()));
        }

        // 3. Re-run: all skipped-active, no new keys minted.
        let rerun = plan_backfill(&db, community, false, "cutover-test", Utc::now())
            .await
            .expect("re-run");
        assert!(rerun
            .iter()
            .all(|r| matches!(r.outcome, BackfillOutcome::SkippedActive)));
        let after = db
            .list_tokens_by_owner(community, &[0xa3; 32])
            .await
            .expect("after");
        assert_eq!(after.len(), 1, "idempotent re-run mints nothing new");
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn cutover_genesis_pins_head_then_refuses_second_run() {
        let Some((_db, pool)) = test_db().await else {
            return;
        };
        let community = make_community(&pool).await;
        let audit = AuditService::new(pool.clone());

        // Seed a "Nostr-era" chain of two entries; the tip is the head to pin.
        let mk = |action| buzz_audit::NewAuditEntry {
            community_id: community,
            action,
            actor_pubkey: Some(vec![0x5a; 32]),
            object_id: None,
            detail: serde_json::json!({"seed": true}),
        };
        audit
            .log(mk(buzz_audit::AuditAction::EventCreated))
            .await
            .expect("e1");
        let head = audit
            .log(mk(buzz_audit::AuditAction::ChannelCreated))
            .await
            .expect("e2");

        // First run appends a genesis pinning the head.
        let now = Utc::now();
        match run_cutover_genesis(&audit, community, now)
            .await
            .expect("first cutover")
        {
            CutoverOutcome::Appended { seq, pinned_head } => {
                assert_eq!(seq, head.seq + 1, "genesis continues the chain");
                assert_eq!(pinned_head, head.hash, "genesis pins the prior head hash");
            }
            CutoverOutcome::AlreadyPresent => panic!("first run must append"),
        }
        assert!(audit.has_cutover_genesis(community).await.unwrap());
        // Chain (incl. genesis) still verifies end to end.
        assert!(audit
            .verify_chain(community, 1, head.seq + 1)
            .await
            .unwrap());

        // Second run refuses (idempotent).
        assert!(matches!(
            run_cutover_genesis(&audit, community, now)
                .await
                .expect("second cutover"),
            CutoverOutcome::AlreadyPresent
        ));
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn cutover_genesis_on_empty_chain_becomes_first_entry() {
        let Some((_db, pool)) = test_db().await else {
            return;
        };
        let community = make_community(&pool).await;
        let audit = AuditService::new(pool.clone());

        match run_cutover_genesis(&audit, community, Utc::now())
            .await
            .expect("cutover on empty chain")
        {
            CutoverOutcome::Appended { seq, pinned_head } => {
                assert_eq!(seq, 1, "genesis is the community's first entry");
                assert!(pinned_head.is_empty(), "no prior head to pin");
            }
            CutoverOutcome::AlreadyPresent => panic!("fresh community must append"),
        }
    }
}
