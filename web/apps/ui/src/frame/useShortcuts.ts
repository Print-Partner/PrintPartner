import { useEffect, useRef } from "react";
import { DESTINATIONS } from "./destinations";

/** How long after `G` the second key still counts. */
export const SEQUENCE_MS = 1500;

function isEditable(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  return (
    target.isContentEditable ||
    target instanceof HTMLInputElement ||
    target instanceof HTMLTextAreaElement ||
    target instanceof HTMLSelectElement
  );
}

/**
 * Global keys: Cmd/Ctrl+K toggles the palette anywhere. While no dialog is open and
 * focus is not in a text field, `?` opens the shortcut list and `G` then a
 * destination key goes there.
 */
export function useShortcuts({
  dialogOpen,
  togglePalette,
  openHelp,
  go,
}: {
  dialogOpen: boolean;
  togglePalette: () => void;
  openHelp: () => void;
  go: (path: string) => void;
}) {
  const latest = useRef({ dialogOpen, togglePalette, openHelp, go });
  useEffect(() => {
    latest.current = { dialogOpen, togglePalette, openHelp, go };
  });

  useEffect(() => {
    let pendingSince: number | null = null;
    function onKeyDown(event: KeyboardEvent) {
      const { dialogOpen, togglePalette, openHelp, go } = latest.current;
      if (event.defaultPrevented || event.isComposing) return;
      if ((event.metaKey || event.ctrlKey) && !event.altKey && event.key.toLowerCase() === "k") {
        event.preventDefault();
        pendingSince = null;
        togglePalette();
        return;
      }
      if (dialogOpen || isEditable(event.target)) return;
      if (event.metaKey || event.ctrlKey || event.altKey) return;

      const pending = pendingSince !== null && performance.now() - pendingSince <= SEQUENCE_MS;
      pendingSince = null;
      if (pending) {
        const target = DESTINATIONS.find((d) => d.key === event.key.toLowerCase());
        if (target) {
          event.preventDefault();
          go(target.path);
          return;
        }
      }
      if (event.key === "?") {
        event.preventDefault();
        openHelp();
      } else if (event.key.toLowerCase() === "g") {
        pendingSince = performance.now();
      }
    }
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, []);
}
