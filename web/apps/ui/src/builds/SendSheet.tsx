import { useQuery, useQueryClient, type UseQueryResult } from "@tanstack/react-query";
import { useId, useState } from "react";
import { CoreError } from "../core/client";
import { writeCore } from "../core/rust";
import { lostSend, reason } from "../core/writes";
import { useConnection } from "../frame/connection";
import { Dialog } from "../frame/Dialog";
import { MutationButton } from "../screen/ScreenTemplate";
import { PrinterStatus } from "../status/PrinterStatus";
import {
  addSend,
  sendable,
  sendPrintersQuery,
  sendQueueQuery,
  SLICED,
  startRefusal,
  type SendPrinter,
} from "./sends";

/** What one Send did, for the tab to say once the sheet has closed. */
export type SendOutcome =
  { kind: "sent" } | { kind: "not-sent"; reason: string } | { kind: "lost" };

type Mode = "upload" | "start";

/**
 * Add, then Send now, for one file. A refusal or a lost answer is reported, never retried:
 * the queue is read again and the user decides.
 */
async function send(
  build: number,
  printer: string,
  file: File,
  start: boolean,
): Promise<SendOutcome> {
  let id: string;
  try {
    id = (await addSend(build, printer, file, start)).item.id;
  } catch (error) {
    if (error instanceof Error && lostSend(error)) return { kind: "lost" };
    throw error;
  }
  try {
    await writeCore("sendQueuedNow", { id });
    return { kind: "sent" };
  } catch (error) {
    if (error instanceof Error && lostSend(error)) return { kind: "lost" };
    if (error instanceof CoreError) return { kind: "not-sent", reason: reason(error) };
    throw error;
  }
}

function StartConfirm({
  file,
  printer,
  onCancel,
  onStart,
}: {
  file: string;
  printer: SendPrinter;
  onCancel: () => void;
  onStart: () => void;
}) {
  const bed = printer.state.bed_clear;
  return (
    <Dialog title="Start this print?" onClose={onCancel}>
      {bed === "needs_clearing" && (
        <p className="pp-banner" data-kind="attention" role="note">
          The bed needs clearing. Clear it before you start this print.
        </p>
      )}
      {bed === "unknown" && <p role="note">The bed is not confirmed clear.</p>}
      <p>
        Print Partner uploads <strong>{file}</strong> to <strong>{printer.name}</strong> and starts
        the print.
      </p>
      <div className="pp-dialog-actions">
        <button type="button" className="pp-button" onClick={onCancel}>
          Cancel
        </button>
        <MutationButton primary onClick={onStart}>
          Start print
        </MutationButton>
      </div>
    </Dialog>
  );
}

function PrinterField({
  id,
  printers,
  printerId,
  onChange,
}: {
  id: string;
  printers: UseQueryResult<{ printers: SendPrinter[] }>;
  printerId: string;
  onChange: (id: string) => void;
}) {
  const choices = (printers.data?.printers ?? []).filter(sendable);
  const printer = choices.find((p) => p.id === printerId);
  return (
    <div className="pp-field">
      <label htmlFor={id}>Printer</label>
      {printers.isError ? (
        <p role="alert">Could not read the printers: {printers.error.message}</p>
      ) : (
        <select id={id} value={printerId} onChange={(e) => onChange(e.target.value)}>
          <option value="">{printers.data ? "Choose a printer" : "Loading printers"}</option>
          {choices.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </select>
      )}
      {printers.data && choices.length === 0 && (
        <p>No Moonraker or PrusaLink printer is set up. Add one in Settings.</p>
      )}
      {printer && <PrinterStatus state={printer.state.display} />}
    </div>
  );
}

function ModeField({
  chosen,
  refusal,
  onChange,
}: {
  chosen: Mode;
  refusal: string | null;
  onChange: (mode: Mode) => void;
}) {
  const reasonId = useId();
  return (
    <fieldset className="pp-field">
      <legend>What happens</legend>
      <label>
        <input
          type="radio"
          name="send-mode"
          checked={chosen === "upload"}
          onChange={() => onChange("upload")}
        />{" "}
        Upload only
      </label>
      <label>
        <input
          type="radio"
          name="send-mode"
          checked={chosen === "start"}
          disabled={!!refusal}
          aria-describedby={refusal ? reasonId : undefined}
          onChange={() => onChange("start")}
        />{" "}
        Upload and start print
      </label>
      {refusal && (
        <p id={reasonId} className="pp-muted">
          Upload and start print is not available: {refusal}
        </p>
      )}
    </fieldset>
  );
}

/** "Send a file": choose a sliced file, a printer and upload-only or upload-and-start. */
export function SendSheet({
  build,
  onClose,
}: {
  build: number;
  onClose: (outcome?: SendOutcome) => void;
}) {
  const queryClient = useQueryClient();
  const printers = useQuery(sendPrintersQuery);
  const [file, setFile] = useState<File | null>(null);
  const [printerId, setPrinterId] = useState("");
  // Upload only is selected every time the sheet opens.
  const [mode, setMode] = useState<Mode>("upload");
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const ids = { file: useId(), printer: useId() };
  const offline = useConnection() === "offline";

  const printer = printers.data?.printers.filter(sendable).find((p) => p.id === printerId);
  const fileProblem =
    file && !SLICED.test(file.name)
      ? "Only .gcode, .gco and .bgcode files can be sent to a printer."
      : null;
  const refusal = printer ? startRefusal(printer) : null;
  // A printer that cannot start a print gets Upload only, whatever was chosen before.
  const chosen: Mode = refusal ? "upload" : mode;
  const ready = Boolean(file && !fileProblem && printer) && !busy;

  const run = async (start: boolean) => {
    if (!file || !printer || busy) return;
    setBusy(true);
    setProblem(null);
    try {
      onClose(await send(build, printer.id, file, start));
    } catch (error) {
      // Add was refused: nothing is queued, so the sheet stays open with the reason.
      setProblem(error instanceof Error ? reason(error) : String(error));
      setConfirming(false);
    } finally {
      setBusy(false);
      void queryClient.invalidateQueries({ queryKey: sendQueueQuery.queryKey });
    }
  };

  if (confirming && file && printer) {
    return (
      <StartConfirm
        file={file.name}
        printer={printer}
        onCancel={() => setConfirming(false)}
        onStart={() => void run(true)}
      />
    );
  }
  return (
    <Dialog title="Send a file" onClose={() => onClose()}>
      <div className="pp-field">
        <label htmlFor={ids.file}>Sliced file</label>
        <input
          id={ids.file}
          type="file"
          accept=".gcode,.gco,.bgcode"
          onChange={(event) => setFile(event.target.files?.[0] ?? null)}
        />
        {fileProblem && <p role="alert">{fileProblem}</p>}
      </div>
      <PrinterField
        id={ids.printer}
        printers={printers}
        printerId={printerId}
        onChange={setPrinterId}
      />
      <ModeField chosen={chosen} refusal={refusal} onChange={setMode} />
      {problem && (
        <p role="alert" className="pp-banner" data-kind="error">
          Not sent: {problem}
        </p>
      )}
      <div className="pp-dialog-actions">
        <button type="button" className="pp-button" onClick={() => onClose()}>
          Cancel
        </button>
        {ready || offline ? (
          <MutationButton
            primary
            onClick={() => (chosen === "start" ? setConfirming(true) : void run(false))}
          >
            Send
          </MutationButton>
        ) : (
          <button type="button" className="pp-button pp-button-primary" aria-disabled="true">
            {busy ? "Sending" : "Send"}
          </button>
        )}
      </div>
    </Dialog>
  );
}
