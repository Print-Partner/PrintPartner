import { afterEach, describe, expect, it, vi } from "vitest";
import { fetchCore } from "../core/client";
import { ContractError, RequestContractError } from "../core/rust";
import { restoreAnswer, restoreVerdict, type RestoreAnswer, type RestoreVerdict } from "./restoreAnswer";

/**
 * The whole allowlist, written out once: `status code` (code `-` for none) to its verdict.
 * Each entry needs the core's own problem body. Every other combination must be `unknown`.
 */
const ALLOWED: Readonly<Record<string, RestoreVerdict>> = {
  "400 -": "unchanged",
  "401 -": "unchanged",
  "403 -": "unchanged",
  "409 -": "unchanged",
  "422 -": "unchanged",
  "404 restore_not_found": "choose-again",
  "500 restore_rolled_back": "choose-again",
  "503 restore_busy": "unchanged",
};

const CODES = [
  undefined,
  "restore_not_found",
  "restore_rolled_back",
  "restore_busy",
  "restart_required",
  "outcome_unknown",
  "transaction_unavailable",
  "draft_changed",
  "",
  "RESTORE_BUSY",
];

const STATUSES = Array.from({ length: 500 }, (_, index) => 100 + index);

describe("restoreVerdict", () => {
  it("allows only the listed status and code pairs, and only with the core's problem body", () => {
    const allowed: string[] = [];
    for (const status of STATUSES) {
      for (const code of CODES) {
        for (const problem of [true, false]) {
          const verdict = restoreVerdict({ kind: "answered", status, problem, code });
          const key = `${status} ${code ?? "-"}`;
          const expected = problem ? (ALLOWED[key] ?? "unknown") : "unknown";
          expect(verdict, `${key} problem=${problem}`).toBe(expected);
          if (verdict !== "unknown") allowed.push(key);
        }
      }
    }
    expect(allowed.sort()).toEqual(Object.keys(ALLOWED).sort());
  });

  it("a request never sent is unchanged; a request that left without a readable answer is unknown", () => {
    expect(restoreVerdict({ kind: "not-sent" })).toBe("unchanged");
    expect(restoreVerdict({ kind: "no-answer" })).toBe("unknown");
  });

  it("reads what the restore send throws", () => {
    expect(restoreAnswer(new RequestContractError("/api/v2/installation/restore"))).toEqual({ kind: "not-sent" });
    expect(restoreAnswer(new TypeError("Failed to fetch"))).toEqual({ kind: "no-answer" });
    expect(restoreAnswer(new DOMException("The operation was aborted.", "AbortError"))).toEqual({ kind: "no-answer" });
    expect(restoreAnswer(new Error("anything else"))).toEqual({ kind: "no-answer" });
  });
});

describe("restoreVerdict over real answers read by fetchCore", () => {
  afterEach(() => vi.unstubAllGlobals());

  const PATH = "/api/v2/installation/restore";
  const body = (status: number, extra: Record<string, unknown> = {}) =>
    JSON.stringify({ type: "about:blank", title: "Refused", status, detail: "Request is invalid", ...extra });

  async function verdictOf(response: Response): Promise<RestoreVerdict> {
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve(response)));
    const failure: unknown = await fetchCore(PATH, { method: "POST" }).then(
      () => undefined,
      (error: unknown) => error,
    );
    return restoreVerdict(restoreAnswer(failure));
  }

  const PROBLEM = { "content-type": "application/problem+json" };
  const cases: [string, () => Response, RestoreVerdict][] = [
    ["the core's 400 problem", () => new Response(body(400), { status: 400, headers: PROBLEM }), "unchanged"],
    [
      "the core's 400 problem with a charset",
      () => new Response(body(400), { status: 400, headers: { "content-type": "application/problem+json; charset=utf-8" } }),
      "unchanged",
    ],
    ["a 400 sent as application/json", () => new Response(body(400), { status: 400, headers: { "content-type": "application/json" } }), "unknown"],
    ["a 400 with no content type", () => new Response(new Blob([body(400)]), { status: 400 }), "unknown"],
    ["a 400 HTML page", () => new Response("<html>Bad Request</html>", { status: 400, headers: { "content-type": "text/html" } }), "unknown"],
    ["a 400 problem whose status says 503", () => new Response(body(503), { status: 400, headers: PROBLEM }), "unknown"],
    ["a 400 problem whose status is a string", () => new Response(body(400, { status: "400" }), { status: 400, headers: PROBLEM }), "unknown"],
    ["a 400 problem body that is an array", () => new Response("[400]", { status: 400, headers: PROBLEM }), "unknown"],
    ["a 400 problem body cut off mid-JSON", () => new Response(body(400).slice(0, 20), { status: 400, headers: PROBLEM }), "unknown"],
    ["an empty 400", () => new Response(null, { status: 400, headers: PROBLEM }), "unknown"],
    [
      "a 400 whose body stream breaks",
      () =>
        new Response(
          new ReadableStream({
            start(controller) {
              controller.error(new TypeError("network error"));
            },
          }),
          { status: 400, headers: PROBLEM },
        ),
      "unknown",
    ],
    ["the core's restore_busy", () => new Response(body(503, { code: "restore_busy" }), { status: 503, headers: PROBLEM }), "unchanged"],
    ["restore_busy sent as application/json", () => new Response(body(503, { code: "restore_busy" }), { status: 503, headers: { "content-type": "application/json" } }), "unknown"],
    ["a code-less 503 problem", () => new Response(body(503), { status: 503, headers: PROBLEM }), "unknown"],
    ["a 300", () => new Response(null, { status: 300 }), "unknown"],
    ["a 302 without Location", () => new Response(null, { status: 302 }), "unknown"],
    ["a 300 carrying a 400 problem body", () => new Response(body(400), { status: 300, headers: PROBLEM }), "unknown"],
    ["a 418 problem", () => new Response(body(418), { status: 418, headers: PROBLEM }), "unknown"],
    ["a 429 problem", () => new Response(body(429), { status: 429, headers: PROBLEM }), "unknown"],
  ];

  it.each(cases)("%s", async (_, response, expected) => {
    expect(await verdictOf(response())).toBe(expected);
  });

  it("a 200 whose body fails the contract is unknown", () => {
    const answer: RestoreAnswer = restoreAnswer(new ContractError(PATH));
    expect(answer).toEqual({ kind: "no-answer" });
    expect(restoreVerdict(answer)).toBe("unknown");
  });
});
