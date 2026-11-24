import type { SendQueue } from "@print-partner/rust-client";
import { readCore } from "./rust";

export type QueuedSend = SendQueue["items"][number];

/** While a listed send is Sending, read the queue again this often. */
export const SENDING_POLL_MS = 3_000;

export const sendQueueQuery = {
  queryKey: ["core", "send-queue"],
  queryFn: ({ signal }: { signal: AbortSignal }) => readCore("readSendQueue", {}, signal),
} as const;

type SendStatus = "Not sent" | "Sending" | "Sent" | "Failed" | "Outcome unknown";

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
