use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Session-scoped shim directory providing tools and git config to shell children.
///
/// On install:
/// 1. Creates a 0700 tempdir with symlinks back to our binary (multicall)
/// 2. Builds ephemeral `GIT_CONFIG_*` env vars wiring git to the bearer
///    credential helper (`git-credential-nostr`, name retained), which reads the
///    API key from `BUZZ_API_KEY` in the child's environment
/// 3. Removes the obsolete `NOSTR_PRIVATE_KEY` from the process env so it can
///    never leak to children (bearer auth does not use a Nostr private key)
/// 4. Prepends the shim dir to PATH
///
/// Shell children receive `path_env`, `git_env`, and the inherited process
/// environment (including `BUZZ_API_KEY`, from which the credential helper mints
/// `Authorization: Bearer <key>`). Cleaned up on drop (TempDir).
pub struct Shim {
    _dir: TempDir,
    pub path_env: String,
    pub git_env: Vec<(String, String)>,
}

impl Shim {
    pub fn install() -> std::io::Result<Self> {
        let dir = tempfile::Builder::new().prefix("buzz-dev-mcp-").tempdir()?;
        set_owner_only(dir.path())?;

        let self_exe = std::env::current_exe()?;

        // Multicall symlinks — all resolve back to this binary.
        for name in ["rg", "tree", "buzz", "git-credential-nostr"] {
            symlink(&self_exe, &dir.path().join(name))?;
        }

        let original = std::env::var_os("PATH").unwrap_or_default();
        let mut entries = vec![PathBuf::from(dir.path())];
        entries.extend(std::env::split_paths(&original));
        // join_paths uses the platform separator (':' on Unix, ';' on Windows).
        let path_env = std::env::join_paths(entries)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?
            .to_string_lossy()
            .into_owned();

        // Bearer auth does not use a Nostr private key. Scrub the obsolete
        // `NOSTR_PRIVATE_KEY` from this process's env so it can never leak to
        // child processes; `BUZZ_API_KEY` is inherited normally and read by the
        // credential helper at request time.
        std::env::remove_var("NOSTR_PRIVATE_KEY");

        // Ephemeral git config wiring git to the bearer credential helper.
        let git_env = build_git_env();

        Ok(Self {
            _dir: dir,
            path_env,
            git_env,
        })
    }
}

/// Derive a git commit-author email from the configured relay host.
/// Format: `agent@<relay_host>` (e.g., `agent@relay.buzz.dev`). Falls back to
/// `agent@buzz` if no usable relay URL is configured.
///
/// Commit identity is advisory only under the bearer model — the server no
/// longer verifies commit signatures — so a stable, non-secret address suffices.
fn derive_git_email() -> String {
    let host = std::env::var("BUZZ_RELAY_URL")
        .ok()
        .and_then(|url| {
            // Strip scheme, port, and trailing paths
            let stripped = url
                .strip_prefix("https://")
                .or_else(|| url.strip_prefix("http://"))
                .or_else(|| url.strip_prefix("wss://"))
                .or_else(|| url.strip_prefix("ws://"))
                .unwrap_or(&url);
            let host_port = stripped.split('/').next()?;
            // Strip port number (e.g., "localhost:3000" → "localhost")
            Some(host_port.split(':').next().unwrap_or(host_port).to_owned())
        })
        .filter(|h| !h.is_empty() && !h.starts_with("localhost") && !h.starts_with("127."))
        .unwrap_or_else(|| "buzz".to_owned());
    format!("agent@{host}")
}

/// Build GIT_CONFIG_COUNT/KEY/VALUE env vars wiring git to the bearer credential
/// helper. Composes with any existing GIT_CONFIG_COUNT in the environment. When
/// launched via buzz-agent (which clears env), the base is always 0 — composition
/// only matters when dev-mcp is run directly with pre-existing GIT_CONFIG vars.
///
/// No commit/tag signing is configured: git object signing was advisory-only and
/// never verified server-side, so it is dropped under the API-key model.
fn build_git_env() -> Vec<(String, String)> {
    let email = derive_git_email();
    let entries: Vec<(&str, String)> = vec![
        // Advisory commit identity (not verified server-side under bearer auth).
        ("user.name", "buzz-agent".into()),
        ("user.email", email),
        // Bearer credential helper (binary name retained). It mints
        // `Authorization: Bearer <BUZZ_API_KEY>` for Buzz remotes and silently
        // declines non-Buzz remotes (exits 0, no credential), so git falls through
        // to system helpers (osxkeychain, store, etc.) for GitHub/GitLab/etc.
        ("credential.helper", "nostr".into()),
        // Pass the full repo path to the helper so it can scope the credential to
        // the Buzz repo-root URL rather than the bare host.
        ("credential.useHttpPath", "true".into()),
    ];

    // Compose with existing GIT_CONFIG_COUNT — don't clobber caller's config.
    let base: usize = std::env::var("GIT_CONFIG_COUNT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let mut env = Vec::with_capacity(entries.len() * 2 + 1);
    env.push((
        "GIT_CONFIG_COUNT".into(),
        (base + entries.len()).to_string(),
    ));
    for (i, (key, val)) in entries.iter().enumerate() {
        let idx = base + i;
        env.push((format!("GIT_CONFIG_KEY_{idx}"), key.to_string()));
        env.push((format!("GIT_CONFIG_VALUE_{idx}"), val.to_string()));
    }
    env
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn set_owner_only(_: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(not(unix))]
fn symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    // No symlinks without elevation on Windows; copy instead. The target needs
    // a .exe extension or PATH lookup (via PATHEXT) won't treat it as runnable.
    let dst = dst.with_extension("exe");
    std::fs::copy(src, dst).map(|_| ())
}

pub fn artifact_dir(session_root: &Path) -> PathBuf {
    let p = session_root.join("artifacts");
    let _ = std::fs::create_dir_all(&p);
    p
}
