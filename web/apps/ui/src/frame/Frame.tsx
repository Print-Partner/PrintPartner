import { useState } from "react";
import { NavLink, Outlet, useNavigate } from "react-router";
import { PrinterStreamProvider } from "../farm/printerStream";
import { PulseLine } from "../farm/PulseLine";
import { CommandPalette, type Command } from "./CommandPalette";
import { ConnectionProvider, useConnection } from "./connection";
import { DESTINATIONS, shortcutLabel } from "./destinations";
import { Dialog } from "./Dialog";
import { useCharacterShortcuts, useShortcuts } from "./useShortcuts";

const GLOBAL_SHORTCUTS: readonly { keys: string; action: string }[] = [
  ...DESTINATIONS.map((d) => ({ keys: shortcutLabel(d), action: `Go to ${d.label}` })),
  { keys: "Cmd/Ctrl K", action: "Open or close the command palette" },
  { keys: "?", action: "Show keyboard shortcuts" },
  { keys: "Esc", action: "Close the open dialog" },
];

function ShortcutList({
  onClose,
  characterKeys,
  setCharacterKeys,
}: {
  onClose: () => void;
  characterKeys: boolean;
  setCharacterKeys: (on: boolean) => void;
}) {
  return (
    <Dialog title="Keyboard shortcuts" onClose={onClose}>
      <label className="pp-setting">
        <input
          type="checkbox"
          checked={characterKeys}
          onChange={(e) => setCharacterKeys(e.target.checked)}
        />
        Single-key shortcuts (G sequences and ?)
      </label>
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
      <button type="button" className="pp-button" onClick={onClose}>
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

type Overlay = "palette" | "help" | null;

function FrameBody() {
  const navigate = useNavigate();
  const [overlay, setOverlay] = useState<Overlay>(null);
  const [characterKeys, setCharacterKeys] = useCharacterShortcuts();
  const close = () => setOverlay(null);

  useShortcuts({
    characterKeys,
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
    {
      id: "character-keys",
      label: characterKeys ? "Turn off single-key shortcuts" : "Turn on single-key shortcuts",
      run: () => setCharacterKeys(!characterKeys),
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
                {characterKeys && (
                  <kbd className="pp-kbd" aria-hidden="true">
                    {shortcutLabel(d)}
                  </kbd>
                )}
              </NavLink>
            </li>
          ))}
        </ul>
      </nav>
      <main id="main" tabIndex={-1} className="pp-main">
        <Outlet />
      </main>
      {overlay === "palette" && <CommandPalette commands={commands} onClose={close} />}
      {overlay === "help" && (
        <ShortcutList
          onClose={close}
          characterKeys={characterKeys}
          setCharacterKeys={setCharacterKeys}
        />
      )}
    </div>
  );
}

/** The app frame: header with the pulse line, the rail, and the current screen. */
export function Frame() {
  return (
    <ConnectionProvider>
      <PrinterStreamProvider>
        <FrameBody />
      </PrinterStreamProvider>
    </ConnectionProvider>
  );
}
