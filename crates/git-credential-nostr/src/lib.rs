//! git-credential-nostr — bearer-token git credential helper for Buzz.
//!
//! Git calls this via the credential helper protocol (stdin/stdout). We read the
//! request and, for a Buzz remote, return the API key from `BUZZ_API_KEY` as a
//! bearer credential. Git then sends:
//!   Authorization: Bearer <BUZZ_API_KEY>
//!
//! The binary/crate name is retained (`git-credential-nostr`) so existing shim,
//! sidecar, and multicall references keep resolving after the Nostr → API-key
//! swap. It no longer signs any Nostr event; the key is read straight from the
//! environment.
//!
//! # Buzz-remote detection
//!
//! The helper only emits a credential when the server's `WWW-Authenticate`
//! challenge (forwarded by git as `wwwauth[]=`) is `Bearer` with Buzz's realm.
//! Non-Buzz remotes see no credential (silent exit 0), so git falls through to
//! system helpers and the bearer key is never leaked to third-party hosts.

use std::io::{self, BufRead, Write};

/// The `WWW-Authenticate` realm the Buzz relay advertises for git routes.
const BUZZ_REALM: &str = "realm=\"buzz\"";

/// Environment variable carrying the bearer API key.
const API_KEY_ENV: &str = "BUZZ_API_KEY";

#[derive(Default)]
struct CredRequest {
    has_authtype_capability: bool,
    wwwauth: Option<String>,
}

fn parse_stdin() -> CredRequest {
    let stdin = io::stdin();
    let mut req = CredRequest::default();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.is_empty() {
            break;
        }
        if line == "capability[]=authtype" {
            req.has_authtype_capability = true;
        } else if let Some(v) = line.strip_prefix("wwwauth[]=") {
            // Capture the first Bearer challenge; ignore others (Basic, etc.).
            if v.trim_start().starts_with("Bearer") && req.wwwauth.is_none() {
                req.wwwauth = Some(v.to_string());
            }
        }
    }
    req
}

/// Whether a `WWW-Authenticate` challenge identifies a Buzz relay bearer route.
///
/// Requires both the `Bearer` scheme and Buzz's `realm="buzz"` marker so the
/// bearer key is only ever presented to the Buzz relay, never to an unrelated
/// server that happens to use bearer auth.
fn is_buzz_bearer_challenge(wwwauth: &str) -> bool {
    let trimmed = wwwauth.trim_start();
    trimmed.starts_with("Bearer") && trimmed.contains(BUZZ_REALM)
}

/// Run the credential helper. Returns exit code.
/// Reads from stdin, writes to stdout. Errors go to stderr only.
pub fn run() -> i32 {
    match std::env::args().nth(1).as_deref() {
        Some("get") | None => {}
        Some(_) => return 0, // store, erase, or unknown → silent exit 0
    }

    let req = parse_stdin();

    // Without the authtype capability git cannot carry a Bearer credential.
    // Emit an empty response so git falls through to the next helper.
    if !req.has_authtype_capability {
        println!();
        let _ = io::stdout().flush();
        return 0;
    }

    // Only respond to Buzz's bearer challenge. Any other (or absent) challenge
    // means this isn't a Buzz remote — exit silently so git tries the next
    // helper and the bearer key never leaks to third-party hosts.
    match req.wwwauth.as_deref() {
        Some(v) if is_buzz_bearer_challenge(v) => {}
        _ => return 0,
    }

    let key = match std::env::var(API_KEY_ENV) {
        Ok(k) if !k.is_empty() => k,
        _ => {
            eprintln!("error: {API_KEY_ENV} is not set; cannot authenticate to Buzz git remote");
            return 1;
        }
    };

    println!("capability[]=authtype");
    println!("authtype=Bearer");
    println!("credential={key}");
    println!("ephemeral=true");
    println!("quit=true");
    println!();
    let _ = io::stdout().flush();
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buzz_bearer_challenge_detected() {
        assert!(is_buzz_bearer_challenge("Bearer realm=\"buzz\""));
        assert!(is_buzz_bearer_challenge(
            "Bearer realm=\"buzz\", charset=\"UTF-8\""
        ));
    }

    #[test]
    fn non_buzz_bearer_challenge_rejected() {
        // Bearer but wrong realm — must not present the Buzz key.
        assert!(!is_buzz_bearer_challenge("Bearer realm=\"github\""));
        // No realm at all.
        assert!(!is_buzz_bearer_challenge("Bearer"));
        // Different scheme.
        assert!(!is_buzz_bearer_challenge("Basic realm=\"buzz\""));
    }
}
