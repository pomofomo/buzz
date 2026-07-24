import * as React from "react";
import { createPortal } from "react-dom";

import { cn } from "@/shared/lib/cn";
import { isProgrammaticScrollActive } from "@/shared/lib/programmaticScroll";
import {
  POPOVER_CUSTOM_ENTER_MOTION_CLASS,
  POPOVER_SHADOW_STYLE,
  POPOVER_SURFACE_CLASS,
} from "@/shared/ui/popoverSurface";

export type MediaContextMenuPosition = {
  x: number;
  y: number;
};

export type MediaContextMenuItem = {
  label: string;
  onSelect: () => void;
};

/**
 * Dismiss listeners for a custom right-click menu.
 *
 * Deferring attachment until after the current event-loop turn matters: the
 * right-click that opens the menu (a `contextmenu` on mousedown) is often
 * followed by a trailing `click`/`pointerup` on the same interaction — and
 * some webviews emit a platform `click` on right-button release. Attaching
 * synchronously lets that trailing event immediately dismiss the menu, so it
 * only flashes. Deferring guarantees the opening interaction can never be the
 * one that closes it.
 *
 * `scroll` dismissal skips app-driven scrolls: the timeline keeps a
 * freshly-arrived message pinned to the bottom for a short window after it
 * mounts (`useVirtualizedBottomSettle`), re-running `scrollToIndex` for a few
 * frames while the row's real height settles (e.g. once a video/poster
 * finishes loading) — which can be a substantial jump, not just sub-pixel
 * noise. That's the app moving the viewport, not the user scrolling away, so
 * it should never win a race against a right-click menu opened on the very
 * row that just arrived. `markProgrammaticScroll` flags that window
 * (`programmaticScroll.ts`); only scrolls outside it close the menu.
 */
export function useDismissMediaContextMenu(
  isOpen: boolean,
  onDismiss: () => void,
) {
  React.useEffect(() => {
    if (!isOpen) return;
    let attached = false;
    const handleScroll = () => {
      if (isProgrammaticScrollActive()) return;
      onDismiss();
    };
    const timer = window.setTimeout(() => {
      attached = true;
      window.addEventListener("click", onDismiss);
      window.addEventListener("contextmenu", onDismiss);
      window.addEventListener("scroll", handleScroll, true);
    }, 0);
    return () => {
      window.clearTimeout(timer);
      if (attached) {
        window.removeEventListener("click", onDismiss);
        window.removeEventListener("contextmenu", onDismiss);
        window.removeEventListener("scroll", handleScroll, true);
      }
    };
  }, [isOpen, onDismiss]);
}

/**
 * A small right-click menu for media and links, positioned at the pointer.
 *
 * Rendered into `portalContainer` (defaults to `document.body`) so it escapes
 * overflow-clipped ancestors. Callers own open/close state and must pair this
 * with `useDismissMediaContextMenu`.
 */
export function MediaContextMenu({
  dataAttributes,
  items,
  portalContainer,
  position,
}: {
  /**
   * Optional empty-valued data attributes (e.g. `["data-image-context-menu"]`)
   * set on the menu root so e2e locators can target a specific menu variant —
   * image, link, and video menus pass distinct attributes so tests never alias
   * one surface for another. The image variant also passes
   * `data-image-lightbox-controls` so the in-lightbox dismiss guard treats its
   * own menu as an interior control. All menus additionally carry the shared
   * generic `data-media-context-menu` marker.
   */
  dataAttributes?: string[];
  items: MediaContextMenuItem[];
  portalContainer?: Element;
  position: MediaContextMenuPosition;
}) {
  const itemClass =
    "flex min-h-9 w-full cursor-default select-none items-center rounded-lg py-2 pl-2 pr-4 text-sm outline-hidden hover:bg-muted/50 hover:text-foreground";
  return createPortal(
    <div
      className={cn(
        "fixed z-[100] min-w-60 origin-top-left rounded-xl p-1 slide-in-from-top-1",
        POPOVER_CUSTOM_ENTER_MOTION_CLASS,
        POPOVER_SURFACE_CLASS,
      )}
      data-media-context-menu=""
      {...Object.fromEntries(
        (dataAttributes ?? []).map((attribute) => [attribute, ""]),
      )}
      style={{ ...POPOVER_SHADOW_STYLE, left: position.x, top: position.y }}
    >
      {items.map((item) => (
        <button
          className={itemClass}
          key={item.label}
          onClick={item.onSelect}
          type="button"
        >
          {item.label}
        </button>
      ))}
    </div>,
    portalContainer ?? document.body,
  );
}
