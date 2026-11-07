import { useMutation, type UseMutationResult } from "@tanstack/react-query";
import { useEffect, useRef, type RefObject } from "react";
import { CoreError } from "../core/client";
import { RequestContractError } from "../core/rust";

// A send that never reached the core, or whose answer was lost: the undo may have landed.
// A body the client refused to send was not lost.
const lostSend = (error: Error) =>
  !(error instanceof CoreError) && !(error instanceof RequestContractError);
// A resent undo whose first answer was lost finds the burst already undone.
const ALREADY_UNDONE = "This Checkoff burst was already undone";

/**
 * Undo of a checkoff burst, one at a time: a second click before the first answer would
 * send it again. A lost send is retried, and "already undone" after a lost send means the
 * undo landed, so it counts as done (`null`). `reload` runs once the undo settles, and
 * focus moves to the outcome, because the Undo button goes away with the undo.
 */
export function useBurstUndo<E extends { id: string }, R>(
  send: (entry: E) => Promise<R>,
  reload: () => Promise<unknown>,
) {
  const inFlight = useRef(false);
  // Entries whose undo send was lost: a later "already undone" means it landed.
  const lostUndos = useRef(new Set<string>());
  const message = useRef<HTMLDivElement>(null);
  const undo = useMutation({
    mutationFn: async (entry: E) => {
      try {
        return await send(entry);
      } catch (error) {
        if (error instanceof Error && lostSend(error)) lostUndos.current.add(entry.id);
        const landed =
          error instanceof CoreError &&
          error.detail === ALREADY_UNDONE &&
          lostUndos.current.has(entry.id);
        if (!landed) throw error;
        return null;
      }
    },
    retry: (failures: number, error: Error) => failures < 2 && lostSend(error),
    onSettled: () => {
      inFlight.current = false;
      return reload();
    },
  });
  useEffect(() => {
    if (undo.isSuccess || undo.isError) message.current?.focus();
  }, [undo.isSuccess, undo.isError, undo.submittedAt]);
  const start = (entry: E) => {
    if (inFlight.current) return;
    inFlight.current = true;
    undo.mutate(entry);
  };
  return { undo, start, message };
}

/** What happened to the last undo: done, refused, or maybe lost on the way. */
export function UndoOutcome<E, R>({
  undo,
  message,
  what,
  done,
  reloaded,
}: {
  undo: UseMutationResult<R | null, Error, E>;
  message: RefObject<HTMLDivElement | null>;
  /** The undone record, such as "checkoff burst". */
  what: string | undefined;
  done: string;
  /** What was reloaded to show the outcome, such as "The log". */
  reloaded: string;
}) {
  return (
    <div ref={message} tabIndex={-1}>
      {undo.isError &&
        (lostSend(undo.error) ? (
          <div role="alert" className="pp-banner" data-kind="stale">
            <p>
              The undo of the {what} may not have reached the core. {reloaded} was reloaded and
              shows what happened.
            </p>
            <p className="pp-banner-detail">{undo.error.message}</p>
          </div>
        ) : (
          <div role="alert" className="pp-banner" data-kind="error">
            <p>Could not undo the {what}.</p>
            <p className="pp-banner-detail">{undo.error.message}</p>
          </div>
        ))}
      {undo.isSuccess && <p role="status">{done}</p>}
    </div>
  );
}
