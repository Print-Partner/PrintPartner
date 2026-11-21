import { Command as Cmdk } from "cmdk";
import { useRef, useState } from "react";
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
 * What to do when nothing matches. It sits outside the listbox, so its button is not an option.
 */
function NoMatch({ search, clear }: { search: string; clear: () => void }) {
  return (
    <div className="pp-palette-empty" role="status">
      <p>No command matches &ldquo;{search.trim()}&rdquo;.</p>
      <p>
        Commands open a screen or Help. Try a screen name such as Builds or Library, or
        &ldquo;help&rdquo;.
      </p>
      <button type="button" className="pp-button" onClick={clear}>
        Clear search
      </button>
    </div>
  );
}

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
  const [search, setSearch] = useState("");
  const input = useRef<HTMLInputElement>(null);
  const none = !commands.some((command) => contains(command.label, search));
  return (
    <Dialog title="Command palette" onClose={onClose} className="pp-palette">
      <Cmdk label="Commands" filter={contains} loop vimBindings={false}>
        <Cmdk.Input
          ref={input}
          value={search}
          onValueChange={setSearch}
          className="pp-palette-input"
          aria-label="Command"
          placeholder="Type a command"
        />
        <Cmdk.List className="pp-palette-list" aria-label="Commands">
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
        {none && (
          <NoMatch
            search={search}
            clear={() => {
              setSearch("");
              input.current?.focus();
            }}
          />
        )}
      </Cmdk>
    </Dialog>
  );
}
