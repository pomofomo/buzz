//! API-key lifecycle for `buzz-admin` (Lane J).
//!
//! Operator-facing provisioning of hashed API keys backed by
//! [`buzz_db::api_token`]. This module owns the small, pure, testable helpers
//! — token generation, scope validation, expiry parsing, and the print-once
//! surface — while the async command handlers live in `main.rs` and call these.
//!
//! # Key model
//!
//! A key is a random opaque secret shown to the operator **exactly once** at
//! issuance. Only its SHA-256 hash is persisted (`api_tokens.token_hash`); the
//! plaintext is never logged or stored. The hash is computed over the raw
//! UTF-8 bytes of the printed token string, matching the relay's bearer
//! verification (`AuthService::verify_api_key`), so an issued key round-trips
//! as an `Authorization: Bearer <token>` credential without transformation.

use buzz_auth::scope::{parse_scopes, Scope};
use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Human-readable prefix on every issued token.
///
/// Purely cosmetic — it is part of the string whose SHA-256 is stored and
/// presented, so it must be reproduced verbatim by the client. It exists to
/// make a leaked Buzz key recognisable in logs/secret-scanners.
pub(crate) const TOKEN_PREFIX: &str = "buzzk_";

/// Number of random bytes in the secret body of a token (256 bits of entropy).
pub(crate) const TOKEN_RANDOM_BYTES: usize = 32;

/// Compute the storage hash for a plaintext token.
///
/// SHA-256 over the raw UTF-8 bytes of the full token string (prefix
/// included). This is the exact transform the relay applies to a presented
/// bearer token, so `hash_token(issued)` equals the value the auth gate looks
/// up.
pub(crate) fn hash_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// Generate a fresh random API token and its storage hash.
///
/// The secret body is [`TOKEN_RANDOM_BYTES`] drawn from the OS-seeded
/// thread CSPRNG (`rand::random`), hex-encoded and prefixed with
/// [`TOKEN_PREFIX`]. Returns `(plaintext, sha256_hash)`; the plaintext is the
/// only place the secret is ever materialised.
pub(crate) fn generate_token() -> (String, [u8; 32]) {
    let bytes: [u8; TOKEN_RANDOM_BYTES] = rand::random();
    let token = format!("{TOKEN_PREFIX}{}", hex::encode(bytes));
    let hash = hash_token(&token);
    (token, hash)
}

/// A comma-separated list of every scope the relay understands.
///
/// Used to build helpful error messages when an operator passes an
/// unrecognised scope string.
pub(crate) fn known_scopes_list() -> String {
    Scope::all_known()
        .iter()
        .map(Scope::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Validate and canonicalise a set of requested scope strings.
///
/// Rejects an empty set and any scope the relay does not recognise (a
/// [`Scope::Unknown`] almost always means an operator typo, which we refuse
/// rather than mint an inert key). On success returns the canonical wire
/// strings with order preserved and duplicates removed.
pub(crate) fn validate_scopes(raw: &[String]) -> std::result::Result<Vec<String>, String> {
    if raw.is_empty() {
        return Err(format!(
            "at least one --scope is required. Known scopes: {}",
            known_scopes_list()
        ));
    }

    let parsed = parse_scopes(raw);

    let unknown: Vec<String> = parsed
        .iter()
        .filter_map(|s| match s {
            Scope::Unknown(u) => Some(u.clone()),
            _ => None,
        })
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "unknown scope(s): {}. Known scopes: {}",
            unknown.join(", "),
            known_scopes_list()
        ));
    }

    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(parsed.len());
    for scope in &parsed {
        let canonical = scope.as_str().to_string();
        if seen.insert(canonical.clone()) {
            out.push(canonical);
        }
    }
    Ok(out)
}

/// Parse an `--expires` value into an absolute UTC instant.
///
/// Accepts either an RFC3339 timestamp (e.g. `2026-08-01T00:00:00Z`) or a
/// relative duration `<n><unit>` where unit is one of `s`, `m`, `h`, `d`, `w`
/// (seconds, minutes, hours, days, weeks) — e.g. `30d`, `12h`, `90m`. The
/// duration form is resolved relative to `now`.
pub(crate) fn parse_expires(input: &str) -> std::result::Result<DateTime<Utc>, String> {
    parse_expires_at(input, Utc::now())
}

/// [`parse_expires`] with an injectable `now`, for deterministic tests.
fn parse_expires_at(input: &str, now: DateTime<Utc>) -> std::result::Result<DateTime<Utc>, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("empty --expires value".to_string());
    }

    // Prefer an explicit RFC3339 timestamp.
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }

    // Otherwise a relative `<n><unit>` duration.
    let (num_part, unit) = s.split_at(s.len() - 1);
    let unit_secs: i64 = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 604_800,
        _ => {
            return Err(format!(
                "invalid --expires '{input}': expected an RFC3339 timestamp \
                 (2026-08-01T00:00:00Z) or a duration like 30d, 12h, 90m, 3600s"
            ))
        }
    };

    let n: i64 = num_part.parse().map_err(|_| {
        format!(
            "invalid --expires '{input}': '{num_part}' is not a whole number of \
             {unit}-units"
        )
    })?;
    if n <= 0 {
        return Err(format!(
            "invalid --expires '{input}': duration must be positive"
        ));
    }

    let secs = n
        .checked_mul(unit_secs)
        .ok_or_else(|| format!("invalid --expires '{input}': duration overflow"))?;
    now.checked_add_signed(Duration::seconds(secs))
        .ok_or_else(|| format!("invalid --expires '{input}': timestamp overflow"))
}

/// Format a `Some(instant)` as RFC3339 or a `None` as `placeholder`.
fn fmt_opt_ts(ts: Option<DateTime<Utc>>, placeholder: &str) -> String {
    ts.map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| placeholder.to_string())
}

/// Render the one-time issuance receipt for a freshly minted key.
///
/// The returned block is printed to stdout by the `issue-key` / `rotate-key`
/// handlers. It carries the plaintext `token` **exactly once**, prefixed by a
/// prominent "store this now, it will not be shown again" notice, alongside the
/// non-secret metadata (id, actor, scopes, channels, expiry). The token hash is
/// never included.
///
/// `verb` is the past-tense action word (`"issued"` or `"rotated"`).
pub(crate) fn format_issued_token(
    verb: &str,
    token: &str,
    id: Uuid,
    actor_hex: &str,
    scopes: &[String],
    channels: Option<&[Uuid]>,
    expires_at: Option<DateTime<Utc>>,
) -> String {
    let channels_str = match channels {
        None | Some([]) => "(all — no per-key restriction)".to_string(),
        Some(ids) => ids
            .iter()
            .map(Uuid::to_string)
            .collect::<Vec<_>>()
            .join(", "),
    };

    let mut out = String::new();
    out.push_str(&format!("API key {verb}.\n\n"));
    out.push_str(&format!("  Token ID: {id}\n"));
    out.push_str(&format!("  Actor:    {actor_hex}\n"));
    out.push_str(&format!("  Scopes:   {}\n", scopes.join(", ")));
    out.push_str(&format!("  Channels: {channels_str}\n"));
    out.push_str(&format!(
        "  Expires:  {}\n",
        fmt_opt_ts(expires_at, "never")
    ));
    out.push('\n');
    out.push_str("  IMPORTANT: store this token now — it will NOT be shown again.\n");
    out.push_str("  Present it as an HTTP header: Authorization: Bearer <token>\n\n");
    out.push_str(&format!("    {token}\n"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_token_has_prefix_and_expected_length() {
        let (token, _hash) = generate_token();
        assert!(
            token.starts_with(TOKEN_PREFIX),
            "token must carry the buzzk_ prefix: {token}"
        );
        // prefix + 2 hex chars per random byte.
        assert_eq!(
            token.len(),
            TOKEN_PREFIX.len() + TOKEN_RANDOM_BYTES * 2,
            "token length must be prefix + hex(32 bytes)"
        );
        // Body is valid lowercase hex.
        let body = &token[TOKEN_PREFIX.len()..];
        assert!(
            body.chars().all(|c| c.is_ascii_hexdigit()),
            "token body must be hex: {body}"
        );
    }

    #[test]
    fn generated_hash_matches_relay_side_hashing() {
        let (token, hash) = generate_token();
        // The relay computes Sha256::digest(token.as_bytes()); the stored hash
        // must equal that so the issued key authenticates.
        let expected: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        assert_eq!(
            hash, expected,
            "stored hash must match relay bearer hashing"
        );
        assert_eq!(hash.len(), 32, "hash must be 32 bytes (api_tokens CHECK)");
    }

    #[test]
    fn token_generation_is_random() {
        // Overwhelmingly unlikely to collide unless the CSPRNG is broken.
        let mut tokens = std::collections::HashSet::new();
        let mut hashes = std::collections::HashSet::new();
        for _ in 0..1000 {
            let (token, hash) = generate_token();
            assert!(tokens.insert(token), "duplicate plaintext token generated");
            assert!(hashes.insert(hash), "duplicate token hash generated");
        }
    }

    #[test]
    fn validate_scopes_accepts_known_and_dedups() {
        let scopes = validate_scopes(&[
            "messages:read".to_string(),
            "messages:write".to_string(),
            "messages:read".to_string(), // duplicate
        ])
        .expect("known scopes accepted");
        assert_eq!(scopes, vec!["messages:read", "messages:write"]);
    }

    #[test]
    fn validate_scopes_accepts_admin_scopes() {
        let scopes = validate_scopes(&["admin:channels".to_string(), "admin:users".to_string()])
            .expect("admin scopes are known");
        assert_eq!(scopes, vec!["admin:channels", "admin:users"]);
    }

    #[test]
    fn validate_scopes_rejects_empty() {
        let err = validate_scopes(&[]).expect_err("empty scope set rejected");
        assert!(err.contains("at least one --scope"), "{err}");
    }

    #[test]
    fn validate_scopes_rejects_unknown() {
        let err = validate_scopes(&["messages:read".to_string(), "not:a:scope".to_string()])
            .expect_err("unknown scope rejected");
        assert!(
            err.contains("not:a:scope"),
            "error names the bad scope: {err}"
        );
        assert!(
            err.contains("Known scopes"),
            "error lists known scopes: {err}"
        );
    }

    #[test]
    fn parse_expires_accepts_rfc3339() {
        let dt = parse_expires("2026-08-01T00:00:00Z").expect("rfc3339 parses");
        assert_eq!(dt.to_rfc3339(), "2026-08-01T00:00:00+00:00");
    }

    #[test]
    fn parse_expires_accepts_durations() {
        let now = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let cases = [
            ("30s", 30i64),
            ("15m", 15 * 60),
            ("12h", 12 * 3600),
            ("7d", 7 * 86_400),
            ("2w", 2 * 604_800),
        ];
        for (input, secs) in cases {
            let got = parse_expires_at(input, now).unwrap_or_else(|e| panic!("{input}: {e}"));
            assert_eq!(
                got,
                now + Duration::seconds(secs),
                "duration {input} resolved wrong"
            );
        }
    }

    #[test]
    fn parse_expires_rejects_garbage() {
        for bad in ["", "abc", "10x", "-5d", "0h", "d"] {
            assert!(
                parse_expires(bad).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn format_issued_token_shows_plaintext_exactly_once() {
        let (token, _hash) = generate_token();
        let id = Uuid::nil();
        let out = format_issued_token(
            "issued",
            &token,
            id,
            "abcd",
            &["messages:read".to_string()],
            None,
            None,
        );
        assert_eq!(
            out.matches(token.as_str()).count(),
            1,
            "plaintext token must appear exactly once"
        );
        assert!(
            out.contains("will NOT be shown again"),
            "receipt must carry the store-now warning"
        );
        assert!(
            out.contains("Bearer"),
            "receipt explains how to present the key"
        );
        assert!(out.contains("never"), "no expiry renders as 'never'");
    }

    #[test]
    fn format_issued_token_never_leaks_hash() {
        let (token, hash) = generate_token();
        let out = format_issued_token(
            "issued",
            &token,
            Uuid::nil(),
            "abcd",
            &["files:read".to_string()],
            Some(&[Uuid::nil()]),
            Some(Utc::now()),
        );
        let hash_hex = hex::encode(hash);
        assert!(
            !out.contains(&hash_hex),
            "receipt must never contain the token hash"
        );
    }

    #[test]
    fn format_issued_token_lists_channels() {
        let ch = Uuid::from_u128(1);
        let out = format_issued_token(
            "rotated",
            "buzzk_deadbeef",
            Uuid::nil(),
            "abcd",
            &["messages:write".to_string()],
            Some(&[ch]),
            None,
        );
        assert!(out.contains(&ch.to_string()), "channel id must be listed");
        assert!(out.contains("rotated"), "verb is surfaced");
    }
}
