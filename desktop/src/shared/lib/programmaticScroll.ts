/**
 * Marks a short window during which `scroll` events are known to originate
 * from an app-driven scroll correction (e.g. the message timeline re-pinning
 * to the bottom while a freshly-arrived row's real height settles) rather
 * than direct user input (wheel, trackpad, scrollbar drag, keyboard).
 *
 * Consumers that dismiss transient UI (popovers, context menus) on scroll
 * should treat events inside this window as noise: the viewport is moving
 * because the app moved it, not because the user scrolled away from what
 * they were interacting with.
 */

let activeUntil = 0;

/** Extend the "programmatic scroll" window by `durationMs` from now. */
export function markProgrammaticScroll(durationMs = 100): void {
  activeUntil = performance.now() + durationMs;
}

/** Whether a scroll happening right now is attributable to app-driven scroll. */
export function isProgrammaticScrollActive(): boolean {
  return performance.now() < activeUntil;
}
