import { expect, test } from "@playwright/test";

import { installMockBridge } from "../helpers/bridge";

/**
 * Coverage for the API-key bearer auth doorway added in Lane I-a
 * (REFACTOR.md / AGENTS.md "Active migration: Nostr substrate → API-key
 * auth"). `getAuthMode()`/`getApiKey()` (src/shared/api/authMode.ts) decide,
 * per `relayClientSession.connect()`, whether the WebSocket upgrade carries
 * an `Authorization: Bearer <token>` header (apikey mode, no NIP-42
 * challenge/response) or falls back to the legacy signed `AUTH` handshake
 * (nostr mode, the default).
 *
 * The mock bridge (`src/testing/e2eBridge.ts`) mirrors both branches:
 * `mock.authMode`/`mock.apiKey` drive `get_auth_mode`/`get_api_key`, and
 * `connectMockSocket` only sends the unsolicited `AUTH` challenge when the
 * connect payload carries no `authorization` — matching the real relay,
 * which never issues a NIP-42 challenge to a bearer-authenticated upgrade.
 */

type ConnectionState =
  | "idle"
  | "connecting"
  | "connected"
  | "reconnecting"
  | "stalled"
  | "disconnected";

async function waitForConnected(page: import("@playwright/test").Page) {
  await page.waitForFunction(() => {
    const win = window as Window & {
      __BUZZ_E2E_GET_RELAY_CONNECTION_STATE__?: () => ConnectionState;
    };
    return win.__BUZZ_E2E_GET_RELAY_CONNECTION_STATE__?.() === "connected";
  });
}

async function getWebsocketConnectPayloads(
  page: import("@playwright/test").Page,
) {
  return page.evaluate(() => {
    const win = window as Window & {
      __BUZZ_E2E_COMMAND_PAYLOADS__?: Array<{
        command: string;
        payload: unknown;
      }>;
    };
    return (win.__BUZZ_E2E_COMMAND_PAYLOADS__ ?? []).filter(
      (entry) => entry.command === "plugin:websocket|connect",
    );
  });
}

async function getInvokedCommands(page: import("@playwright/test").Page) {
  return page.evaluate(
    () =>
      (window as Window & { __BUZZ_E2E_COMMANDS__?: string[] })
        .__BUZZ_E2E_COMMANDS__ ?? [],
  );
}

test("apikey auth mode attaches the bearer header and skips the NIP-42 challenge", async ({
  page,
}) => {
  const token = "buzz_test_bearer_token_123";
  await installMockBridge(page, { authMode: "apikey", apiKey: token });

  await page.goto("/");
  await expect(page.getByTestId("app-sidebar")).toBeVisible();
  await waitForConnected(page);

  const connectCalls = await getWebsocketConnectPayloads(page);
  expect(connectCalls.length).toBeGreaterThan(0);
  for (const call of connectCalls) {
    const payload = call.payload as { config?: { authorization?: string } };
    expect(payload.config?.authorization).toBe(`Bearer ${token}`);
  }

  // No NIP-42 signed challenge/response in apikey mode — the relay
  // authenticated the connection at the WS upgrade.
  const commands = await getInvokedCommands(page);
  expect(commands).not.toContain("create_auth_event");
});

test("nostr auth mode (default) still signs the NIP-42 AUTH challenge", async ({
  page,
}) => {
  await installMockBridge(page);

  await page.goto("/");
  await expect(page.getByTestId("app-sidebar")).toBeVisible();
  await waitForConnected(page);

  const connectCalls = await getWebsocketConnectPayloads(page);
  expect(connectCalls.length).toBeGreaterThan(0);
  for (const call of connectCalls) {
    const payload = call.payload as { config?: { authorization?: string } };
    expect(payload.config?.authorization).toBeUndefined();
  }

  await page.waitForFunction(() =>
    (
      window as Window & { __BUZZ_E2E_COMMANDS__?: string[] }
    ).__BUZZ_E2E_COMMANDS__?.includes("create_auth_event"),
  );
  const commands = await getInvokedCommands(page);
  expect(commands).toContain("create_auth_event");
});
