import { printerDisplayDescriptors } from "@print-partner/rust-client";
import { LoadedAt } from "../screen/ScreenTemplate";
import { PRINTER_DISPLAY_STATES, PrinterStatus } from "../status/PrinterStatus";
import { usePrinterStream, type StreamPrinter } from "./printerStream";

/** Printer counts per display state, in the order Rust declares the states. Zeros are left out. */
function countByState(printers: readonly StreamPrinter[]) {
  return PRINTER_DISPLAY_STATES.map((state) => ({
    state,
    count: printers.filter((p) => p.display.display === state).length,
  })).filter((c) => c.count > 0);
}

/**
 * The pulse line, on every screen: printer counts by state from `/ws/printers`. It never
 * shows zeros it has not read. While the stream is down it keeps the last counts with
 * the time they were last known.
 */
export function PulseLine() {
  const { status, printers, receivedAt } = usePrinterStream();
  const counts = printers && countByState(printers);
  return (
    <section aria-label="Printer pulse" className="pp-pulse" data-stream={status}>
      {!counts ? (
        <span className="pp-pulse-empty">
          {status === "down" ? "Printer counts not loaded: live updates are down" : "Printer counts not loaded"}
        </span>
      ) : (
        <>
          <ul className="pp-pulse-counts" aria-live="polite">
            {counts.length === 0 ? (
              <li>No printers</li>
            ) : (
              counts.map(({ state, count }) => (
                <li key={state} aria-label={`${count} ${printerDisplayDescriptors[state].label}`}>
                  <span className="num">{count}</span> <PrinterStatus state={state} />
                </li>
              ))
            )}
          </ul>
          {status === "down" && receivedAt !== null && (
            <span className="pp-pulse-stale">
              <LoadedAt prefix="Live updates down. Last known" at={receivedAt} />
            </span>
          )}
        </>
      )}
    </section>
  );
}
