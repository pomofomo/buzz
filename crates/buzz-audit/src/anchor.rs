//! External WORM anchoring of the per-community audit chain head (Lane F).
//!
//! Decision #4 of the refactor keeps the Postgres `prev_hash → hash` chain as
//! the source of truth and *adds* an out-of-band tamper-evidence layer: on a
//! configurable interval (and once at graceful shutdown) the current head of
//! every community's chain — `MAX(seq)` and its `hash` — is exported as a small
//! JSON object to an S3 bucket configured for **Object Lock (WORM)**. Once a
//! head hash is written to a locked object it cannot be altered or deleted for
//! the bucket's retention period, so an operator (or an attacker) who later
//! rewrites the Postgres chain cannot also rewrite the anchored evidence: a
//! `verify_chain` pass whose recomputed head no longer matches the WORM record
//! is proof of tampering.
//!
//! This module is **purely additive and off by default**
//! ([`AnchorConfig::enabled`] defaults to `false`). When disabled nothing is
//! constructed, spawned, or written, and the relay behaves exactly as before.
//!
//! ## WORM / Object Lock expectations
//!
//! The target bucket must have S3 Object Lock enabled with a **default
//! retention rule** (compliance or governance mode). This module writes each
//! anchor under a unique, monotonic key (`{prefix}/{community}/{seq}.json`), so
//! it never needs to overwrite a locked object — bucket-level default retention
//! is what makes the write immutable. No per-object retention headers are set;
//! immutability is a property of the bucket configuration, provisioned out of
//! band (Terraform / bucket policy), not of this writer.

use std::future::Future;
use std::time::Duration;

use chrono::{DateTime, Utc};
use s3::creds::Credentials;
use s3::{Bucket, Region};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use tracing::{error, info, instrument, warn};
use uuid::Uuid;

use crate::error::AuditError;

/// Default anchoring interval when `BUZZ_AUDIT_ANCHOR_INTERVAL_SECS` is unset.
const DEFAULT_ANCHOR_INTERVAL_SECS: u64 = 3600;

/// Key prefix for anchor objects within the WORM bucket.
const ANCHOR_KEY_PREFIX: &str = "audit-anchor";

/// Configuration for the external audit head-hash anchor worker.
///
/// Populated from the environment via [`AnchorConfig::from_env`]. S3
/// endpoint/region/credentials mirror the way `buzz-media` configures its
/// client: anchor-specific `BUZZ_AUDIT_ANCHOR_S3_*` variables take precedence,
/// falling back to the shared `BUZZ_S3_*` (and `AWS_REGION`) variables so a
/// deployment that already points `buzz-media` at S3 needs only to set
/// `BUZZ_AUDIT_ANCHOR_ENABLED` and `BUZZ_AUDIT_ANCHOR_BUCKET`.
#[derive(Debug, Clone)]
pub struct AnchorConfig {
    /// Master switch. When `false` (the default) no anchor client is built and
    /// no worker runs.
    pub enabled: bool,
    /// Destination bucket. Must be Object-Lock (WORM) enabled with a default
    /// retention rule. Required when `enabled` is `true`.
    pub bucket: String,
    /// S3-compatible endpoint URL (e.g. `https://s3.us-east-1.amazonaws.com`).
    pub endpoint: String,
    /// AWS region for SigV4 signing.
    pub region: String,
    /// Static access key. Empty (together with `secret_key`) selects the AWS
    /// default credential chain (env, profile, IRSA web-identity, instance
    /// metadata) — the same policy `buzz-media` uses.
    pub access_key: String,
    /// Static secret key. See [`AnchorConfig::access_key`].
    pub secret_key: String,
    /// Seconds between anchor sweeps.
    pub interval_secs: u64,
}

impl Default for AnchorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bucket: String::new(),
            endpoint: String::new(),
            region: "us-east-1".to_string(),
            access_key: String::new(),
            secret_key: String::new(),
            interval_secs: DEFAULT_ANCHOR_INTERVAL_SECS,
        }
    }
}

impl AnchorConfig {
    /// Build an [`AnchorConfig`] from the process environment.
    ///
    /// Recognised variables:
    /// - `BUZZ_AUDIT_ANCHOR_ENABLED` — `true`/`1` to enable (default off).
    /// - `BUZZ_AUDIT_ANCHOR_BUCKET` — WORM bucket name.
    /// - `BUZZ_AUDIT_ANCHOR_INTERVAL_SECS` — sweep interval (default 3600).
    /// - `BUZZ_AUDIT_ANCHOR_S3_ENDPOINT` / `BUZZ_S3_ENDPOINT` — endpoint URL.
    /// - `BUZZ_AUDIT_ANCHOR_S3_REGION` / `BUZZ_S3_REGION` / `AWS_REGION` — region.
    /// - `BUZZ_AUDIT_ANCHOR_S3_ACCESS_KEY` / `BUZZ_S3_ACCESS_KEY` — access key.
    /// - `BUZZ_AUDIT_ANCHOR_S3_SECRET_KEY` / `BUZZ_S3_SECRET_KEY` — secret key.
    ///
    /// This never fails: absent/invalid values fall back to defaults, and an
    /// enabled-but-misconfigured worker surfaces the problem at
    /// [`AuditAnchor::from_config`] time (logged, non-fatal).
    pub fn from_env() -> Self {
        let enabled = std::env::var("BUZZ_AUDIT_ANCHOR_ENABLED")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);

        let bucket = std::env::var("BUZZ_AUDIT_ANCHOR_BUCKET").unwrap_or_default();

        let endpoint = std::env::var("BUZZ_AUDIT_ANCHOR_S3_ENDPOINT")
            .or_else(|_| std::env::var("BUZZ_S3_ENDPOINT"))
            .unwrap_or_else(|_| "http://localhost:9000".to_string());

        let region = std::env::var("BUZZ_AUDIT_ANCHOR_S3_REGION")
            .or_else(|_| std::env::var("BUZZ_S3_REGION"))
            .or_else(|_| std::env::var("AWS_REGION"))
            .unwrap_or_else(|_| "us-east-1".to_string());

        let access_key = std::env::var("BUZZ_AUDIT_ANCHOR_S3_ACCESS_KEY")
            .or_else(|_| std::env::var("BUZZ_S3_ACCESS_KEY"))
            .unwrap_or_default();

        let secret_key = std::env::var("BUZZ_AUDIT_ANCHOR_S3_SECRET_KEY")
            .or_else(|_| std::env::var("BUZZ_S3_SECRET_KEY"))
            .unwrap_or_default();

        let interval_secs = std::env::var("BUZZ_AUDIT_ANCHOR_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(DEFAULT_ANCHOR_INTERVAL_SECS);

        Self {
            enabled,
            bucket,
            endpoint,
            region,
            access_key,
            secret_key,
            interval_secs,
        }
    }
}

/// The exported anchor record for one community's chain head. Serialized to
/// JSON and written as a single WORM object.
///
/// `hash` is lowercase hex of the 32-byte chain-head SHA-256 so the artifact is
/// human-readable and diff-friendly in the bucket. `timestamp` is the wall
/// clock at which the anchor was taken (not the audit entry's `created_at`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorPayload {
    /// Community whose chain head this record anchors.
    pub community_id: Uuid,
    /// Head sequence number (`MAX(seq)`) at anchor time.
    pub seq: i64,
    /// Lowercase hex of the head entry's 32-byte chain hash.
    pub hash: String,
    /// Wall-clock time the anchor was taken.
    pub timestamp: DateTime<Utc>,
}

impl AnchorPayload {
    /// Build a payload from a community head's raw fields.
    pub fn new(community_id: Uuid, seq: i64, hash: &[u8], timestamp: DateTime<Utc>) -> Self {
        Self {
            community_id,
            seq,
            hash: hex::encode(hash),
            timestamp,
        }
    }

    /// The WORM object key for this anchor: `audit-anchor/{community}/{seq}.json`.
    ///
    /// `seq` is monotonic per community, so every anchor lands on a fresh key —
    /// the writer never overwrites (and could not overwrite) a locked object.
    pub fn object_key(&self) -> String {
        format!(
            "{ANCHOR_KEY_PREFIX}/{}/{}.json",
            self.community_id, self.seq
        )
    }
}

/// One community chain head as read from `audit_log`.
struct ChainHead {
    community_id: Uuid,
    seq: i64,
    hash: Vec<u8>,
}

/// Read the current head (`MAX(seq)` and its `hash`) of every community's
/// chain in a single query. Communities with no audit rows are absent.
async fn read_all_heads(pool: &PgPool) -> Result<Vec<ChainHead>, AuditError> {
    let rows = sqlx::query(
        r#"
        SELECT DISTINCT ON (community_id) community_id, seq, hash
        FROM audit_log
        ORDER BY community_id, seq DESC
        "#,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .map(|row| ChainHead {
            community_id: row.get::<Uuid, _>("community_id"),
            seq: row.get::<i64, _>("seq"),
            hash: row.get::<Vec<u8>, _>("hash"),
        })
        .collect())
}

/// External WORM anchor for audit chain heads.
///
/// Holds the Postgres pool (to read heads) and an S3 client bound to the
/// configured Object-Lock bucket. Construct via [`AuditAnchor::from_config`];
/// drive with [`AuditAnchor::anchor_all`] or the background
/// [`spawn_anchor_worker`].
pub struct AuditAnchor {
    pool: PgPool,
    bucket: Box<Bucket>,
}

impl AuditAnchor {
    /// Build an anchor client from config. Returns an error if the S3
    /// credentials/region/bucket cannot be resolved.
    ///
    /// Credential selection mirrors `buzz-media`: both static keys present →
    /// use them; both empty → AWS default credential chain; exactly one present
    /// → hard error (a half-configured credential is always a mistake).
    pub fn from_config(pool: PgPool, config: &AnchorConfig) -> Result<Self, AuditError> {
        if config.bucket.is_empty() {
            return Err(AuditError::Anchor(
                "BUZZ_AUDIT_ANCHOR_BUCKET must be set when anchoring is enabled".to_string(),
            ));
        }

        let region = Region::Custom {
            region: config.region.clone(),
            endpoint: config.endpoint.clone(),
        };

        let creds = match (config.access_key.is_empty(), config.secret_key.is_empty()) {
            (false, false) => Credentials::new(
                Some(&config.access_key),
                Some(&config.secret_key),
                None,
                None,
                None,
            ),
            (true, true) => Credentials::default(),
            _ => {
                return Err(AuditError::Anchor(
                    "anchor S3 access key and secret key must be configured together, or both \
                     empty to use the AWS credential chain"
                        .to_string(),
                ));
            }
        }
        .map_err(|e| AuditError::Anchor(e.to_string()))?;

        let bucket = Bucket::new(&config.bucket, region, creds)
            .map_err(|e| AuditError::Anchor(e.to_string()))?
            .with_path_style();

        Ok(Self { pool, bucket })
    }

    /// Anchor the current head of every community's chain to WORM storage.
    ///
    /// Reads all heads, then writes one immutable object per community. A write
    /// failure for one community is logged and does not abort the sweep — the
    /// next interval retries. Returns the number of heads successfully written.
    #[instrument(skip(self))]
    pub async fn anchor_all(&self) -> Result<usize, AuditError> {
        let heads = read_all_heads(&self.pool).await?;
        let mut written = 0usize;
        let now = Utc::now();

        for head in &heads {
            let payload = AnchorPayload::new(head.community_id, head.seq, &head.hash, now);
            match self.put_anchor(&payload).await {
                Ok(()) => written += 1,
                Err(e) => {
                    // Per-community failure is non-fatal for the sweep; the head
                    // is re-anchored next interval. Log without the community id
                    // to keep operator logs free of cross-tenant identifiers.
                    warn!(seq = head.seq, error = %e, "audit anchor write failed for a community");
                }
            }
        }

        info!(
            communities = heads.len(),
            written, "audit anchor sweep complete"
        );
        Ok(written)
    }

    /// Serialize and PUT a single anchor payload as an immutable WORM object.
    async fn put_anchor(&self, payload: &AnchorPayload) -> Result<(), AuditError> {
        let body = serde_json::to_vec(payload)?;
        self.bucket
            .put_object_with_content_type(payload.object_key(), &body, "application/json")
            .await
            .map_err(|e| AuditError::Anchor(e.to_string()))?;
        Ok(())
    }
}

/// Spawn the background anchor worker.
///
/// The worker anchors immediately on start, then every `config.interval_secs`,
/// and once more when `shutdown` resolves (graceful-shutdown flush) before
/// exiting. If `config.enabled` is `false`, or the S3 client cannot be built,
/// the worker logs and exits without doing anything — anchoring is strictly
/// additive and never blocks or fails relay startup.
///
/// `shutdown` is any future that resolves when the process should stop
/// anchoring — e.g. a `tokio_util::sync::CancellationToken`'s
/// `cancelled_owned()` or a `watch::Receiver` change. Returns the worker's
/// [`tokio::task::JoinHandle`].
///
/// # Example
///
/// ```no_run
/// # use buzz_audit::anchor::{AnchorConfig, spawn_anchor_worker};
/// # use sqlx::PgPool;
/// # async fn wire(pool: PgPool) {
/// let config = AnchorConfig::from_env();
/// // `shutdown` is any future that resolves when anchoring should stop; here
/// // a watch channel stands in for the host's shutdown signal.
/// let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
/// let handle = spawn_anchor_worker(pool, config, async move {
///     let _ = shutdown_rx.changed().await;
/// });
/// // ... on shutdown: `shutdown_tx.send(true)` triggers a final anchor, then
/// // the worker task completes; await `handle` to join it.
/// let _ = shutdown_tx.send(true);
/// let _ = handle.await;
/// # }
/// ```
pub fn spawn_anchor_worker<F>(
    pool: PgPool,
    config: AnchorConfig,
    shutdown: F,
) -> tokio::task::JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(run_anchor_worker(pool, config, shutdown))
}

/// Worker body — separated from [`spawn_anchor_worker`] for testability.
async fn run_anchor_worker<F>(pool: PgPool, config: AnchorConfig, shutdown: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    if !config.enabled {
        info!("audit anchor worker disabled (BUZZ_AUDIT_ANCHOR_ENABLED not set)");
        return;
    }

    let anchor = match AuditAnchor::from_config(pool, &config) {
        Ok(a) => a,
        Err(e) => {
            error!(error = %e, "audit anchor worker not started: client init failed");
            return;
        }
    };

    let interval_secs = config.interval_secs.max(1);
    info!(interval_secs, "audit anchor worker started");

    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if let Err(e) = anchor.anchor_all().await {
                    error!(error = %e, "audit anchor sweep failed");
                }
            }
            _ = &mut shutdown => {
                info!("audit anchor worker draining: final anchor before shutdown");
                if let Err(e) = anchor.anchor_all().await {
                    error!(error = %e, "final audit anchor sweep failed");
                }
                break;
            }
        }
    }
    info!("audit anchor worker exited");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_payload() -> AnchorPayload {
        AnchorPayload::new(
            Uuid::from_u128(0x1234),
            42,
            &[0xabu8; 32],
            DateTime::parse_from_rfc3339("2026-01-02T03:04:05Z")
                .expect("valid rfc3339")
                .with_timezone(&Utc),
        )
    }

    #[test]
    fn payload_hashes_to_lowercase_hex() {
        let p = sample_payload();
        assert_eq!(p.hash, "ab".repeat(32));
        assert_eq!(p.hash.len(), 64);
        assert_eq!(p.seq, 42);
        assert_eq!(p.community_id, Uuid::from_u128(0x1234));
    }

    #[test]
    fn payload_serializes_all_fields() {
        let p = sample_payload();
        let v: serde_json::Value = serde_json::to_value(&p).expect("serialize");
        assert_eq!(v["community_id"], serde_json::json!(p.community_id));
        assert_eq!(v["seq"], serde_json::json!(42));
        assert_eq!(v["hash"], serde_json::json!("ab".repeat(32)));
        assert_eq!(v["timestamp"], serde_json::json!("2026-01-02T03:04:05Z"));
    }

    #[test]
    fn payload_roundtrips_through_json() {
        let p = sample_payload();
        let bytes = serde_json::to_vec(&p).expect("serialize");
        let back: AnchorPayload = serde_json::from_slice(&bytes).expect("deserialize");
        assert_eq!(p, back);
    }

    #[test]
    fn object_key_is_unique_and_worm_safe() {
        let p = sample_payload();
        assert_eq!(
            p.object_key(),
            format!("audit-anchor/{}/42.json", Uuid::from_u128(0x1234))
        );

        // Different heads (different seq) never collide → no overwrite of a
        // locked object.
        let mut later = sample_payload();
        later.seq = 43;
        assert_ne!(p.object_key(), later.object_key());
    }

    #[test]
    fn config_default_is_disabled() {
        let c = AnchorConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.interval_secs, DEFAULT_ANCHOR_INTERVAL_SECS);
    }

    #[tokio::test]
    async fn from_config_rejects_empty_bucket() {
        // A lazily-connected pool never touches the network here (but needs a
        // Tokio context to build); from_config fails on the bucket check before
        // any S3 work.
        let pool =
            PgPool::connect_lazy("postgres://buzz:buzz@localhost/buzz").expect("lazy pool builds");
        let mut config = AnchorConfig {
            enabled: true,
            ..AnchorConfig::default()
        };
        config.bucket = String::new();
        // AuditAnchor isn't Debug (holds an S3 Bucket), so match rather than
        // unwrap_err.
        match AuditAnchor::from_config(pool, &config) {
            Ok(_) => panic!("empty bucket must be rejected"),
            Err(e) => assert!(matches!(e, AuditError::Anchor(_))),
        }
    }

    #[tokio::test]
    async fn from_config_rejects_half_configured_credentials() {
        let pool =
            PgPool::connect_lazy("postgres://buzz:buzz@localhost/buzz").expect("lazy pool builds");
        let config = AnchorConfig {
            enabled: true,
            bucket: "audit-worm".to_string(),
            access_key: "only-access".to_string(),
            secret_key: String::new(),
            ..AnchorConfig::default()
        };
        match AuditAnchor::from_config(pool, &config) {
            Ok(_) => panic!("half-configured credentials must be rejected"),
            Err(e) => assert!(matches!(e, AuditError::Anchor(_))),
        }
    }

    #[tokio::test]
    async fn disabled_worker_exits_immediately() {
        let pool =
            PgPool::connect_lazy("postgres://buzz:buzz@localhost/buzz").expect("lazy pool builds");
        // enabled = false → returns without building a client or touching S3.
        run_anchor_worker(pool, AnchorConfig::default(), std::future::ready(())).await;
    }

    /// End-to-end WORM proof against a live S3-compatible store (MinIO).
    ///
    /// Seeds one community's audit chain, anchors every head, then reads the
    /// written object back and asserts it carries the head's seq and hash — the
    /// property an operator relies on to detect a later Postgres rewrite.
    ///
    /// Gated on both Postgres and the MinIO env being reachable; skips (returns)
    /// when either is absent so it never breaks the offline unit run. Point it
    /// at a bucket with:
    /// ```text
    /// DATABASE_URL=postgres://buzz:buzz_dev@localhost:5432/buzz \
    /// BUZZ_AUDIT_ANCHOR_S3_ENDPOINT=http://localhost:9000 \
    /// BUZZ_AUDIT_ANCHOR_S3_ACCESS_KEY=buzz_dev \
    /// BUZZ_AUDIT_ANCHOR_S3_SECRET_KEY=buzz_dev_secret \
    /// BUZZ_AUDIT_ANCHOR_BUCKET=buzz-audit-worm \
    /// cargo test -p buzz-audit anchor::tests::anchor_all_writes_head_to_worm -- --ignored
    /// ```
    #[tokio::test]
    #[ignore = "requires Postgres + MinIO (live WORM anchor test)"]
    async fn anchor_all_writes_head_to_worm() {
        use crate::{action::AuditAction, entry::NewAuditEntry, AuditService};
        use buzz_core::CommunityId;

        let db_url = match std::env::var("DATABASE_URL") {
            Ok(u) => u,
            Err(_) => return,
        };
        let Ok(pool) = PgPool::connect(&db_url).await else {
            return;
        };

        // Seed a fresh community with two audit entries; the tip is the head.
        let community = Uuid::new_v4();
        let host = format!("anchor-worm-{community}.example");
        sqlx::query("INSERT INTO communities (id, host) VALUES ($1, $2)")
            .bind(community)
            .bind(host)
            .execute(&pool)
            .await
            .expect("insert community");

        let svc = AuditService::new(pool.clone());
        let cid = CommunityId::from_uuid(community);
        let mk = |action| NewAuditEntry {
            community_id: cid,
            action,
            actor_pubkey: Some(vec![0x11; 32]),
            object_id: None,
            detail: serde_json::json!({"anchor": "test"}),
        };
        svc.log(mk(AuditAction::EventCreated)).await.expect("e1");
        let head = svc.log(mk(AuditAction::ChannelCreated)).await.expect("e2");

        // Anchor to MinIO. Force enabled + the anchor-specific bucket/creds so
        // the test does not depend on a fully-populated ambient environment.
        let mut config = AnchorConfig::from_env();
        config.enabled = true;
        if config.bucket.is_empty() {
            config.bucket = "buzz-audit-worm".to_string();
        }
        if config.access_key.is_empty() && config.secret_key.is_empty() {
            config.access_key = "buzz_dev".to_string();
            config.secret_key = "buzz_dev_secret".to_string();
        }

        let anchor = AuditAnchor::from_config(pool.clone(), &config).expect("anchor client");
        let written = anchor.anchor_all().await.expect("anchor sweep");
        assert!(
            written >= 1,
            "at least this community's head must be written"
        );

        // Read the object back and prove it pins the head seq + hash.
        let payload = AnchorPayload::new(community, head.seq, &head.hash, Utc::now());
        let key = payload.object_key();
        let response = anchor
            .bucket
            .get_object(&key)
            .await
            .unwrap_or_else(|e| panic!("read anchored object {key}: {e}"));
        let fetched: AnchorPayload =
            serde_json::from_slice(response.as_slice()).expect("anchored JSON parses");

        assert_eq!(fetched.community_id, community);
        assert_eq!(fetched.seq, head.seq, "anchored seq must equal chain head");
        assert_eq!(
            fetched.hash,
            hex::encode(&head.hash),
            "anchored hash must equal chain-head hash (lowercase hex)"
        );
    }
}
