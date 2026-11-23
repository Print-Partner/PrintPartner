import { readCore } from "./rust";

/**
 * `GET /api/v2/spoolman/deductions`: the newest 100 deductions of every Build, newest first.
 * It reads Print Partner's own rows and never asks Spoolman. The Build Overview's Filament
 * section and the Farm log share this cache, so an answer given on the Farm log is what the
 * Filament section shows next.
 */
export const deductionsQuery = {
  queryKey: ["core", "spoolman-deductions"],
  queryFn: ({ signal }: { signal: AbortSignal }) =>
    readCore("listSpoolmanDeductions", {}, signal, undefined, { rejectRedirect: true }),
  refetchOnReconnect: false,
};

/** A deduction's `state` in words. */
export const DEDUCTION_STATE: Record<string, string> = {
  pending: "waiting to send",
  sending: "sending",
  applied: "applied",
  unknown: "outcome unknown",
  failed: "failed",
  skipped: "skipped",
  dismissed: "dismissed",
};

/** A `skipped` deduction's `reason`, as the end of "skipped because …". */
export const SKIP_REASON: Record<string, string> = {
  no_grams: "the print recorded no grams",
  no_pinned_spool: "no spool is pinned",
  integration_missing: "its Spoolman integration is not set up",
  integration_disabled: "its Spoolman integration is turned off",
  integrations_unreadable: "the Spoolman integrations could not be read",
  module_setting_unreadable: "the Spoolman Module setting could not be read",
};
