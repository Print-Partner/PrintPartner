import { readFileSync } from "node:fs";
import { expect, it } from "vitest";
import { moonrakerHostSnapshot } from "./moonraker-host-snapshot.js";

const corpusFile = new URL("../../../../fixtures/drivers/moonraker/host-snapshot-parity.json", import.meta.url);
const { cases } = JSON.parse(readFileSync(corpusFile, "utf8")) as {
  cases: Array<{ name: string; setting: string | null; expected: unknown }>;
};
// The Rust-only runtime projects the same setting in pp-storage::printer_hosts
// and replays this corpus (GRE-449).
it.each(cases)("ticket_t_4_watcher_proof host snapshot parity: $name", ({ setting, expected }) => {
  expect(moonrakerHostSnapshot(setting)).toEqual(expected);
});

it("ticket_t_4 unrelated integrations do not disable a bounded Moonraker snapshot", () => {
  const other = Array.from({ length: 129 }, (_, id) => ({ id, type: "spoolman" }));
  const host = {
    id: "printer",
    type: "moonraker",
    config: { baseUrl: "http://printer", apiKey: "test", enabled: false },
  };
  expect(moonrakerHostSnapshot(JSON.stringify([...other, null, host]))).toEqual([
    { id: "printer", base_url: "http://printer", api_key: "test", enabled: false },
  ]);
  expect(moonrakerHostSnapshot(JSON.stringify(other))).toEqual([]);
});
it("ticket_t_4 Moonraker host budget sends ids only past the cap", () => {
  const hosts = Array.from({ length: 128 }, (_, id) => ({
    id: String(id),
    type: "moonraker",
    config: { base_url: "http://printer", api_key: "secret" },
  }));
  expect(moonrakerHostSnapshot(JSON.stringify(hosts))).toHaveLength(128);
  const over = moonrakerHostSnapshot(JSON.stringify([...hosts, { ...hosts[0], id: "128" }]));
  expect(over).toHaveLength(129);
  expect(over?.[128]).toEqual({ id: "128", base_url: "", api_key: null, enabled: false });
  expect(over?.every((host) => host.base_url === "" && host.api_key === null)).toBe(true);
  expect(moonrakerHostSnapshot("{}")).toBeNull();
  expect(moonrakerHostSnapshot("{")).toBeNull();
  expect(moonrakerHostSnapshot("")).toEqual([]);
});
it("projects an oversized Moonraker row before the snapshot is serialized", () => {
  const healthy = {
    id: "ok",
    type: "moonraker",
    config: { base_url: "http://printer", api_key: "secret" },
  };
  const maxUrl = {
    id: "max",
    type: "moonraker",
    config: { base_url: "h".repeat(4096), api_key: "k" },
  };
  const hugeUrl = {
    id: "big",
    type: "moonraker",
    config: { base_url: "h".repeat(4097), api_key: "secret" },
  };
  const hugeKey = {
    id: "key",
    type: "moonraker",
    config: { base_url: "http://printer", api_key: "k".repeat(4097) },
  };
  const wide = {
    id: "wide",
    type: "moonraker",
    config: { base_url: "é".repeat(2049), api_key: "k" },
  };
  expect(moonrakerHostSnapshot(JSON.stringify([healthy, maxUrl, hugeUrl, hugeKey, wide]))).toEqual([
    { id: "ok", base_url: "http://printer", api_key: "secret", enabled: true },
    { id: "max", base_url: "h".repeat(4096), api_key: "k", enabled: true },
    { id: "big", base_url: "", api_key: null, enabled: true },
    { id: "key", base_url: "", api_key: null, enabled: true },
    { id: "wide", base_url: "", api_key: null, enabled: true },
  ]);
});

it.each([undefined, null, 123, true, {}, [], "", "x".repeat(257), "é".repeat(129)])(
  "rejects a malformed Moonraker id %j instead of fabricating a polling identity",
  (id) => {
    const healthy = { id: "healthy", type: "moonraker", config: { base_url: "http://printer" } };
    const invalid = { id, type: "moonraker", config: { base_url: "http://other-printer" } };
    expect(moonrakerHostSnapshot(JSON.stringify([healthy, invalid]))).toBeNull();
    const overflow = Array.from({ length: 128 }, (_, index) => ({ ...healthy, id: String(index) }));
    expect(moonrakerHostSnapshot(JSON.stringify([...overflow, invalid]))).toBeNull();
  },
);
it("accepts a nonempty Moonraker id at the UTF-8 byte limit", () => {
  const id = "é".repeat(128);
  expect(moonrakerHostSnapshot(JSON.stringify([{ id, type: "moonraker" }]))?.[0]?.id).toBe(id);
});

it("bounds an escaped ID-only overflow response and rejects larger inventories", () => {
  const hosts = Array.from({ length: 2048 }, (_, index) => ({
    id: `${String(index).padStart(4, "0")}${"\u0001".repeat(252)}`,
    type: "moonraker",
  }));
  const projected = moonrakerHostSnapshot(JSON.stringify(hosts));
  expect(projected).toHaveLength(2048);
  const rustLimit = 128 * (6 * (256 + 2 * 4096) + 128) + 2;
  expect(Buffer.byteLength(JSON.stringify(projected), "utf8")).toBeLessThan(rustLimit);
  expect(
    moonrakerHostSnapshot(JSON.stringify([...hosts, { id: "new", type: "moonraker" }])),
  ).toBeNull();
});
it("keeps oversized enabled rows unavailable and actual disabled rows disabled", () => {
  const enabled = { id: "big", type: "moonraker", config: { base_url: "h".repeat(4097) } };
  const disabled = { ...enabled, id: "disabled", config: { ...enabled.config, enabled: false } };
  expect(moonrakerHostSnapshot(JSON.stringify([enabled, disabled]))).toEqual([
    { id: "big", base_url: "", api_key: null, enabled: true },
    { id: "disabled", base_url: "", api_key: null, enabled: false },
  ]);
});
