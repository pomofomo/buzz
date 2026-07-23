/**
 * Web auth doorway selection.
 *
 * The browser client authenticates either with a bearer API token (`apikey`
 * mode) or the legacy Nostr signing path (NIP-07 / ephemeral key + NIP-42 /
 * NIP-98). Because a browser `WebSocket` cannot set an `Authorization` header
 * on the upgrade, bearer-mode reads use the HTTP `POST /query` bridge (which
 * accepts `Authorization: Bearer <token>`) instead of the WS REQ path.
 *
 * The bearer token is sourced from (in order):
 *  - `window.__BUZZ_API_KEY__` (host-injected), or
 *  - `localStorage["buzz_api_key"]`.
 *
 * When no token is present the client stays on the Nostr doorway, so existing
 * open-relay browsing keeps working unchanged.
 */

const LOCAL_STORAGE_KEY = "buzz_api_key";

declare global {
  interface Window {
    __BUZZ_API_KEY__?: string;
  }
}

/** Return the configured bearer API token, or `null` when none is set. */
export function getBearerToken(): string | null {
  if (typeof window === "undefined") {
    return null;
  }
  const injected = window.__BUZZ_API_KEY__;
  if (typeof injected === "string" && injected.trim() !== "") {
    return injected.trim();
  }
  try {
    const stored = window.localStorage?.getItem(LOCAL_STORAGE_KEY);
    if (stored && stored.trim() !== "") {
      return stored.trim();
    }
  } catch {
    // localStorage may be unavailable (private mode / sandbox) — ignore.
  }
  return null;
}

/** Whether the client should use the bearer (`apikey`) doorway. */
export function isBearerMode(): boolean {
  return getBearerToken() !== null;
}
