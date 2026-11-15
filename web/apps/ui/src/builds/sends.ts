import {
  routePath,
  routes,
  type FarmPrinterList,
  type QueuedSendReceipt,
  type SendQueue,
} from "@print-partner/rust-client";
import { CoreError, fetchCore } from "../core/client";
import { ContractError, readCore } from "../core/rust";
import { lostSend } from "../core/writes";

export type QueuedSend = SendQueue["items"][number];
export type SendPrinter = FarmPrinterList["printers"][number];

/** While a listed send is Sending, the list reads the queue again this often. */
export const SENDING_POLL_MS = 3_000;

/** Sliced files the core takes; anything else is refused before a request. */
export const SLICED = /\.(gcode|gco|bgcode)$/i;

export const sendQueueQuery = {
  queryKey: ["core", "send-queue"],
  queryFn: ({ signal }: { signal: AbortSignal }) => readCore("readSendQueue", {}, signal),
} as const;

/** The farm read, fresh each time the sheet opens, for each printer's current state. */
export const sendPrintersQuery = {
  queryKey: ["core", "printers", "send-sheet"],
  queryFn: ({ signal }: { signal: AbortSignal }) => readCore("listPrinters", {}, signal),
  staleTime: 0,
} as const;

/** This Build's sends, newest first; removed ones are hidden. */
export function buildSends(queue: SendQueue, build: number): QueuedSend[] {
  return queue.items
    .filter((item) => item.profile_id === build && item.state !== "cancelled")
    .reverse();
}

export type SendStatus = "Not sent" | "Sending" | "Sent" | "Failed" | "Outcome unknown";

export function sendStatus(item: QueuedSend): SendStatus {
  if (item.outcome_unknown) return "Outcome unknown";
  switch (item.state) {
    case "queued":
      return "Not sent";
    case "sending":
      return "Sending";
    case "done":
      return "Sent";
    default:
      return "Failed";
  }
}

/**
 * Whether a failed write's effect is unknown: no answer reached the UI, or the core answered
 * 5xx, which it can do after the write committed. Only a 4xx is a refusal that wrote nothing.
 */
export const uncertain = (error: unknown) =>
  error instanceof Error &&
  (lostSend(error) || (error instanceof CoreError && error.status >= 500));

/** What a row offers: none while a send is in flight or after it is Sent. */
export type RowAction = "send-now" | "remove" | "printed" | "not-printed";

export function actionsFor(item: QueuedSend): RowAction[] {
  switch (sendStatus(item)) {
    case "Not sent":
    case "Failed":
      return ["send-now", "remove"];
    case "Outcome unknown":
      return ["printed", "not-printed", "remove"];
    default:
      return [];
  }
}

/** Moonraker and PrusaLink printers: the hosts a send can upload to. */
export const sendable = (printer: SendPrinter) =>
  printer.integration?.kind === "moonraker" || printer.integration?.kind === "prusalink";

/**
 * Why "Upload and start print" is not offered for this printer, or null when it is. The
 * route starts a print on an idle printer: Ready, or its last print has finished.
 */
export function startRefusal(printer: SendPrinter): string | null {
  if (printer.state.execution === "idle") return null;
  return `It is ${STATE_WORDS[printer.state.execution]}; a print starts only on a ready printer.`;
}

const STATE_WORDS: Readonly<Record<SendPrinter["state"]["execution"], string>> = {
  printing: "printing",
  paused: "paused",
  idle: "idle",
  failed: "in a failed state",
  unreachable: "unreachable",
  not_set_up: "not set up",
  manual: "not watched",
  unknown: "in an unknown state",
};

/**
 * Add to queue: one multipart form for `printer`, bound to Build `build`. Nothing is sent
 * to the printer; Send now does that. The answer is checked against the generated schema.
 */
export async function addSend(
  build: number,
  printer: string,
  file: File,
  start: boolean,
): Promise<QueuedSendReceipt> {
  const path = routePath("addQueuedSend", {});
  const form = new FormData();
  form.set("printer_id", printer);
  form.set("profile_id", String(build));
  form.set("start", String(start));
  form.set("wait_for_idle", String(start));
  form.set("file", file, file.name);
  const response = await fetchCore(path, {
    method: "POST",
    headers: { accept: "application/json" },
    body: form,
  });
  const parsed = routes.addQueuedSend.response.safeParse(
    await response.json().catch(() => undefined),
  );
  if (!parsed.success) throw new ContractError(path);
  return parsed.data;
}
