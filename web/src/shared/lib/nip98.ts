/**
 * HTTP Auth header helper for relay requests (used by isomorphic-git for smart
 * HTTP transport and the invite API).
 *
 * In `apikey` mode this returns `Authorization: Bearer <token>` — the server
 * resolves the actor + scopes from the token and authors any resulting row. In
 * `nostr` mode it signs a NIP-98 kind:27235 event as before. The function name
 * is retained so callers need no change; only the produced header differs.
 */

import { getBearerToken } from "./auth-mode";
import { signNostrEvent } from "./nostr-signer";

async function sha256Hex(value: string): Promise<string> {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(value),
  );
  return Array.from(new Uint8Array(digest))
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}

/**
 * Build a NIP-98 Authorization header value.
 *
 * Signed POST bodies include the payload digest required by invite endpoints.
 */
export async function makeNip98AuthHeader(
  url: string,
  method: string,
  options?: { body?: string; requireNip07?: boolean },
): Promise<string> {
  // apikey mode: present the bearer token; no per-request signing/nonce needed.
  const bearer = getBearerToken();
  if (bearer) {
    return `Bearer ${bearer}`;
  }

  const tags = [
    ["u", url],
    ["method", method],
  ];
  if (options?.body !== undefined) {
    tags.push(["payload", await sha256Hex(options.body)]);
    tags.push(["nonce", crypto.randomUUID()]);
  }
  const event = await signNostrEvent(
    {
      kind: 27235,
      tags,
      content: "",
    },
    { requireNip07: options?.requireNip07 },
  );

  const json = JSON.stringify(event);
  const base64 = btoa(json);
  return `Nostr ${base64}`;
}
