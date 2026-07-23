# GREEN GATE — `claude/nostr-api-keys-refactor-06pyto`

Single source of truth for the "is the tree green?" check during the
Nostr → API-key refactor. **Run this list before AND after any change; every
command must pass.** If a command that passed before your change now fails, you
broke the gate — fix it before pushing.

This gate reconciles with the `Justfile` `ci` / `check` / `test-unit` recipes,
reduced to what is actually runnable in the CI sandbox (see "Environment
limitations" below).

---

## The gate

Prefix Rust commands with the Hermit toolchain **or** use the system cargo
fallback. Both are shown once; the rest assume one of them is active:

```bash
# Option A — Hermit (may 403 on the proxy for toolchain fetch):
. ./bin/activate-hermit

# Option B — system cargo fallback (>=1.88; used to validate this gate):
export PATH="/root/.cargo/bin:$PATH"
```

### Workspace (root; desktop crate is excluded from the root workspace)

```bash
cargo fmt --all -- --check                       # formatting
cargo clippy --workspace --all-targets           # zero warnings
cargo build --workspace                          # compiles
cargo test -p buzz-core -p buzz-auth --lib       # unit tests, no infra
cargo test -p buzz-db --lib                      # unit tests (see infra note)
cargo test -p buzz-conformance                   # multi-tenant conformance
```

### Desktop Tauri crate (separate manifest, excluded from root workspace)

```bash
cargo check   --manifest-path desktop/src-tauri/Cargo.toml
cargo clippy  --manifest-path desktop/src-tauri/Cargo.toml --all-targets
cargo fmt     --manifest-path desktop/src-tauri/Cargo.toml --all -- --check
cargo test    --manifest-path desktop/src-tauri/Cargo.toml
```

> Run the desktop-tauri `fmt` from the **main checkout**, not a git worktree —
> `cargo fmt` resolves workspace paths relative to the worktree root and fails
> in worktrees (known gotcha; CI is unaffected).

---

## Prerequisites (once per fresh environment)

The desktop Tauri crate needs native system libraries and sidecar placeholder
binaries before it will `check`/`clippy`/`build`:

```bash
# 1. Native GTK / WebKit / ALSA dev libraries (Linux):
apt-get update && apt-get install -y \
  libgtk-3-dev libwebkit2gtk-4.1-dev libsoup-3.0-dev libasound2-dev

# 2. Sidecar placeholder binaries (Tauri validates externalBin at compile time).
#    Mirrors the Justfile `_ensure-sidecar-stubs` recipe:
TARGET=$(rustc -vV | sed -n 's|host: ||p')
mkdir -p desktop/src-tauri/binaries
for bin in buzz-acp buzz-agent buzz-dev-mcp git-credential-nostr buzz; do
  touch "desktop/src-tauri/binaries/${bin}-${TARGET}"
done
```

---

## Environment limitations (this sandbox)

Two items in the desktop gate are **blocked by the sandbox**, not by the code.
Downstream lanes running in a fully provisioned CI do not hit these; document
them so a red result here is understood correctly:

1. **`sherpa-onnx-sys` prebuilt native lib download is egress-blocked.**
   The build script downloads
   `github.com/k2-fsa/sherpa-onnx/releases/...` which the agent proxy denies
   with `403` (repo not enabled for this session's egress — a policy denial, do
   not route around it). Consequences:
   - `cargo check` / `clippy` / `fmt` on the desktop crate **do pass** here by
     setting `DOCS_RS=1`, which makes the `sherpa-onnx-sys` build script skip
     the native download (check/clippy do not link):
     ```bash
     DOCS_RS=1 cargo check  --manifest-path desktop/src-tauri/Cargo.toml
     DOCS_RS=1 cargo clippy --manifest-path desktop/src-tauri/Cargo.toml --all-targets
     ```
   - `cargo test` / `cargo build` on the desktop crate **cannot link** here:
     with `DOCS_RS=1` the sherpa native symbols are undefined at link time, and
     without it the native archive is un-fetchable. A provisioned CI (or a local
     `SHERPA_ONNX_LIB_DIR` / `SHERPA_ONNX_ARCHIVE_DIR`) links normally. In a
     normal environment run the plain commands above (no `DOCS_RS`).

2. **Mobile (Flutter/Dart) and the full `pnpm` JS/desktop-frontend CI are not
   runnable here.** `flutter`/`dart` are not installed, and
   `desktop/node_modules` + `web/node_modules` are not installed (a `pnpm
   install` in those dirs may fail on the proxy). These steps are **env-blocked
   in this sandbox** and must be validated in a provisioned environment:
   - `just mobile-check` / `mobile-test` (Flutter)
   - `pnpm check` / `pnpm build` / `pnpm typecheck` (desktop + web)
   - Desktop/web Playwright E2E.

## Infra note

`cargo test -p buzz-db --lib` and the DB-dependent integration suites need a
running Postgres + Redis. In this sandbox they are available at
`postgres://buzz:buzz_dev@localhost:5432/buzz` and `redis://localhost:6379`.
Pure unit tests (`buzz-core`, `buzz-auth`) need no infrastructure.
