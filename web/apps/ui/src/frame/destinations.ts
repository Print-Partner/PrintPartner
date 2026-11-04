/**
 * The five destinations from docs/product/ui-ux-target.md (Information architecture).
 * The rail, the `G` shortcuts, the palette and the shortcut list all read this list.
 */
export interface Destination {
  label: string;
  path: string;
  /** The key pressed after `G`. */
  key: string;
  /** What the screen holds once its slice lands. */
  holds: string;
}

export const DESTINATIONS: readonly Destination[] = [
  {
    label: "Today",
    path: "/today",
    key: "t",
    holds: "the pulse line, the hero task, ranked tasks and open Build cards",
  },
  { label: "Builds", path: "/builds", key: "b", holds: "the Builds list and each Build workspace" },
  { label: "Farm", path: "/farm", key: "f", holds: "printers, the queue and the farm log" },
  {
    label: "Library",
    path: "/library",
    key: "l",
    holds: "Library Sources, import, sync and revisions",
  },
  {
    label: "Settings",
    path: "/settings",
    key: ",",
    holds: "modules, integrations, sharing, devices and backups",
  },
];

/** How a destination's shortcut is written in the UI, for example "G T" or "G ,". */
export const shortcutLabel = (d: Destination) => `G ${d.key.toUpperCase()}`;
