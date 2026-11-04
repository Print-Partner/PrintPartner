import { useEffect, useRef, useState } from "react";
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

const CHARACTER_KEYS_STORAGE = "pp.characterShortcuts";

/**
 * Whether single-key shortcuts (`G` sequences and `?`) are on. WCAG 2.1.4 asks for a way
 * to turn them off, since speech input can trigger them. Saved per viewer; storage that
 * throws (private windows, blocked site data) leaves them on for the session.
 */
export function useCharacterShortcuts(): [boolean, (on: boolean) => void] {
  const [on, setOn] = useState(() => {
    try {
      return localStorage.getItem(CHARACTER_KEYS_STORAGE) !== "off";
    } catch {
      return true;
    }
  });
  const set = (next: boolean) => {
    setOn(next);
    try {
      localStorage.setItem(CHARACTER_KEYS_STORAGE, next ? "on" : "off");
    } catch {
      // Not saved; the choice lasts until reload.
    }
  };
  return [on, set];
}

/**
 * Global keys: Cmd/Ctrl+K toggles the palette anywhere. When single-key shortcuts are
 * on, no dialog is open and focus is not in a text field, `?` opens the shortcut list
 * and `G` then a destination key goes there.
 */
export function useShortcuts({
  characterKeys,
  dialogOpen,
  togglePalette,
  openHelp,
  go,
}: {
  characterKeys: boolean;
  dialogOpen: boolean;
  togglePalette: () => void;
  openHelp: () => void;
  go: (path: string) => void;
}) {
  const latest = useRef({ characterKeys, dialogOpen, togglePalette, openHelp, go });
  useEffect(() => {
    latest.current = { characterKeys, dialogOpen, togglePalette, openHelp, go };
  });

  useEffect(() => {
    let pendingSince: number | null = null;
    function onKeyDown(event: KeyboardEvent) {
      const { characterKeys, dialogOpen, togglePalette, openHelp, go } = latest.current;
      if (event.defaultPrevented || event.isComposing) return;
      if ((event.metaKey || event.ctrlKey) && !event.altKey && event.key.toLowerCase() === "k") {
        event.preventDefault();
        pendingSince = null;
        togglePalette();
        return;
      }
      if (!characterKeys || dialogOpen || isEditable(event.target)) return;
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
