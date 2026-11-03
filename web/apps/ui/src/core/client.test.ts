import { afterEach, describe, expect, it, vi } from "vitest";
import { CoreError, checkCoreHealth, fetchCore } from "./client";

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("fetchCore", () => {
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

  it("turns a non-2xx answer into a CoreError naming the status", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("", { status: 503 })));

    const error = await fetchCore("/health").catch((caught: unknown) => caught);

    expect(error).toBeInstanceOf(CoreError);
    expect((error as CoreError).message).toContain("503");
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
      vi
        .fn()
        .mockResolvedValue(
          new Response("<!doctype html>", {
            status: 200,
            headers: { "content-type": "text/html" },
          }),
        ),
    );

    await expect(checkCoreHealth()).rejects.toBeInstanceOf(CoreError);
  });
});
