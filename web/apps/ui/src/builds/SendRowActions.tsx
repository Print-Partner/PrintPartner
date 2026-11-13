import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { CoreError } from "../core/client";
import { writeCore } from "../core/rust";
import { reason } from "../core/writes";
import { MutationButton } from "../screen/ScreenTemplate";
import { StartConfirm } from "./SendSheet";
import {
  actionsFor,
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

/**
 * A row's actions: Send now and Remove for a Not sent or Failed send; "It printed", "It did
 * not print" and Remove for an Outcome unknown one. Each answer is its own labelled button
 * and none is the default, so a mis-tap or Enter never records a history entry. Send now on
 * an "Upload and start print" send asks first, as the sheet does. One write at a time; after
 * each the queue is read again, and nothing is repeated on its own.
 */
export function SendRowActions({
  item,
  printer,
}: {
  item: QueuedSend;
  printer: SendPrinter | undefined;
}) {
  const queryClient = useQueryClient();
  const [busy, setBusy] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const actions = actionsFor(item);

  const run = async (action: RowAction) => {
    if (busy) return;
    setBusy(true);
    setConfirming(false);
    setNotice(null);
    try {
      setNotice(await write(item, action));
    } catch (error) {
      setNotice(error instanceof Error ? reason(error) : String(error));
    } finally {
      setBusy(false);
      void queryClient.invalidateQueries({ queryKey: sendQueueQuery.queryKey });
    }
  };
  const press = (action: RowAction) =>
    action === "send-now" && item.start ? setConfirming(true) : void run(action);

  if (actions.length === 0 && !notice) return null;
  return (
    <div className="pp-row-actions" role="group" aria-label={`Actions for ${item.filename}`}>
      {actions.map((action) =>
        busy ? (
          <button key={action} type="button" className="pp-button" aria-disabled="true">
            {LABELS[action]}
          </button>
        ) : (
          <MutationButton key={action} onClick={() => press(action)}>
            {LABELS[action]}
          </MutationButton>
        ),
      )}
      {notice && (
        <p role="status" className="pp-fact-detail">
          {notice}
        </p>
      )}
      {confirming && (
        <StartConfirm
          file={item.filename}
          printerName={printer?.name ?? item.printer_id}
          bedToClear={printer?.state.display === "bed_to_clear"}
          onCancel={() => setConfirming(false)}
          onStart={() => void run("send-now")}
        />
      )}
    </div>
  );
}
