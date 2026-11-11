import { useId } from "react";
import { Link } from "react-router";
import { PLAN_HELP } from "../builds/planCopy";
import { DESTINATIONS, shortcutLabel } from "./destinations";
import { Dialog } from "./Dialog";

const GLOBAL_SHORTCUTS: readonly { keys: string; action: string }[] = [
  ...DESTINATIONS.map((d) => ({ keys: shortcutLabel(d), action: `Go to ${d.label}` })),
  { keys: "Cmd/Ctrl K", action: "Open or close the command palette" },
  { keys: "?", action: "Show keyboard shortcuts and help" },
  { keys: "Esc", action: "Close the open dialog" },
];

/** Shared Help for the header, command palette and keyboard shortcut. */
export function HelpDialog({
  onClose,
  characterKeys,
  setCharacterKeys,
}: {
  onClose: () => void;
  characterKeys: boolean;
  setCharacterKeys: (on: boolean) => void;
}) {
  const ids = { destinations: useId(), plan: useId(), keys: useId() };
  return (
    <Dialog title="Help" onClose={onClose}>
      <section aria-labelledby={ids.destinations} className="pp-help-section">
        <h3 id={ids.destinations}>Where to go</h3>
        <dl>
          {DESTINATIONS.map((destination) => (
            <div key={destination.path}>
              <dt>
                <Link to={destination.path} className="pp-link" onClick={onClose}>
                  {destination.label}
                </Link>
              </dt>
              <dd>{destination.help}.</dd>
            </div>
          ))}
        </dl>
      </section>
      <section aria-labelledby={ids.plan} className="pp-help-section">
        <h3 id={ids.plan}>Plan changes</h3>
        <ul>
          {PLAN_HELP.map((line) => (
            <li key={line}>{line}</li>
          ))}
        </ul>
      </section>
      <section aria-labelledby={ids.keys} className="pp-help-section">
        <h3 id={ids.keys}>Keyboard shortcuts</h3>
        <p>
          G sequences and ? work outside text fields when no dialog is open. Cmd/Ctrl K also works
          in text fields. Escape closes the open dialog.
        </p>
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
      </section>
      <button type="button" className="pp-button" onClick={onClose}>
        Close
      </button>
    </Dialog>
  );
}
