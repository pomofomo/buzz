import type { RelayEvent } from "@/shared/api/types";
import {
  isValidBlob,
  isValidReadStateDTag,
  sanitizeContexts,
  type ReadStateBlob,
} from "@/features/channels/readState/readStateFormat";

export type ParsedReadStateEvent = {
  dTag: string;
  blob: ReadStateBlob;
  createdAt: number;
};

// Read-state blobs are stored as plaintext JSON. Legacy rows written before
// E2E removal hold NIP-44 ciphertext; JSON.parse throws on them and the record
// is treated as absent (returns null) — there is no decrypt fallback.
export function parseReadStateEvent(
  event: RelayEvent,
  pubkey: string,
): ParsedReadStateEvent | null {
  if (event.pubkey !== pubkey) return null;

  const dTags = event.tags.filter((tag) => tag[0] === "d");
  if (dTags.length !== 1) return null;
  const dTag = dTags[0]?.[1];
  if (!isValidReadStateDTag(dTag)) return null;

  const tTags = event.tags.filter(
    (tag) => tag[0] === "t" && tag[1] === "read-state",
  );
  if (tTags.length !== 1) return null;

  try {
    const parsed = JSON.parse(event.content);
    if (!isValidBlob(parsed)) return null;
    return {
      dTag,
      blob: {
        v: 1,
        client_id: parsed.client_id,
        contexts: sanitizeContexts(parsed.contexts),
      },
      createdAt: event.created_at,
    };
  } catch (error) {
    console.debug(
      `[ReadStateSnapshot] parse failed event=${event.id.substring(0, 8)}…:`,
      error,
    );
    return null;
  }
}

export function mergeReadStateEvents(
  events: RelayEvent[],
  pubkey: string,
): Map<string, number> {
  const contexts = new Map<string, number>();

  for (const event of events) {
    const parsed = parseReadStateEvent(event, pubkey);
    if (!parsed) continue;

    for (const [contextId, timestamp] of Object.entries(parsed.blob.contexts)) {
      const current = contexts.get(contextId) ?? 0;
      if (timestamp > current) {
        contexts.set(contextId, timestamp);
      }
    }
  }

  return contexts;
}

export function getSnapshotReadTimestamp(
  contexts: ReadonlyMap<string, number>,
  contextId: string,
): number | null {
  return contexts.get(contextId) ?? null;
}
