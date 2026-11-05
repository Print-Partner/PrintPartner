import { dtoSchemas, farmPrinterListSchema } from "@print-partner/rust-client";
import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import { z } from "zod";

/**
 * The frames of `GET /ws/printers` (docs/printer-stream.md). The generated client covers
 * HTTP routes only, so the envelope is declared here; the printer state and reason reuse
 * the generated schemas, so they cannot drift from the farm reads.
 */
const reasonSchema = farmPrinterListSchema.shape.printers.element.shape.reason.unwrap();

const streamPrinterSchema = z.object({
  id: z.string(),
  name: z.string(),
  display: dtoSchemas.PrinterState,
  reason: reasonSchema.optional(),
  progress_percent: z.number().int().min(0).max(100).optional(),
  last_heard_at: z.string().optional(),
});

const envelope = { stream: z.string(), seq: z.number().int().min(0) };
const frameSchema = z.discriminatedUnion("type", [
  z.object({
    type: z.literal("snapshot"),
    ...envelope,
    heartbeat_seconds: z.number().int().min(1),
    printers: z.array(streamPrinterSchema),
  }),
  z.object({ type: z.literal("printer"), ...envelope, printer: streamPrinterSchema }),
  z.object({ type: z.literal("removed"), ...envelope, id: z.string() }),
  z.object({ type: z.literal("heartbeat"), ...envelope }),
]);

export type StreamPrinter = z.output<typeof streamPrinterSchema>;

export type PrinterStreamView = {
  /** `live` while the open socket is current; `down` once it closes or goes quiet. */
  status: "connecting" | "live" | "down";
  /** Every printer from the last snapshot and the events after it; null before the first. */
  printers: readonly StreamPrinter[] | null;
  /** When the last frame arrived: the time the shown state was last known current. */
  receivedAt: number | null;
};

/** Seconds before reconnect attempts 1, 2, 3, ... (the last repeats). */
const RECONNECT_DELAYS_S = [1, 2, 5, 10, 30] as const;
/** Missed heartbeats, plus slack, before a quiet socket counts as down. */
const QUIET_HEARTBEATS = 2;
const QUIET_SLACK_MS = 5_000;

const INITIAL: PrinterStreamView = { status: "connecting", printers: null, receivedAt: null };
const StreamContext = createContext<PrinterStreamView>(INITIAL);

type Resume = { stream: string; seq: number };

function streamUrl(resume: Resume | null): string {
  const url = new URL("/ws/printers", window.location.href);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  if (resume) {
    url.searchParams.set("stream", resume.stream);
    url.searchParams.set("since", String(resume.seq));
  }
  return url.toString();
}

/** The seq a non-snapshot frame must carry to follow `resume`, or null when it cannot. */
function expectedSeq(resume: Resume | null, frame: { type: string; stream: string }) {
  if (!resume || resume.stream !== frame.stream) return null;
  return frame.type === "heartbeat" ? resume.seq : resume.seq + 1;
}

/**
 * Holds one `/ws/printers` socket for the whole frame. The snapshot, then each event,
 * updates the printers; no screen polls for printer state. A closed or quiet socket
 * reconnects with `stream` and `since`, so the server replays what was missed or sends a
 * fresh snapshot. While it is down the last-known printers stay, with `receivedAt`.
 */
export function PrinterStreamProvider({ children }: { children: ReactNode }) {
  const [view, setView] = useState<PrinterStreamView>(INITIAL);

  useEffect(() => {
    let socket: WebSocket | null = null;
    let resume: Resume | null = null;
    let attempt = 0;
    let quietTimer: ReturnType<typeof setTimeout> | undefined;
    let retryTimer: ReturnType<typeof setTimeout> | undefined;
    let heartbeatMs = 15_000;
    let stopped = false;

    /**
     * Stops trusting `current` at once: marks the stream down and schedules the reconnect
     * without waiting for the close handshake, which an unreachable peer never finishes.
     * Later callbacks from that socket are ignored.
     */
    const drop = (current: WebSocket) => {
      if (socket !== current) return;
      socket = null;
      clearTimeout(quietTimer);
      setView((v) => (v.status === "down" ? v : { ...v, status: "down" }));
      current.close();
      if (stopped) return;
      const delay = RECONNECT_DELAYS_S[Math.min(attempt, RECONNECT_DELAYS_S.length - 1)]!;
      attempt += 1;
      retryTimer = setTimeout(connect, delay * 1000);
    };

    const watchQuiet = (current: WebSocket) => {
      clearTimeout(quietTimer);
      quietTimer = setTimeout(() => drop(current), heartbeatMs * QUIET_HEARTBEATS + QUIET_SLACK_MS);
    };

    function connect() {
      const current = new WebSocket(streamUrl(resume));
      socket = current;
      // A socket that never opens counts as quiet too.
      watchQuiet(current);
      current.onopen = () => {
        if (socket === current) watchQuiet(current);
      };
      current.onmessage = (message) => {
        if (socket !== current) return;
        let raw: unknown;
        try {
          raw = JSON.parse(String(message.data));
        } catch {
          raw = undefined;
        }
        const parsed = frameSchema.safeParse(raw);
        // A frame the contract does not describe, or a gap in `seq`, means this socket's
        // view can no longer be trusted: drop it and resume from the last good event.
        if (!parsed.success) return drop(current);
        const frame = parsed.data;
        if (frame.type === "snapshot") heartbeatMs = frame.heartbeat_seconds * 1000;
        else if (frame.seq !== expectedSeq(resume, frame)) return drop(current);
        resume = { stream: frame.stream, seq: frame.seq };
        attempt = 0;
        watchQuiet(current);
        const now = Date.now();
        setView((v) => {
          switch (frame.type) {
            case "snapshot":
              return { status: "live", printers: frame.printers, receivedAt: now };
            case "printer": {
              const list = v.printers ?? [];
              const known = list.some((p) => p.id === frame.printer.id);
              const printers = known
                ? list.map((p) => (p.id === frame.printer.id ? frame.printer : p))
                : [...list, frame.printer];
              return { status: "live", printers, receivedAt: now };
            }
            case "removed":
              return {
                status: "live",
                printers: (v.printers ?? []).filter((p) => p.id !== frame.id),
                receivedAt: now,
              };
            case "heartbeat":
              return { ...v, status: "live", receivedAt: now };
          }
        });
      };
      current.onclose = () => drop(current);
    }

    connect();
    return () => {
      stopped = true;
      clearTimeout(quietTimer);
      clearTimeout(retryTimer);
      const open = socket;
      socket = null;
      open?.close();
    };
  }, []);

  return <StreamContext value={view}>{children}</StreamContext>;
}

export const usePrinterStream = () => useContext(StreamContext);
