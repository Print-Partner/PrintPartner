import { afterEach, describe, expect, it, vi } from "vitest";
import {
  CoreError,
  RedirectedRead,
  RedirectedWrite,
  checkCoreHealth,
  fetchCore,
  onUnauthorized,
} from "./client";

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("fetchCore", () => {
  it("tells unauthorized listeners about every 401 and whether it was a write", async () => {
    const answers = [401, 200, 401, 401];
    vi.stubGlobal(
      "fetch",
      vi.fn(() => Promise.resolve(new Response("{}", { status: answers.shift() }))),
    );
    const listener = vi.fn();
    const stop = onUnauthorized(listener);
    await expect(fetchCore("/api/v2/plans")).rejects.toBeInstanceOf(CoreError);
    await fetchCore("/health");
    await expect(fetchCore("/api/v2/plans", { method: "POST" })).rejects.toBeInstanceOf(CoreError);
    stop();
    await expect(fetchCore("/api/v2/plans")).rejects.toBeInstanceOf(CoreError);
    expect(listener.mock.calls).toEqual([[{ write: false }], [{ write: true }]]);
  });

  it("sends same-origin requests with same-origin credentials", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response("{}", { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);

    await fetchCore("/api/v1/plans", { method: "GET" });

    expect(fetchMock).toHaveBeenCalledWith("/api/v1/plans", {
      method: "GET",
      credentials: "same-origin",
    });
  });

  it.each([
    "http://127.0.0.1:18765/health",
    "//evil.example/health",
    "/\\evil.example/health",
    "/\n/evil.example/health",
    "/\t\\evil.example/health",
    "/health\r",
    "health",
    "",
  ])("rejects %j before any request", async (path) => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);

    await expect(fetchCore(path)).rejects.toThrow(TypeError);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it.each([
    undefined,
    null,
    {},
    [null, {}, { name: 42 }, { name: "" }, { name: "Other kit", profile_id: 1.5 }],
  ])("keeps only holding Build names from a refusal: %j", async (builds) => {
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValue(
          new Response(JSON.stringify({ code: "duplicate_order", builds }), { status: 409 }),
        ),
    );
    const error = await fetchCore("/api/v2/plans/2/drafts/9/accept-preview").catch(
      (caught: unknown) => caught,
    );
    expect(error).toBeInstanceOf(CoreError);
    expect((error as CoreError).conflictingBuilds).toEqual(
      Array.isArray(builds) ? [{ name: "Other kit", profileId: null }] : [],
    );
  });

  it("keeps each holding Build's ID from a duplicate-order refusal", async () => {
    const builds = [
      { profile_id: 5, name: "Other kit", order_number: "PO-7" },
      { profile_id: 0, name: "Zero kit", order_number: "PO-7" },
      { profile_id: "6", name: "Text kit", order_number: "PO-7" },
    ];
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValue(
          new Response(JSON.stringify({ code: "duplicate_order", builds }), { status: 409 }),
        ),
    );
    const error = await fetchCore("/api/v2/plans/2/drafts/9/accept-preview").catch(
      (caught: unknown) => caught,
    );
    expect((error as CoreError).conflictingBuilds).toEqual([
      { name: "Other kit", profileId: 5 },
      { name: "Zero kit", profileId: null },
      { name: "Text kit", profileId: null },
    ]);
  });

  it("keeps the fields a refusal names, in the core's order, and drops malformed entries", async () => {
    const errors = [
      { pointer: "/api_key", detail: "must be sent again" },
      { pointer: 3 },
      "x",
      { pointer: "/name", detail: "must not be empty" },
    ];
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValue(
          new Response(JSON.stringify({ detail: "Request is invalid", errors }), { status: 400 }),
        ),
    );
    const error = await fetchCore("/api/v2/integrations/i-1", { method: "PATCH" }).catch(
      (caught: unknown) => caught,
    );
    expect((error as CoreError).errors).toEqual([errors[0], errors[3]]);
    expect((error as CoreError).detail).toBe("Request is invalid");
  });

  it("turns a non-2xx answer into a CoreError naming the status", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("", { status: 503 })));

    const error = await fetchCore("/health").catch((caught: unknown) => caught);

    expect(error).toBeInstanceOf(CoreError);
    expect((error as CoreError).message).toContain("503");
  });
});

describe("fetchCore on a redirected write", () => {
  const opaqueResponse = () =>
    Object.defineProperties(new Response(null, { status: 200 }), {
      type: { value: "opaqueredirect" }, status: { value: 0 }, ok: { value: false },
    });
  const redirectedResponse = () =>
    Object.defineProperty(new Response("{}", { status: 200 }), "redirected", { value: true });

  it("sends a write manual, throws RedirectedWrite on a 307 and tells no 401 listener", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 307, headers: { location: "/elsewhere" } }));
    vi.stubGlobal("fetch", fetchMock);
    const listener = vi.fn();
    const stop = onUnauthorized(listener);
    const error = await fetchCore("/api/v2/plans", { method: "post", body: "{}" }).catch((c: unknown) => c);
    stop();
    expect(error).toBeInstanceOf(RedirectedWrite);
    expect(error).not.toBeInstanceOf(CoreError);
    expect((error as Error).message).toBe(
      "POST /api/v2/plans was answered with a redirect (307); it was not followed, so whether it applied is unknown",
    );
    expect(fetchMock.mock.calls).toEqual([
      ["/api/v2/plans", { method: "post", body: "{}", credentials: "same-origin", redirect: "manual" }],
    ]);
    expect(listener).not.toHaveBeenCalled();
  });

  it("throws RedirectedWrite on opaqueredirect (status 0) and on response.redirected", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValueOnce(opaqueResponse()).mockResolvedValueOnce(redirectedResponse()));
    const opaque = await fetchCore("/api/v2/plans/2", { method: "PUT" }).catch((c: unknown) => c);
    const followed = await fetchCore("/api/v2/plans", { method: "POST" }).catch((c: unknown) => c);
    expect(opaque).toBeInstanceOf(RedirectedWrite);
    expect((opaque as Error).message).toBe(
      "PUT /api/v2/plans/2 was answered with a redirect; it was not followed, so whether it applied is unknown",
    );
    expect(followed).toBeInstanceOf(RedirectedWrite);
    expect((followed as Error).message).toBe(
      "POST /api/v2/plans followed a redirect; its answer was not trusted, so whether it applied is unknown",
    );
  });

  it("sends a write manual even when the caller asks to follow, and reads without redirect option by default", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response("{}", { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);
    await fetchCore("/api/v2/plans", { method: "DELETE", redirect: "follow" });
    await fetchCore("/api/v2/plans");
    await fetchCore("/api/v2/plans", { method: "HEAD" });
    expect(fetchMock.mock.calls).toEqual([
      ["/api/v2/plans", { method: "DELETE", credentials: "same-origin", redirect: "manual" }],
      ["/api/v2/plans", { credentials: "same-origin" }],
      ["/api/v2/plans", { method: "HEAD", credentials: "same-origin" }],
    ]);
  });

  it("sends a read manual and throws RedirectedRead when rejectRedirect is set", async () => {
    const cases: [Response, string][] = [
      [new Response(null, { status: 302, headers: { location: "/other" } }), "GET /api/v2/plans was answered with a redirect (302); it was not followed"],
      [opaqueResponse(), "GET /api/v2/plans was answered with a redirect; it was not followed"],
      [redirectedResponse(), "GET /api/v2/plans followed a redirect; its answer was not trusted"],
    ];
    const fetchMock = vi.fn();
    for (const [res] of cases) fetchMock.mockResolvedValueOnce(res);
    vi.stubGlobal("fetch", fetchMock);
    const listener = vi.fn();
    const stop = onUnauthorized(listener);
    for (const [, expected] of cases) {
      const error = await fetchCore("/api/v2/plans", undefined, { rejectRedirect: true }).catch((c: unknown) => c);
      expect(error).toBeInstanceOf(RedirectedRead);
      expect(error).not.toBeInstanceOf(CoreError);
      expect((error as Error).message).toBe(expected);
    }
    stop();
    expect(fetchMock.mock.calls[0]).toEqual(["/api/v2/plans", { credentials: "same-origin", redirect: "manual" }]);
    expect(listener).not.toHaveBeenCalled();
  });
});

describe("checkCoreHealth", () => {
  it("accepts a JSON answer from /health", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(
        new Response("{}", { status: 200, headers: { "content-type": "application/json" } }),
      );
    vi.stubGlobal("fetch", fetchMock);

    await expect(checkCoreHealth()).resolves.toBeUndefined();
    expect(fetchMock).toHaveBeenCalledWith("/health", {
      headers: { accept: "application/json" },
      credentials: "same-origin",
    });
  });

  it("rejects an index.html fallback that answers 200", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response("<!doctype html>", {
          status: 200,
          headers: { "content-type": "text/html" },
        }),
      ),
    );

    await expect(checkCoreHealth()).rejects.toBeInstanceOf(CoreError);
  });
});

describe("CoreError.problem", () => {
  const PROBLEM = {
    type: "urn:print-partner:problem:conflict",
    title: "Conflict",
    status: 409,
    detail: "Installation has no Rust marker",
  };
  const refused = async (body: string, contentType = "application/problem+json", status = 409) => {
    // A byte array, not a Blob, so no Content-Type is implied and every runtime reads it.
    const bytes = new TextEncoder().encode(body);
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(new Response(bytes, { status, headers: { "content-type": contentType } })),
    );
    const error: unknown = await fetchCore("/api/v2/installation/backup", { method: "POST" }).catch(
      (caught: unknown) => caught,
    );
    expect(error).toBeInstanceOf(CoreError);
    return error as CoreError;
  };

  it.each([
    ["without a code", PROBLEM],
    ["with a known code", { ...PROBLEM, code: "restore_not_found" }],
    ["with field errors and extra members", { ...PROBLEM, errors: [{ pointer: "/a", detail: "x" }], builds: [] }],
  ])("is true for the core's own problem %s", async (_, body) => {
    const error = await refused(JSON.stringify(body), "application/problem+json; charset=utf-8");
    expect(error.problem).toBe(true);
    expect(error.detail).toBe(PROBLEM.detail);
  });

  it.each([
    ["a null code", { ...PROBLEM, code: null }],
    ["a number code", { ...PROBLEM, code: 409 }],
    ["an object code", { ...PROBLEM, code: { value: "restore_not_found" } }],
    ["a null detail", { ...PROBLEM, detail: null }],
    ["no title", { type: PROBLEM.type, status: 409, detail: PROBLEM.detail }],
    ["an unknown type", { ...PROBLEM, type: "about:blank" }],
    ["a status that disagrees", { ...PROBLEM, status: 400 }],
    ["a string status", { ...PROBLEM, status: "409" }],
  ])("is false for a problem with %s, and a malformed code is never read", async (_, body) => {
    const error = await refused(JSON.stringify(body));
    expect(error.problem).toBe(false);
    expect(error.code).toBeUndefined();
  });

  it.each([
    ["application/json", JSON.stringify(PROBLEM)],
    ["text/html", "<html>Conflict</html>"],
    ["application/problem+json", JSON.stringify(PROBLEM).slice(0, 40)],
    ["application/problem+json", JSON.stringify([PROBLEM])],
  ])("is false for a %s body that is not a whole problem object", async (contentType, body) => {
    expect((await refused(body, contentType)).problem).toBe(false);
  });
});
