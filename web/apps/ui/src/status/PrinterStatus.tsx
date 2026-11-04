import {
  printerDisplayDescriptors,
  type PrinterDisplayDescriptor,
} from "@print-partner/rust-client";
import {
  CircleCheck,
  CircleDashed,
  CirclePause,
  CircleQuestionMark,
  Hand,
  OctagonAlert,
  PackageOpen,
  Printer,
  WifiOff,
  type LucideIcon,
} from "lucide-react";
import type { CSSProperties } from "react";

export type PrinterDisplayState = PrinterDisplayDescriptor["state"];
export type PrinterDisplayTable = Readonly<Record<PrinterDisplayState, PrinterDisplayDescriptor>>;

// Keyed by the generated icon names, so a new Rust icon fails to typecheck here.
const ICONS: Readonly<Record<PrinterDisplayDescriptor["icon"], LucideIcon>> = {
  printer: Printer,
  "package-open": PackageOpen,
  "circle-check": CircleCheck,
  "circle-pause": CirclePause,
  "octagon-alert": OctagonAlert,
  "wifi-off": WifiOff,
  "circle-dashed": CircleDashed,
  hand: Hand,
  "circle-question-mark": CircleQuestionMark,
};

/** Every printer display state, in the order Rust declares them. */
export const PRINTER_DISPLAY_STATES = Object.keys(printerDisplayDescriptors) as PrinterDisplayState[];

/**
 * The word, icon and colour token for a state. Throws instead of rendering a
 * blank or icon-only status. `table` exists so the test can prove a missing
 * label is refused.
 */
export function printerStatusPresentation(
  state: PrinterDisplayState,
  table: PrinterDisplayTable = printerDisplayDescriptors,
) {
  const descriptor = table[state];
  const label = descriptor?.label.trim();
  if (!label) throw new Error(`Printer state ${state} has no label`);
  const Icon = ICONS[descriptor.icon];
  if (!Icon) throw new Error(`Printer state ${state} has no icon`);
  return { label, Icon, color: descriptor.color };
}

/** A printer status: always an icon plus a word, coloured by a status token. */
export function PrinterStatus({ state }: { state: PrinterDisplayState }) {
  const { label, Icon, color } = printerStatusPresentation(state);
  const style = { "--status-color": `var(--${color})` } as CSSProperties;
  return (
    <span className="printer-status" style={style}>
      <Icon className="printer-status-icon" aria-hidden="true" focusable="false" />
      <span>{label}</span>
    </span>
  );
}
