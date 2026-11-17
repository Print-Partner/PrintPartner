import type { DeductionView } from "@print-partner/rust-client";
import { Link } from "react-router";
import { readCore } from "../core/rust";

/**
 * `GET /api/v2/spoolman/deductions`: the newest 100 deductions of every Build, newest first.
 * It reads Print Partner's own rows and never asks Spoolman. The caller keeps one Build's
 * rows and enables the read only while the Spoolman Module is on.
 */
export const deductionsQuery = {
  queryKey: ["core", "spoolman-deductions"],
  queryFn: ({ signal }: { signal: AbortSignal }) => readCore("listSpoolmanDeductions", {}, signal),
  refetchOnReconnect: false,
};

const STATE: Record<string, string> = {
  pending: "waiting to send",
  sending: "sending",
  applied: "applied",
  unknown: "outcome unknown",
  failed: "failed",
  dismissed: "dismissed",
};

const SKIP: Record<string, string> = {
  no_grams: "the print recorded no grams",
  no_pinned_spool: "no spool is pinned",
  integration_missing: "its Spoolman integration is not set up",
  integration_disabled: "its Spoolman integration is turned off",
  integrations_unreadable: "the Spoolman integrations could not be read",
  module_setting_unreadable: "the Spoolman Module setting could not be read",
};

const count = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;
const skipWhy = (reason: string | null) =>
  (reason && SKIP[reason]) ?? `the core gave the reason "${reason ?? "none"}"`;

/** The newest deduction from one of `spoolIds`. `rows` is newest first, as the route sends. */
export function newestOnSpools(rows: DeductionView[], spoolIds: number[]) {
  return rows.find((row) => row.spool_id !== null && spoolIds.includes(row.spool_id));
}

/** One filament row's newest deduction: grams, spool, state and time. */
export function DeductionFact({ deduction }: { deduction: DeductionView }) {
  const at = new Date(deduction.updated_at);
  const grams = deduction.grams === null ? "Grams not known" : `${deduction.grams} g`;
  return (
    <div className="pp-fact">
      <dt>Last deduction</dt>
      <dd>
        {grams} from spool {deduction.spool_id}
        <span className="pp-fact-detail">
          {STATE[deduction.state] ?? deduction.state},{" "}
          <time dateTime={at.toISOString()}>
            {new Intl.DateTimeFormat(undefined, {
              dateStyle: "medium",
              timeStyle: "short",
            }).format(at)}
          </time>
        </span>
      </dd>
    </div>
  );
}

/** "1 deduction was skipped because no spool is pinned." Null when none was skipped. */
export function skippedText(rows: DeductionView[]): string | null {
  const skipped = rows.filter((row) => row.state === "skipped");
  if (skipped.length === 0) return null;
  const byReason = new Map<string | null, number>();
  for (const row of skipped) byReason.set(row.reason, (byReason.get(row.reason) ?? 0) + 1);
  const head = count(skipped.length, "deduction was", "deductions were");
  if (byReason.size === 1) return `${head} skipped because ${skipWhy(skipped[0]!.reason)}.`;
  const parts = [...byReason].map(([reason, n]) => `${n} because ${skipWhy(reason)}`);
  return `${head} skipped: ${parts.join(", ")}.`;
}

/**
 * This Build's deductions that may have left Spoolman's numbers wrong (`unknown` and
 * `failed`), and the skipped ones in words. The answers live on the Farm log.
 */
export function DeductionNotices({ rows }: { rows: DeductionView[] }) {
  const unknown = rows.filter((row) => row.state === "unknown");
  const failed = rows.filter((row) => row.state === "failed");
  const grams = [...unknown, ...failed].reduce((sum, row) => sum + (row.grams ?? 0), 0);
  const phrases = [
    unknown.length > 0 &&
      `${count(unknown.length, "deduction has", "deductions have")} an unknown outcome`,
    failed.length > 0 && `${count(failed.length, "deduction", "deductions")} failed`,
  ].filter(Boolean);
  const skipped = skippedText(rows);
  return (
    <>
      {phrases.length > 0 && (
        <div className="pp-banner" data-kind={failed.length > 0 ? "error" : undefined}>
          <p>
            {phrases.join(" and ")}: {grams} g. Spoolman's numbers may be off by up to {grams} g.{" "}
            <Link to="/farm/log" className="pp-link">
              Answer in the Farm log
            </Link>
          </p>
        </div>
      )}
      {skipped && <p className="pp-muted">{skipped}</p>}
    </>
  );
}
