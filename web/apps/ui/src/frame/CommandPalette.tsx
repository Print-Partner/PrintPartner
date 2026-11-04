import { useEffect, useId, useState, type KeyboardEvent } from "react";
import { Dialog } from "./Dialog";

export interface Command {
  id: string;
  label: string;
  /** Shortcut shown beside the command, if it has one. */
  shortcut?: string;
  run: () => void;
}

/**
 * Cmd/Ctrl+K palette. A combobox over a listbox: type to filter, Up and Down to move,
 * Enter to run, Escape to close. The input keeps focus the whole time.
 */
export function CommandPalette({
  commands,
  onClose,
}: {
  commands: readonly Command[];
  onClose: () => void;
}) {
  const listId = useId();
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const needle = query.trim().toLowerCase();
  const matches = commands.filter((c) => c.label.toLowerCase().includes(needle));
  const current = Math.min(active, Math.max(matches.length - 1, 0));
  const activeId = matches[current] ? `${listId}-${matches[current].id}` : undefined;

  // Focus stays in the input, so the browser does not scroll to the active option itself.
  // At 400% zoom most options start below the fold.
  useEffect(() => {
    if (activeId) document.getElementById(activeId)?.scrollIntoView?.({ block: "nearest" });
  }, [activeId]);

  function run(command: Command | undefined) {
    if (!command) return;
    onClose();
    command.run();
  }

  function onKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (matches.length === 0) return;
      const step = event.key === "ArrowDown" ? 1 : -1;
      setActive((current + step + matches.length) % matches.length);
    } else if (event.key === "Enter") {
      event.preventDefault();
      run(matches[current]);
    }
  }

  return (
    <Dialog title="Command palette" onClose={onClose} className="pp-palette">
      <input
        data-autofocus
        className="pp-palette-input"
        role="combobox"
        aria-label="Command"
        aria-expanded="true"
        aria-controls={listId}
        aria-autocomplete="list"
        aria-activedescendant={activeId}
        placeholder="Type a command"
        autoComplete="off"
        spellCheck={false}
        value={query}
        onChange={(e) => {
          setQuery(e.target.value);
          setActive(0);
        }}
        onKeyDown={onKeyDown}
      />
      <ul id={listId} role="listbox" aria-label="Commands" className="pp-palette-list">
        {matches.map((command, index) => (
          <li
            key={command.id}
            id={`${listId}-${command.id}`}
            role="option"
            aria-selected={index === current}
            className="pp-palette-option"
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => run(command)}
          >
            <span>{command.label}</span>
            {command.shortcut && <kbd className="pp-kbd">{command.shortcut}</kbd>}
          </li>
        ))}
      </ul>
      {matches.length === 0 && (
        <p role="status" className="pp-palette-empty">
          No command matches “{query}”.
        </p>
      )}
    </Dialog>
  );
}
