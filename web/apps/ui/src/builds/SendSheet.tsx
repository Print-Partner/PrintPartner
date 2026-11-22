import type { SendUnitsPreview } from "@print-partner/rust-client";
import { useQuery, useQueryClient, type UseQueryResult } from "@tanstack/react-query";
import { useId, useState } from "react";
import { CoreError } from "../core/client";
import { writeCore } from "../core/rust";
import { reason } from "../core/writes";
import { useConnection } from "../frame/connection";
import { Dialog } from "../frame/Dialog";
import { MutationButton } from "../screen/ScreenTemplate";
import { PrinterStatus } from "../status/PrinterStatus";
import {
  addSend,
  previewSendUnits,
  sendable,
  sendPrintersQuery,
  sendQueueQuery,
  SLICED,
  startRefusal,
  uncertain,
  type SendPrinter,
} from "./sends";

/** What one Send did, for the tab to say once the sheet has closed. */
export type SendOutcome =
  { kind: "sent" } | { kind: "not-sent"; reason: string } | { kind: "lost" };

type Mode = "upload" | "start";

/**
 * Add, then Send now, for one file. A refusal or an uncertain answer is reported, never
 * retried: the queue is read again and the user decides.
 */
async function send(
  build: number,
  printer: string,
  file: File,
  start: boolean,
  binding: string | null,
): Promise<SendOutcome> {
  let id: string;
  try {
    id = (await addSend(build, printer, file, start, binding)).item.id;
  } catch (error) {
    if (uncertain(error)) return { kind: "lost" };
    throw error;
  }
  try {
    await writeCore("sendQueuedNow", { id });
    return { kind: "sent" };
  } catch (error) {
    if (uncertain(error)) return { kind: "lost" };
    if (error instanceof CoreError) return { kind: "not-sent", reason: reason(error) };
    throw error;
  }
}

/**
 * The explicit step before any print start, from the sheet or a row. `bedToClear` is true when
 * the printer shows Bed to clear: the core shows that for an idle printer whose bed needs
 * clearing or is not confirmed clear, so the operator confirms it first.
 */
export function StartConfirm({
  file,
  printerName,
  bedToClear,
  stateUnknown = false,
  onCancel,
  onStart,
}: {
  file: string;
  printerName: string;
  bedToClear: boolean;
  /** The printer's current state could not be read, so the bed may need clearing. */
  stateUnknown?: boolean;
  onCancel: () => void;
  onStart: () => void;
}) {
  return (
    <Dialog title="Start this print?" onClose={onCancel}>
      {bedToClear && (
        <p className="pp-banner" data-kind="attention" role="note">
          The bed needs clearing. Clear it before you start this print.
        </p>
      )}
      {stateUnknown && (
        <p className="pp-banner" data-kind="attention" role="note">
          Print Partner could not read this printer's current state. Check that the bed is clear
          before you start this print.
        </p>
      )}
      <p>
        Print Partner uploads <strong>{file}</strong> to <strong>{printerName}</strong> and starts
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

/** A stable key per chosen File object, so choosing another file starts a new preview. */
const fileKeys = new WeakMap<File, number>();
let nextFileKey = 0;
const fileKey = (file: File) => {
  if (!fileKeys.has(file)) fileKeys.set(file, nextFileKey++);
  return fileKeys.get(file)!;
};

/**
 * The core's preview of the Checkoff units a send of `file` links. It refetches only when
 * Add finds its units changed, and switching files or closing the sheet aborts a preview
 * still in flight.
 *
 * `linked` is the one decision for what the sheet shows and what Add sends: the send links
 * units, bound to the preview's `binding`, only while the latest preview succeeded.
 * `settled` is false while a preview is being read, so Send waits for it.
 */
function useSendUnitsPreview(build: number, file: File | null) {
  const preview = useQuery({
    queryKey: ["core", "send-units-preview", build, file ? fileKey(file) : null],
    queryFn: ({ signal }: { signal: AbortSignal }) => previewSendUnits(build, file!, signal),
    enabled: file !== null,
    retry: false,
    staleTime: Infinity,
    gcTime: 0,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  });
  const linked = preview.status === "success" ? preview.data : null;
  const settled = file !== null && !preview.isFetching && (linked !== null || preview.isError);
  return { preview, linked, settled };
}

const UNITS_CHANGED = "send_units_changed";

const NO_GRAMS = "This send links no Checkoff units, so it records no filament grams.";

const FILE_NOTICE: Record<Exclude<SendUnitsPreview["file"], "read">, string> = {
  unsupported:
    "Print Partner cannot read the object names in this binary G-code file: it uses a compression or encoding the core does not decode.",
  unreadable: "This binary G-code file is damaged, so Print Partner cannot read its object names.",
  too_many_objects: "This file names more than 10,000 objects, so Print Partner does not match them.",
  too_many_units:
    "This file prints more units than one send can link (at most 1,000 units, with their names).",
};

/** The Checkoff units the send links, as `linked` (the latest successful preview) says. */
function UnitsField({
  preview,
  linked,
}: {
  preview: UseQueryResult<SendUnitsPreview>;
  linked: SendUnitsPreview | null;
}) {
  let body;
  if (preview.isError) {
    body = (
      <p role="alert">
        Could not match this file to the Build's Checkoff units: {reason(preview.error)}.{" "}
        {NO_GRAMS}
      </p>
    );
  } else if (!linked) {
    body = <p className="pp-muted">Reading the objects in this file.</p>;
  } else if (linked.file !== "read") {
    body = (
      <p role="note">
        {FILE_NOTICE[linked.file]} {NO_GRAMS}
      </p>
    );
  } else if (linked.unit_count === 0) {
    body = <p>No object in this file matches a unit still to print. {NO_GRAMS}</p>;
  } else {
    body = (
      <ul>
        {linked.parts.map(({ part_id: id, filename, count }) => (
          <li key={id}>
            {filename} × {count}
          </li>
        ))}
      </ul>
    );
  }
  return (
    <fieldset className="pp-field">
      <legend>Checkoff units</legend>
      {body}
      {linked && linked.unmatched.length > 0 && (
        <p className="pp-muted">Not matched to a unit: {linked.unmatched.join(", ")}</p>
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
  const sliced = fileProblem ? null : file;
  const { preview, linked, settled } = useSendUnitsPreview(build, sliced);
  const refusal = printer ? startRefusal(printer) : null;
  // A printer that cannot start a print gets Upload only, whatever was chosen before.
  const chosen: Mode = refusal ? "upload" : mode;
  const ready = Boolean(printer) && settled && !busy;

  const run = async (start: boolean) => {
    if (!file || !printer || busy) return;
    setBusy(true);
    setProblem(null);
    try {
      onClose(await send(build, printer.id, file, start, linked?.binding ?? null));
    } catch (error) {
      // Add was refused: nothing is queued, so the sheet stays open with the reason.
      setProblem(error instanceof Error ? reason(error) : String(error));
      setConfirming(false);
      // The Build's units changed since the preview: read it again before another Send.
      if (error instanceof CoreError && error.code === UNITS_CHANGED) void preview.refetch();
    } finally {
      setBusy(false);
      void queryClient.invalidateQueries({ queryKey: sendQueueQuery.queryKey });
    }
  };

  if (confirming && file && printer) {
    return (
      <StartConfirm
        file={file.name}
        printerName={printer.name}
        bedToClear={printer.state.display === "bed_to_clear"}
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
      {sliced && <UnitsField preview={preview} linked={linked} />}
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
