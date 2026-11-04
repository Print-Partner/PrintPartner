/// <reference types="node" />
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import {
  printerDisplayDescriptors,
  printerDisplayDescriptorSchema,
} from "@print-partner/rust-client";
import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import {
  PRINTER_DISPLAY_STATES,
  PrinterStatus,
  printerStatusPresentation,
  type PrinterDisplayTable,
} from "./PrinterStatus";

afterEach(cleanup);

// A path string: jsdom replaces the global URL, which node:fs refuses.
const tokens = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../index.css"), "utf8");
/** `--name: #rrggbb;` declarations in the stylesheet. */
const declared = new Map(
  [...tokens.matchAll(/--([a-z-]+):\s*(#[0-9a-f]{6})\s*;/gi)].map((match) => [match[1], match[2].toLowerCase()]),
);

function hue(hex: string): { hue: number; saturation: number } {
  const [r, g, b] = [1, 3, 5].map((start) => parseInt(hex.slice(start, start + 2), 16) / 255);
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const delta = max - min;
  const lightness = (max + min) / 2;
  const saturation = delta === 0 ? 0 : delta / (1 - Math.abs(2 * lightness - 1));
  if (delta === 0) return { hue: 0, saturation };
  const raw = max === r ? ((g - b) / delta) % 6 : max === g ? (b - r) / delta + 2 : (r - g) / delta + 4;
  return { hue: (raw * 60 + 360) % 360, saturation };
}

describe("PrinterStatus", () => {
  it("covers every state in the generated Rust enum, in Rust order", () => {
    expect(PRINTER_DISPLAY_STATES).toEqual(printerDisplayDescriptorSchema.shape.state.options);
    expect(PRINTER_DISPLAY_STATES.map((state) => printerDisplayDescriptors[state].label)).toEqual([
      "Printing",
      "Bed to clear",
      "Ready",
      "Paused",
      "Failed",
      "Unreachable",
      "Not set up",
      "Manual",
      "Unknown",
    ]);
  });

  it.each(PRINTER_DISPLAY_STATES)("renders %s with an icon and a word", (state) => {
    const { container } = render(<PrinterStatus state={state} />);
    const status = container.querySelector(".printer-status");
    expect(status).not.toBeNull();
    expect(status?.querySelector("svg[aria-hidden='true']")).not.toBeNull();
    expect(status?.textContent?.trim()).toBe(printerDisplayDescriptors[state].label);
    expect(status?.textContent?.trim()).not.toBe("");
  });

  it("refuses a state whose label is missing", () => {
    const table: PrinterDisplayTable = {
      ...printerDisplayDescriptors,
      ready: { ...printerDisplayDescriptors.ready, label: " " },
    };
    expect(() => printerStatusPresentation("ready", table)).toThrow("Printer state ready has no label");
  });

  it("colours every state with a status token, never cyan", () => {
    for (const state of PRINTER_DISPLAY_STATES) {
      const { color } = printerStatusPresentation(state);
      expect(color).toMatch(/^status-/);
      const value = declared.get(color);
      expect(value, `${color} is not defined in index.css`).toBeDefined();
      const { hue: degrees, saturation } = hue(value!);
      const cyan = saturation > 0.2 && degrees >= 170 && degrees <= 200;
      expect(cyan, `${color} ${value} is cyan`).toBe(false);
    }
  });
});
