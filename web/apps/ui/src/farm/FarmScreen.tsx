import type { FarmPrinterList } from "@print-partner/rust-client";
import { useQuery, type UseQueryResult } from "@tanstack/react-query";
import { useEffect, useRef } from "react";
import { Link } from "react-router";
import { useModule } from "../core/modules";
import { readCore } from "../core/rust";
import { useConnection } from "../frame/connection";
import { queryScreenState } from "../screen/queryState";
import { LoadedAt, ScreenTemplate, type ScreenState } from "../screen/ScreenTemplate";
import { PrinterStatus } from "../status/PrinterStatus";
import { usePrinterStream, type PrinterStreamView, type StreamPrinter } from "./printerStream";
import { BED_WORDS, EXECUTION_WORDS, reasonText, type IntegrationKind } from "./words";

type FarmPrinter = FarmPrinterList["printers"][number];

/**
 * `GET /api/v2/printers` once, for each printer's model and integration. State after that
 * comes from `/ws/printers`, so this read never polls; it reads again only when the stream
 * names a printer it does not contain.
 */
const printersQuery = {
  queryKey: ["core", "printers"],
  queryFn: ({ signal }: { signal: AbortSignal }) => readCore("listPrinters", {}, signal),
  staleTime: Infinity,
} as const;

/** One row: the stream's live state over the farm read's details. */
type Row = {
  id: string;
  name: string;
  model: string | null;
  kind: IntegrationKind | null;
  state: FarmPrinter["state"];
  reason: FarmPrinter["reason"];
  progress: number | null;
  lastHeardAt: string | null;
};

/** Execution states in which the core is not hearing from the printer. */
const NOT_HEARD = new Set<FarmPrinter["state"]["execution"]>(["unreachable", "not_set_up", "manual"]);

/**
 * The stream sends a printer only when its state changes, so a printer that keeps
 * answering was last heard as of the stream's last frame, not at its last change.
 */
function heardAt(p: StreamPrinter, receivedAt: number | null): string | null {
  // A reason means the printer is not answering (awaiting status, unwatched, unreachable).
  if (p.reason || NOT_HEARD.has(p.display.execution) || receivedAt === null || !p.last_heard_at) {
    return p.last_heard_at ?? null;
  }
  return new Date(Math.max(Date.parse(p.last_heard_at) || 0, receivedAt)).toISOString();
}

/**
 * Once the stream has sent a snapshot it decides which printers exist and their state;
 * the farm read adds the model and integration. Before that, the farm read alone.
 */
function farmRows(read: FarmPrinter[], stream: PrinterStreamView): Row[] {
  const details = new Map(read.map((p) => [p.id, p]));
  if (!stream.printers) {
    return read.map((p) => ({
      id: p.id,
      name: p.name,
      model: p.model,
      kind: p.integration?.kind ?? null,
      state: p.state,
      reason: p.reason,
      progress: p.progress_percent,
      lastHeardAt: p.last_heard_at,
    }));
  }
  return stream.printers.map((p) => {
    const detail = details.get(p.id);
    return {
      id: p.id,
      name: p.name,
      model: detail?.model ?? null,
      kind: detail?.integration?.kind ?? null,
      state: p.display,
      reason: p.reason ?? null,
      progress: p.progress_percent ?? null,
      lastHeardAt: heardAt(p, stream.receivedAt),
    };
  });
}

function Time({ iso }: { iso: string }) {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return <>{iso}</>;
  const text = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(date);
  return <time dateTime={date.toISOString()}>{text}</time>;
}

/** Says when the live printer stream is not current, and from when the shown state is. */
function StreamNotice({ stream, readAt }: { stream: PrinterStreamView; readAt: number | undefined }) {
  if (stream.status === "live") return null;
  if (stream.status === "connecting" && stream.printers === null) {
    return <p className="pp-muted">Connecting to live printer updates.</p>;
  }
  const at = stream.receivedAt ?? readAt;
  return (
    <div role="status" className="pp-banner" data-kind="stale">
      <p>
        Live printer updates are down, so these are the last-known states.{" "}
        <LoadedAt prefix="Last known at" at={at} /> Print Partner keeps trying to reconnect.
      </p>
    </div>
  );
}

function PrintersTable({ rows }: { rows: Row[] }) {
  return (
    <table className="pp-farm-table" aria-label="Printers">
      <thead>
        <tr>
          <th scope="col">Printer</th>
          <th scope="col">Status</th>
          <th scope="col">Execution</th>
          <th scope="col">Bed</th>
          <th scope="col">Last heard</th>
        </tr>
      </thead>
      <tbody>
        {rows.map((row) => (
          <tr key={row.id} data-printer={row.id}>
            <th scope="row">
              {row.name}
              <span className="pp-fact-detail">{row.model ?? "Model not loaded"}</span>
            </th>
            <td>
              <PrinterStatus state={row.state.display} />
              {row.reason && (
                <span className="pp-fact-detail">{reasonText(row.reason, row.kind)}</span>
              )}
            </td>
            <td>
              {EXECUTION_WORDS[row.state.execution]}
              {row.progress !== null && <span className="num"> {row.progress}%</span>}
            </td>
            <td>{BED_WORDS[row.state.bed_clear]}</td>
            <td>{row.lastHeardAt ? <Time iso={row.lastHeardAt} /> : <span className="pp-muted">Never</span>}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

/** Says the farm read failed while the stream still shows printers, with a Retry. */
function DetailsNotice({ query, live }: { query: UseQueryResult<unknown>; live: boolean }) {
  if (!query.isError) return null;
  return (
    <div role="alert" className="pp-banner" data-kind="error">
      <p>
        Could not load printer models and integrations.{" "}
        {live ? "States are live from the printer stream." : "States are the last the printer stream sent."}
      </p>
      <p className="pp-banner-detail">{query.error.message}</p>
      <button type="button" className="pp-button" onClick={() => void query.refetch()}>
        Retry
      </button>
    </div>
  );
}

/**
 * Once the stream has sent a snapshot, it is what the screen shows, and its last frame
 * time is the time every banner names. Before that, the farm read alone decides.
 */
function farmState(
  stream: PrinterStreamView,
  printers: UseQueryResult<FarmPrinterList>,
  connection: ReturnType<typeof useConnection>,
): ScreenState<Row[]> {
  if (stream.printers) {
    const rows = farmRows(printers.data?.printers ?? [], stream);
    if (connection === "offline") {
      return { kind: "offline", lastData: rows, updatedAt: stream.receivedAt ?? undefined };
    }
    return rows.length === 0 ? { kind: "empty" } : { kind: "ready", data: rows };
  }
  const state = queryScreenState(printers, connection, (data) => data.printers.length === 0);
  if (state.kind === "ready") return { kind: "ready", data: farmRows(state.data.printers, stream) };
  if (state.kind === "error" || state.kind === "offline") {
    const lastData = state.lastData && farmRows(state.lastData.printers, stream);
    return { ...state, lastData };
  }
  return state;
}

/** Farm: every configured printer with one honest state, kept live by `/ws/printers`. */
export function FarmScreen() {
  const connection = useConnection();
  const farm = useModule("farm");
  const stream = usePrinterStream();
  const printers = useQuery(printersQuery);
  const asked = useRef(new Set<string>());
  const { data, isFetching, refetch } = printers;
  useEffect(() => {
    if (!stream.printers || !data || isFetching) return;
    const known = new Set(data.printers.map((p) => p.id));
    const added = stream.printers.filter((p) => !known.has(p.id) && !asked.current.has(p.id));
    if (added.length === 0) return;
    for (const p of added) asked.current.add(p.id);
    void refetch();
  }, [stream.printers, data, isFetching, refetch]);
  const state: ScreenState<Row[]> =
    farm?.state === "off"
      ? { kind: "module-off", module: "Farm", wouldShow: "every printer and its state" }
      : farmState(stream, printers, connection);
  return (
    <ScreenTemplate
      title="Farm"
      state={state}
      renderData={(rows) => (
        <>
          {state.kind !== "offline" && (
            <StreamNotice stream={stream} readAt={printers.dataUpdatedAt || undefined} />
          )}
          {stream.printers && <DetailsNotice query={printers} live={stream.status === "live"} />}
          <PrintersTable rows={rows} />
          <p>
            <Link to="/farm/match" className="pp-link">
              Match prints the app could not match
            </Link>
          </p>
          <p>
            <Link to="/farm/log" className="pp-link">
              Farm log
            </Link>
          </p>
        </>
      )}
      empty={{ message: "No printers yet. Printers you add in Settings appear here." }}
    />
  );
}
