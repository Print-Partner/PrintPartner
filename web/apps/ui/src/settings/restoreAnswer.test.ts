import { afterEach, describe, expect, it, vi } from "vitest";
import { fetchCore } from "../core/client";
import { RequestContractError } from "../core/rust";
import { restoreAnswer, restoreVerdict, type RestoreVerdict } from "./restoreAnswer";

/** The only answers that allow a retry: [status, code or undefined, verdict]. */
const ALLOWED: readonly [number, string | undefined, RestoreVerdict][] = [
  [400, undefined, "unchanged"],
  [401, undefined, "unchanged"],
  [403, undefined, "unchanged"],
  [409, undefined, "unchanged"],
  [422, undefined, "unchanged"],
  [404, "restore_not_found", "choose-again"],
  [503, "restore_busy", "unchanged"],
  [500, "restore_rolled_back", "choose-again"],
];

const CODES = [
  undefined,
  "restore_not_found",
  "restore_busy",
  "restore_rolled_back",
  "restart_required",
  "outcome_unknown",
  "transaction_unavailable",
  "",
  "RESTORE_BUSY",
  " restore_busy",
];

describe("restoreVerdict", () => {
  it("allows a retry only for the listed answers, over every status, code and body", () => {
    for (let status = 100; status <= 599; status += 1) {
      for (const code of CODES) {
        for (const problem of [true, false]) {
          const allowed = ALLOWED.find(([s, c]) => s === status && c === code);
          const expected = problem && allowed ? allowed[2] : "unknown";
          expect(restoreVerdict({ kind: "answered", status, problem, code }), `${status} ${code} ${problem}`).toBe(expected);
        }
      }
    }
  });

  it("a request never sent is unchanged; a request without a readable answer is unknown", () => {
    expect(restoreVerdict({ kind: "not-sent" })).toBe("unchanged");
    expect(restoreVerdict({ kind: "no-answer" })).toBe("unknown");
    expect(restoreAnswer(new RequestContractError("/api/v2/installation/restore"))).toEqual({ kind: "not-sent" });
    expect(restoreAnswer(new TypeError("Failed to fetch"))).toEqual({ kind: "no-answer" });
  });
});

describe("restoreAnswer over the wire", () => {
  afterEach(() => vi.unstubAllGlobals());

  /** The verdict for one raw answer read through `fetchCore`. The body is bytes, not a Blob. */
  async function verdict(status: number, body: unknown, type = "application/problem+json") {
    const bytes = new TextEncoder().encode(JSON.stringify(body));
    vi.stubGlobal("fetch", () => Promise.resolve(new Response(bytes, { status, headers: { "content-type": type } })));
    const failure = await fetchCore("/api/v2/installation/restore", { method: "POST" }).then(
      () => undefined,
      (error: unknown) => error,
    );
    return restoreVerdict(restoreAnswer(failure));
  }
  const problem = (status: number, extra: Record<string, unknown> = {}) => ({
    type: "urn:print-partner:problem:conflict",
    title: "Conflict",
    status,
    detail: "Refused",
    ...extra,
  });

  it("reads the core's own refusals as listed", async () => {
    expect(await verdict(409, problem(409))).toBe("unchanged");
    expect(await verdict(503, problem(503, { code: "restore_busy" }))).toBe("unchanged");
    expect(await verdict(404, problem(404, { code: "restore_not_found" }))).toBe("choose-again");
  });

  it.each([
    ["null", null],
    ["a number", 409],
    ["an object", { code: "restore_busy" }],
    ["an array", ["restore_busy"]],
    ["true", true],
    ["an empty string", ""],
    ["an unknown string", "restore_maybe"],
  ])("code as %s is unknown on every allowed status", async (_, code) => {
    for (const status of [400, 401, 403, 404, 409, 422, 500, 503]) {
      expect(await verdict(status, problem(status, { code })), `${status}`).toBe("unknown");
    }
  });

  it("a status member that disagrees, a non-problem body or a body that is not JSON is unknown", async () => {
    expect(await verdict(409, problem(400))).toBe("unknown");
    expect(await verdict(503, problem(503, { code: "restore_busy" }), "application/json")).toBe("unknown");
    expect(await verdict(409, problem(409, { status: "409" }))).toBe("unknown");
    expect(await verdict(409, problem(409, { type: "about:blank" }))).toBe("unknown");
    expect(await verdict(409, "Conflict", "text/plain")).toBe("unknown");
  });
});
