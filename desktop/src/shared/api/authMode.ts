import { invokeTauri } from "@/shared/api/tauri";

/**
 * Relay authentication doorway.
 *
 * - `"apikey"` — bearer token attached at the WebSocket upgrade and on HTTP
 *   bridge calls; the server authors rows from the resolved actor (no
 *   client-side signing / NIP-42 / NIP-98).
 * - `"nostr"` — legacy NIP-42 challenge/response over WS and NIP-98 signed
 *   headers over HTTP.
 *
 * The active mode is decided by the Tauri backend (`get_auth_mode`): the
 * `BUZZ_AUTH_MODE` env var is authoritative when set, otherwise the mode is
 * `"apikey"` whenever an API bearer token is configured (env override or OS
 * keyring) and `"nostr"` otherwise.
 */
export type AuthMode = "apikey" | "nostr";

let cachedMode: AuthMode | null = null;
let cachedModePromise: Promise<AuthMode> | null = null;

/**
 * Resolve and cache the active relay auth mode. The value is stable for a
 * process/community session; call {@link resetAuthModeCache} on community
 * switch so a relay with a different auth posture is re-probed.
 */
export async function getAuthMode(): Promise<AuthMode> {
  if (cachedMode) {
    return cachedMode;
  }
  if (!cachedModePromise) {
    cachedModePromise = invokeTauri<string>("get_auth_mode")
      .then((mode) => {
        cachedMode = mode === "apikey" ? "apikey" : "nostr";
        return cachedMode;
      })
      .catch(() => {
        // Fail safe to the legacy Nostr doorway if the command is unavailable.
        cachedMode = "nostr";
        return cachedMode;
      })
      .finally(() => {
        cachedModePromise = null;
      });
  }
  return cachedModePromise;
}

/**
 * Return the configured API bearer token, or `null` when none is set. Used to
 * build the `Authorization: Bearer <token>` header for the WS upgrade in
 * `apikey` mode.
 */
export async function getApiKey(): Promise<string | null> {
  try {
    return (await invokeTauri<string | null>("get_api_key")) ?? null;
  } catch {
    return null;
  }
}

/**
 * Clear the cached auth mode. Wire into `resetCommunityState()` so switching to
 * a relay with a different auth mode re-probes the backend.
 */
export function resetAuthModeCache(): void {
  cachedMode = null;
  cachedModePromise = null;
}
