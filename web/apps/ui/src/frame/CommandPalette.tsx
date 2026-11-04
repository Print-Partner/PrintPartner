import { Command as Cmdk } from "cmdk";
import { Dialog } from "./Dialog";

export interface Command {
  id: string;
  label: string;
  /** Shortcut shown beside the command, if it has one. */
  shortcut?: string;
  run: () => void;
}

// Plain substring match, so commands keep their listed order instead of a fuzzy ranking.
// vimBindings is off because cmdk would take Ctrl+K, which closes the palette.
const contains = (value: string, search: string) =>
  value.toLowerCase().includes(search.trim().toLowerCase()) ? 1 : 0;

/**
 * Cmd/Ctrl+K palette on cmdk (docs/architecture.md): a combobox over a listbox. Type to
 * filter, Up and Down to move, Enter to run, Escape to close. cmdk leaves keys to an
 * active IME composition and scrolls the active option into view.
 */
export function CommandPalette({
  commands,
  onClose,
}: {
  commands: readonly Command[];
  onClose: () => void;
}) {
  return (
    <Dialog title="Command palette" onClose={onClose} className="pp-palette">
      <Cmdk label="Commands" filter={contains} loop vimBindings={false}>
        <Cmdk.Input
          className="pp-palette-input"
          aria-label="Command"
          placeholder="Type a command"
        />
        <Cmdk.List className="pp-palette-list" aria-label="Commands">
          <Cmdk.Empty className="pp-palette-empty">No command matches.</Cmdk.Empty>
          {commands.map((command) => (
            <Cmdk.Item
              key={command.id}
              value={command.label}
              className="pp-palette-option"
              onSelect={() => {
                onClose();
                command.run();
              }}
            >
              <span>{command.label}</span>
              {command.shortcut && <kbd className="pp-kbd">{command.shortcut}</kbd>}
            </Cmdk.Item>
          ))}
        </Cmdk.List>
      </Cmdk>
    </Dialog>
  );
}
