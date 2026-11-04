import { useState } from "react";
import { NavLink, Outlet, useNavigate } from "react-router";
import { CommandPalette, type Command } from "./CommandPalette";
import { ConnectionProvider, useConnection } from "./connection";
import { DESTINATIONS, shortcutLabel } from "./destinations";
import { Dialog } from "./Dialog";
import { useShortcuts } from "./useShortcuts";

export const GLOBAL_SHORTCUTS: readonly { keys: string; action: string }[] = [
  ...DESTINATIONS.map((d) => ({ keys: shortcutLabel(d), action: `Go to ${d.label}` })),
  { keys: "Cmd/Ctrl K", action: "Open or close the command palette" },
  { keys: "?", action: "Show keyboard shortcuts" },
  { keys: "Esc", action: "Close the open dialog" },
];

function ShortcutList({ onClose }: { onClose: () => void }) {
  return (
    <Dialog title="Keyboard shortcuts" onClose={onClose}>
      <table className="pp-shortcut-table">
        <thead>
          <tr>
            <th scope="col">Keys</th>
            <th scope="col">Action</th>
          </tr>
        </thead>
        <tbody>
          {GLOBAL_SHORTCUTS.map((s) => (
            <tr key={s.keys}>
              <td>
                <kbd className="pp-kbd">{s.keys}</kbd>
              </td>
              <td>{s.action}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <button type="button" data-autofocus className="pp-button" onClick={onClose}>
        Close
      </button>
    </Dialog>
  );
}

function ConnectionLine() {
  const status = useConnection();
  const label = { checking: "Checking core", online: "Core reachable", offline: "Offline" }[status];
  return (
    <p role="status" className="pp-connection" data-status={status}>
      {label}
    </p>
  );
}

/**
 * The pulse line slot: printer counts by state, on every screen. The counts arrive
 * with the Farm slice; until then the slot says so instead of showing zeros.
 */
function PulseLine() {
  return (
    <section aria-label="Printer pulse" className="pp-pulse">
      <span className="pp-pulse-empty">Printer counts not loaded</span>
    </section>
  );
}

type Overlay = "palette" | "help" | null;

function FrameBody() {
  const navigate = useNavigate();
  const [overlay, setOverlay] = useState<Overlay>(null);
  const close = () => setOverlay(null);

  useShortcuts({
    dialogOpen: overlay !== null,
    togglePalette: () => setOverlay((o) => (o === "palette" ? null : "palette")),
    openHelp: () => setOverlay("help"),
    go: (path) => void navigate(path),
  });

  const commands: Command[] = [
    ...DESTINATIONS.map((d) => ({
      id: d.path.slice(1),
      label: `Go to ${d.label}`,
      shortcut: shortcutLabel(d),
      run: () => void navigate(d.path),
    })),
    {
      id: "shortcuts",
      label: "Show keyboard shortcuts",
      shortcut: "?",
      run: () => setOverlay("help"),
    },
  ];

  return (
    <div className="pp-frame">
      <a className="pp-skip" href="#main">
        Skip to content
      </a>
      <header className="pp-header">
        <span className="pp-name">Print Partner</span>
        <PulseLine />
        <ConnectionLine />
        <button
          type="button"
          className="pp-header-button"
          aria-haspopup="dialog"
          onClick={() => setOverlay("palette")}
        >
          Commands <kbd className="pp-kbd">Cmd/Ctrl K</kbd>
        </button>
      </header>
      <nav aria-label="Main" className="pp-rail">
        <ul>
          {DESTINATIONS.map((d) => (
            <li key={d.path}>
              <NavLink to={d.path} className="pp-rail-link">
                <span>{d.label}</span>
                <kbd className="pp-kbd" aria-hidden="true">
                  {shortcutLabel(d)}
                </kbd>
              </NavLink>
            </li>
          ))}
        </ul>
      </nav>
      <main id="main" tabIndex={-1} className="pp-main">
        <Outlet />
      </main>
      {overlay === "palette" && <CommandPalette commands={commands} onClose={close} />}
      {overlay === "help" && <ShortcutList onClose={close} />}
    </div>
  );
}

/** The app frame: header with the pulse line slot, the rail, and the current screen. */
export function Frame() {
  return (
    <ConnectionProvider>
      <FrameBody />
    </ConnectionProvider>
  );
}
