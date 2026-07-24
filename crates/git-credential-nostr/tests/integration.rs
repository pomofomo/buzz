//! Integration tests for git-credential-nostr (bearer credential helper).
//!
//! Each test spawns the compiled binary as a subprocess, feeds it the
//! credential-helper protocol on stdin, and asserts on stdout/stderr/exit-code.
//! Under the API-key model the helper mints `Authorization: Bearer <BUZZ_API_KEY>`
//! for Buzz remotes and silently declines everything else.

use std::io::Write;
use std::process::{Command, Stdio};

/// Spawn the binary with optional `args`, write `input` to stdin, collect output.
/// `env_vars` are added on top of the inherited environment. Auth-relevant env
/// vars are cleared first to prevent host/CI pollution.
fn run_helper_args(args: &[&str], input: &str, env_vars: &[(&str, &str)]) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_git-credential-nostr");
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(std::env::temp_dir())
        .env_remove("BUZZ_API_KEY")
        .env_remove("NOSTR_PRIVATE_KEY")
        .env("HOME", std::env::temp_dir());
    for (k, v) in env_vars {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("failed to spawn git-credential-nostr");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().expect("failed to wait on child")
}

fn run_helper(input: &str, env_vars: &[(&str, &str)]) -> std::process::Output {
    run_helper_args(&["get"], input, env_vars)
}

/// Standard credential-helper input for a Buzz remote: advertises the authtype
/// capability and carries the relay's `Bearer realm="buzz"` challenge.
fn buzz_input() -> String {
    "capability[]=authtype\n\
     capability[]=state\n\
     protocol=https\n\
     host=relay.example.com\n\
     path=git/owner/repo.git/info/refs\n\
     wwwauth[]=Bearer realm=\"buzz\"\n\
     \n"
    .to_string()
}

/// Happy path: `BUZZ_API_KEY` set + Buzz bearer challenge → `authtype=Bearer`
/// and the key echoed as the credential.
#[test]
fn happy_path_emits_bearer_credential() {
    let out = run_helper(&buzz_input(), &[("BUZZ_API_KEY", "secret-api-key-123")]);

    assert!(
        out.status.success(),
        "expected exit 0, got {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();

    assert!(
        lines.contains(&"capability[]=authtype"),
        "missing capability[]=authtype in:\n{stdout}"
    );
    assert!(
        lines.contains(&"authtype=Bearer"),
        "missing authtype=Bearer in:\n{stdout}"
    );
    assert!(
        lines.contains(&"credential=secret-api-key-123"),
        "missing/incorrect credential in:\n{stdout}"
    );
    assert!(
        lines.contains(&"ephemeral=true"),
        "missing ephemeral=true in:\n{stdout}"
    );
    assert!(
        lines.contains(&"quit=true"),
        "missing quit=true in:\n{stdout}"
    );
}

/// Old git (no `capability[]=authtype`) → single blank line, exit 0, no credential.
#[test]
fn old_git_no_authtype_capability() {
    let input = "protocol=https\n\
                 host=relay.example.com\n\
                 path=git/owner/repo.git/info/refs\n\
                 wwwauth[]=Bearer realm=\"buzz\"\n\
                 \n";

    let out = run_helper(input, &[("BUZZ_API_KEY", "secret-api-key-123")]);

    assert!(
        out.status.success(),
        "expected exit 0 for old-git path, got {:?}",
        out.status.code()
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim(), "", "expected empty output, got:\n{stdout}");
    assert!(
        !stdout.contains("credential="),
        "should not emit credential= for old git"
    );
}

/// A non-Buzz bearer challenge (wrong realm) must NOT leak the key — silent
/// decline (exit 0, no credential) so git falls through to other helpers.
#[test]
fn non_buzz_challenge_declines_without_leaking_key() {
    let input = "capability[]=authtype\n\
                 protocol=https\n\
                 host=github.com\n\
                 path=owner/repo.git/info/refs\n\
                 wwwauth[]=Bearer realm=\"github\"\n\
                 \n";

    let out = run_helper(input, &[("BUZZ_API_KEY", "secret-api-key-123")]);

    assert!(out.status.success(), "expected exit 0 for non-Buzz remote");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("credential="),
        "must not present the Buzz key to a non-Buzz remote:\n{stdout}"
    );
    assert!(
        !stdout.contains("secret-api-key-123"),
        "the bearer key must never appear for a non-Buzz remote"
    );
}

/// No `wwwauth[]` challenge at all → silent decline (exit 0, no credential).
#[test]
fn absent_challenge_declines() {
    let input = "capability[]=authtype\n\
                 protocol=https\n\
                 host=relay.example.com\n\
                 path=git/owner/repo.git/info/refs\n\
                 \n";

    let out = run_helper(input, &[("BUZZ_API_KEY", "secret-api-key-123")]);

    assert!(
        out.status.success(),
        "expected exit 0 when no challenge present"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("credential="),
        "should not emit a credential without a Buzz bearer challenge"
    );
}

/// Buzz challenge but `BUZZ_API_KEY` unset → exit 1, stderr names the env var.
#[test]
fn missing_api_key_errors() {
    // run_helper clears BUZZ_API_KEY.
    let out = run_helper(&buzz_input(), &[]);

    assert_eq!(
        out.status.code(),
        Some(1),
        "expected exit 1 when BUZZ_API_KEY is unset"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("BUZZ_API_KEY"),
        "expected BUZZ_API_KEY in stderr, got:\n{stderr}"
    );
}

/// `store` and `erase` operations are no-ops (exit 0, no output) — the helper
/// only services `get`.
#[test]
fn store_and_erase_are_noops() {
    for op in ["store", "erase"] {
        let out = run_helper_args(
            &[op],
            &buzz_input(),
            &[("BUZZ_API_KEY", "secret-api-key-123")],
        );
        assert!(out.status.success(), "expected exit 0 for `{op}`");
        assert!(
            out.stdout.is_empty(),
            "`{op}` must produce no output, got:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}
