import { relayClient } from "@/shared/api/relayClient";
import { signRelayEvent } from "@/shared/api/tauri";
import type { RelayEvent } from "@/shared/api/types";
import { KIND_CHANNEL_SECTIONS } from "@/shared/constants/kinds";
import {
  parseChannelSectionPayload,
  type ChannelSection,
  type ChannelSectionStore,
} from "./channelSectionsStorage";

const D_TAG = "channel-sections";
const DEBOUNCE_MS = 2_000;

export type RemoteSections = {
  store: ChannelSectionStore;
  createdAt: number;
  eventId: string;
};

// Content is stored as plaintext JSON. Legacy rows written before E2E removal
// hold NIP-44 ciphertext; JSON.parse throws on them and we treat the record as
// absent (returns null) — no decrypt fallback.
function parseRemoteSections(event: RelayEvent): RemoteSections | null {
  try {
    const store = parseChannelSectionPayload(JSON.parse(event.content));
    if (!store) return null;
    return { store, createdAt: event.created_at, eventId: event.id };
  } catch {
    return null;
  }
}

export class ChannelSectionSyncManager {
  private pubkey: string;
  private debounceTimer: number | null = null;
  private lastRemoteCreatedAt = 0;
  private pendingStore: ChannelSectionStore | null = null;
  private lastPublishedStore: ChannelSectionStore | null = null;
  private destroyed = false;

  constructor(pubkey: string) {
    this.pubkey = pubkey;
  }

  async fetchRemoteSections(): Promise<RemoteSections | null> {
    try {
      const events = await relayClient.fetchEvents({
        kinds: [KIND_CHANNEL_SECTIONS],
        authors: [this.pubkey],
        "#d": [D_TAG],
        limit: 1,
      });
      if (events.length === 0) return null;
      if (events[0].pubkey !== this.pubkey) return null;
      const result = parseRemoteSections(events[0]);
      if (result) {
        this.lastRemoteCreatedAt = Math.max(
          this.lastRemoteCreatedAt,
          result.createdAt,
        );
      }
      return result;
    } catch {
      return null;
    }
  }

  cancelPendingPublish(): void {
    if (this.debounceTimer !== null) {
      window.clearTimeout(this.debounceTimer);
      this.debounceTimer = null;
    }
  }

  getPendingStore(): ChannelSectionStore | null {
    return this.pendingStore;
  }

  publishSections(store: ChannelSectionStore): void {
    this.pendingStore = store;
    if (this.debounceTimer !== null) {
      window.clearTimeout(this.debounceTimer);
    }
    this.debounceTimer = window.setTimeout(() => {
      this.debounceTimer = null;
      void this.doPublish(store);
    }, DEBOUNCE_MS);
  }

  private async fetchOwnBlobBeforePublish(
    store: ChannelSectionStore,
  ): Promise<ChannelSectionStore> {
    try {
      const events = await relayClient.fetchEvents({
        kinds: [KIND_CHANNEL_SECTIONS],
        authors: [this.pubkey],
        "#d": [D_TAG],
        limit: 1,
      });
      if (events.length === 0 || events[0].pubkey !== this.pubkey) return store;
      const remote = parseRemoteSections(events[0]);
      if (!remote) return store;
      // Sections use whole-blob LWW: take whichever is newer
      if (remote.createdAt > this.lastRemoteCreatedAt) {
        this.lastRemoteCreatedAt = remote.createdAt;
        return remote.store;
      }
      return store;
    } catch {
      return store;
    }
  }

  private isIdenticalToLastPublished(store: ChannelSectionStore): boolean {
    if (!this.lastPublishedStore) return false;
    const lastSections = this.lastPublishedStore.sections;
    const currentSections = store.sections;
    if (lastSections.length !== currentSections.length) return false;
    for (let i = 0; i < currentSections.length; i++) {
      const last = lastSections[i] as ChannelSection | undefined;
      const current = currentSections[i] as ChannelSection;
      if (
        !last ||
        last.id !== current.id ||
        last.name !== current.name ||
        last.icon !== current.icon ||
        last.order !== current.order
      )
        return false;
    }
    const lastAssignKeys = Object.keys(this.lastPublishedStore.assignments);
    const currentAssignKeys = Object.keys(store.assignments);
    if (lastAssignKeys.length !== currentAssignKeys.length) return false;
    for (const key of currentAssignKeys) {
      if (this.lastPublishedStore.assignments[key] !== store.assignments[key])
        return false;
    }
    return true;
  }

  private async doPublish(store: ChannelSectionStore): Promise<void> {
    try {
      const merged = await this.fetchOwnBlobBeforePublish(store);
      // Guard: manager may have been destroyed while fetchOwnBlobBeforePublish
      // was awaited (community switch during in-flight fetch). If so, abort
      // before touching the relay.
      if (this.destroyed) return;
      if (this.isIdenticalToLastPublished(merged)) {
        this.pendingStore = null;
        return;
      }
      const payload = {
        version: 1,
        sections: merged.sections,
        assignments: merged.assignments,
      };
      const content = JSON.stringify(payload);
      const createdAt = Math.max(
        Math.floor(Date.now() / 1_000),
        this.lastRemoteCreatedAt + 1,
      );
      const event = await signRelayEvent({
        kind: KIND_CHANNEL_SECTIONS,
        content,
        createdAt,
        tags: [
          ["d", D_TAG],
          ["t", D_TAG], // relay discoverability; not used in our filters
        ],
      });
      // Final guard immediately before the network call — sign/encrypt are
      // synchronous-ish but cheap; the relay socket may have moved to a
      // different community by the time we reach this point.
      if (this.destroyed) return;
      await relayClient.publishEvent(
        event,
        "Timed out publishing channel sections.",
        "Failed to publish channel sections.",
      );
      this.lastRemoteCreatedAt = Math.max(
        this.lastRemoteCreatedAt,
        event.created_at,
      );
      this.lastPublishedStore = merged;
      this.pendingStore = null;
    } catch (error) {
      console.warn("[channelSectionsSync] publish failed:", error);
    }
  }

  async subscribeToSections(
    onUpdate: (remote: RemoteSections) => void,
  ): Promise<() => Promise<void>> {
    return relayClient.subscribeLive(
      {
        kinds: [KIND_CHANNEL_SECTIONS],
        authors: [this.pubkey],
        "#d": [D_TAG],
        limit: 0,
      },
      (event: RelayEvent) => {
        if (event.pubkey !== this.pubkey) return;
        const result = parseRemoteSections(event);
        if (result) {
          this.lastRemoteCreatedAt = Math.max(
            this.lastRemoteCreatedAt,
            result.createdAt,
          );
          onUpdate(result);
        }
      },
    );
  }

  destroy(): void {
    // Cancel any pending publish and mark this manager as destroyed so any
    // in-flight doPublish() calls abort before reaching relayClient. The
    // scoped localStorage write is already durable; when the user returns to
    // this relay the existing seed-publish guard will re-publish from local
    // state. Flushing here would race against community switching and could
    // publish relay A's sections to relay B via the shared relayClient
    // singleton.
    this.destroyed = true;
    this.cancelPendingPublish();
    this.pendingStore = null;
  }
}
