import type { FarmPrinterList } from "@print-partner/rust-client";

type FarmPrinter = FarmPrinterList["printers"][number];
export type PrinterReason = NonNullable<FarmPrinter["reason"]>;
export type ExecutionState = FarmPrinter["state"]["execution"];
export type BedClearState = FarmPrinter["state"]["bed_clear"];
export type IntegrationKind = NonNullable<FarmPrinter["integration"]>["kind"];

/** What the printer is doing. The summary status (icon and word) comes from Rust. */
export const EXECUTION_WORDS: Readonly<Record<ExecutionState, string>> = {
  printing: "Printing",
  paused: "Paused",
  idle: "Idle",
  failed: "Failed",
  unreachable: "Unreachable",
  not_set_up: "Not set up",
  manual: "Manual",
  unknown: "Unknown",
};

/** Whether the bed is clear: a separate fact from execution, set only by a confirmation. */
export const BED_WORDS: Readonly<Record<BedClearState, string>> = {
  clear: "Clear, confirmed",
  needs_clearing: "Needs clearing",
  unknown: "Not confirmed clear",
};

const KIND_NAMES: Readonly<Record<IntegrationKind, string>> = {
  moonraker: "Moonraker",
  prusalink: "PrusaLink",
  bambu: "Bambu",
};

/**
 * Why a printer is not watched live, in words. `kind` is the printer's integration, when
 * the farm read named one.
 */
export function reasonText(reason: PrinterReason, kind: IntegrationKind | null): string {
  switch (reason) {
    case "not_set_up":
      return "No printer integration is set up for it, so check off its prints by hand.";
    case "integration_missing":
      return "Its integration is missing or is not a printer integration, so check off its prints by hand.";
    case "monitoring_disabled":
      return "Its integration is turned off, so Print Partner does not watch it. Check off its prints by hand.";
    case "monitoring_not_supported":
      return kind === "moonraker"
        ? "Print Partner cannot watch this Moonraker host (HTTPS, a redirect, no address, or past the host limit). Check off its prints by hand."
        : `Print Partner cannot watch ${kind ? KIND_NAMES[kind] : "these"} printers yet. Check off its prints by hand.`;
    case "awaiting_status":
      return "Waiting for the printer's first status.";
    case "unreachable":
      return "The printer did not answer its last status check.";
  }
}
