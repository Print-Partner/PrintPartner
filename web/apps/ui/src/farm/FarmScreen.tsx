import type { FarmPrinterList } from "@print-partner/rust-client";
import { useQuery } from "@tanstack/react-query";
import { useModule } from "../core/modules";
import { readCore } from "../core/rust";
import { useConnection } from "../frame/connection";
import { queryScreenState } from "../screen/queryState";
import { LoadedAt, ScreenTemplate, type ScreenState } from "../screen/ScreenTemplate";
import { PrinterStatus } from "../status/PrinterStatus";
import { usePrinterStream, type PrinterStreamView } from "./printerStream";
import { BED_WORDS, EXECUTION_WORDS, reasonText, type IntegrationKind } from "./words";

type FarmPrinter = FarmPrinterList["printers"][number];

/**
 * `GET /api/v2/printers` once, for each printer's model and integration. State after that
 * comes from `/ws/printers`, so this read never polls or refetches on its own.
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
      lastHeardAt: p.last_heard_at ?? detail?.last_heard_at ?? null,
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

/** Farm: every configured printer with one honest state, kept live by `/ws/printers`. */
export function FarmScreen() {
  const connection = useConnection();
  const farm = useModule("farm");
  const stream = usePrinterStream();
  const printers = useQuery(printersQuery);
  const state: ScreenState<FarmPrinterList> =
    farm?.state === "off"
      ? { kind: "module-off", module: "Farm", wouldShow: "every printer and its state" }
      : queryScreenState(printers, connection, (data) => farmRows(data.printers, stream).length === 0);
  return (
    <ScreenTemplate
      title="Farm"
      state={state}
      renderData={(data) => (
        <>
          <StreamNotice stream={stream} readAt={printers.dataUpdatedAt || undefined} />
          <PrintersTable rows={farmRows(data.printers, stream)} />
        </>
      )}
      empty={{ message: "No printers yet. Printers you add in Settings appear here." }}
    />
  );
}
