import * as RadixDialog from "@radix-ui/react-dialog";
import { useState, type ReactNode } from "react";

/**
 * A modal dialog on Radix Dialog (docs/architecture.md): it traps focus, hides the page
 * behind it from assistive technology, closes on Escape or a backdrop click and returns
 * focus to the element that opened it. The content sits inside the overlay so a tall
 * dialog scrolls at 400% zoom.
 */
export function Dialog({
  title,
  onClose,
  children,
  className = "",
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
  className?: string;
}) {
  // Radix returns focus to a Dialog.Trigger; these dialogs open from shortcuts too, so
  // remember whatever had focus when this one opened.
  const [opener] = useState(() => document.activeElement as HTMLElement | null);
  return (
    <RadixDialog.Root open onOpenChange={(open) => !open && onClose()}>
      <RadixDialog.Portal>
        <RadixDialog.Overlay className="pp-backdrop">
          <RadixDialog.Content
            className={`pp-dialog ${className}`}
            aria-describedby={undefined}
            // Escape during IME composition cancels the composition, not the dialog.
            onEscapeKeyDown={(event) => event.isComposing && event.preventDefault()}
            onCloseAutoFocus={(event) => {
              event.preventDefault();
              opener?.focus();
            }}
          >
            <RadixDialog.Title className="pp-dialog-title">{title}</RadixDialog.Title>
            {children}
          </RadixDialog.Content>
        </RadixDialog.Overlay>
      </RadixDialog.Portal>
    </RadixDialog.Root>
  );
}
