import { useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";
import { CoreError } from "../core/client";
import { writeCore } from "../core/rust";
import { reason } from "../core/writes";
import { OFFLINE_REASON, useConnection } from "../frame/connection";
import { StartConfirm } from "./SendSheet";
import {
  actionsFor,
  sendPrintersQuery,
  sendQueueQuery,
  uncertain,
  type QueuedSend,
  type RowAction,
  type SendPrinter,
} from "./sends";

const LABELS: Readonly<Record<RowAction, string>> = {
  "send-now": "Send now",
  remove: "Remove",
  printed: "It printed",
  "not-printed": "It did not print",
};

const UNCONFIRMED =
  "Print Partner could not confirm what the core did, so this row shows what the core has. Nothing was sent again.";
const WAITING = "Waiting for the core's answer.";

/** One write for a row. A 4xx is a refusal with the core's reason; a 5xx or no answer is not. */
async function write(item: QueuedSend, action: RowAction): Promise<string | null> {
  try {
    if (action === "send-now") await writeCore("sendQueuedNow", { id: item.id });
    else if (action === "remove") await writeCore("removeQueuedSend", { id: item.id });
    else {
      const body = { printed: action === "printed", job_id: item.upload_job_id };
      await writeCore("reconcileQueuedSend", { id: item.id }, body);
    }
    return null;
  } catch (error) {
    if (uncertain(error)) return UNCONFIRMED;
    if (error instanceof CoreError) return reason(error);
    throw error;
  }
}

/** What the start confirmation needs about the printer, read fresh. */
type StartTarget = { printerName: string; bedToClear: boolean; stateUnknown: boolean };

/**
 * A row's actions: Send now and Remove for a Not sent or Failed send; "It printed", "It did
 * not print" and Remove for an Outcome unknown one. Each answer is its own labelled button
 * and none is the default, so a mis-tap or Enter never records a history entry. Send now on
 * an "Upload and start print" send reads the printer again and asks first, as the sheet does.
 * One write at a time: the buttons stay off until the queue has been read again, and nothing
 * is repeated on its own.
 */
export function SendRowActions({ item }: { item: QueuedSend }) {
  const queryClient = useQueryClient();
  const offline = useConnection() === "offline";
  const reasonId = useId();
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState<StartTarget | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const actions = actionsFor(item);

  const run = async (action: RowAction) => {
    setConfirm(null);
    setNotice(null);
    try {
      setNotice(await write(item, action));
    } catch (error) {
      setNotice(error instanceof Error ? reason(error) : String(error));
    } finally {
      await queryClient.invalidateQueries({ queryKey: sendQueueQuery.queryKey });
      setBusy(false);
    }
  };

  /** A print start needs the printer's current state, not the state from when the tab opened. */
  const askToStart = async () => {
    let printer: SendPrinter | undefined;
    try {
      const read = await queryClient.fetchQuery(sendPrintersQuery);
      printer = read.printers.find((p) => p.id === item.printer_id);
    } catch {
      printer = undefined;
    }
    setBusy(false);
    setConfirm({
      printerName: printer?.name ?? item.printer_id,
      bedToClear: printer?.state.display === "bed_to_clear",
      stateUnknown: !printer,
    });
  };

  const press = (action: RowAction) => {
    if (busy || offline) return;
    setBusy(true);
    void (action === "send-now" && item.start ? askToStart() : run(action));
  };

  if (actions.length === 0 && !notice) return null;
  const off = busy || offline;
  return (
    <div className="pp-row-actions" role="group" aria-label={`Actions for ${item.filename}`}>
      {actions.map((action) => (
        <button
          key={action}
          type="button"
          className="pp-button"
          aria-disabled={off || undefined}
          aria-describedby={off ? reasonId : undefined}
          onClick={() => press(action)}
        >
          {LABELS[action]}
        </button>
      ))}
      {off && (
        <span id={reasonId} className="pp-mutation-reason">
          {offline ? OFFLINE_REASON : WAITING}
        </span>
      )}
      <p role="status" className="pp-fact-detail">
        {notice}
      </p>
      {confirm && (
        <StartConfirm
          file={item.filename}
          {...confirm}
          onCancel={() => setConfirm(null)}
          onStart={() => {
            setBusy(true);
            void run("send-now");
          }}
        />
      )}
    </div>
  );
}
