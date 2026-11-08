import { afterEach, describe, expect, it, vi } from "vitest";
import { CoreError } from "./client";
import { ContractError, readCore, RequestContractError, writeCore } from "./rust";

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("readCore", () => {
  it("fills the generated route's path and returns the parsed body", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(json({ profile_id: 7, layers: [], extra: "stripped" }));
    vi.stubGlobal("fetch", fetchMock);
    await expect(readCore("listBuildLayers", { id: 7 })).resolves.toEqual({
      profile_id: 7,
      layers: [],
    });
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v2/plans/7/layers",
      expect.objectContaining({ credentials: "same-origin" }),
    );
  });

  it("refuses an ID that is not a positive safe integer before any request", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    await expect(readCore("readBuild", { id: 0 })).rejects.toThrow(RangeError);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it.each([
    ["a body that breaks the schema", json({ profiles: [{ id: "1" }] })],
    ["a body that is not JSON", new Response("<!doctype html>", { status: 200 })],
  ])("turns %s into a ContractError", async (_, response) => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(response));
    await expect(readCore("listBuilds", {})).rejects.toBeInstanceOf(ContractError);
  });

  it("keeps the status and problem code of a refused request", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(json({ code: "draft_changed" }, 409)));
    const error = await readCore("readBuild", { id: 3 }).catch((e: unknown) => e);
    expect(error).toBeInstanceOf(CoreError);
    expect((error as CoreError).status).toBe(409);
    expect((error as CoreError).code).toBe("draft_changed");
  });
});

describe("writeCore", () => {
  const body = {
    expected_base: { revision_id: null, plan_version: 0 },
    expected_draft: null,
    expected_header_version: null,
    header: { name: "Kit", order_number: null, customer: null, due_date: null },
  };

  it("never sends a route that declares an Idempotency-Key without one", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    await expect(writeCore("saveDraftHeader", { id: 2 }, body)).rejects.toBeInstanceOf(
      RequestContractError,
    );
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("sends the key as the Idempotency-Key header", async () => {
    const fetchMock = vi.fn().mockResolvedValue(json({}, 409));
    vi.stubGlobal("fetch", fetchMock);
    await writeCore("saveDraftHeader", { id: 2 }, body, "key-1").catch(() => undefined);
    const init = fetchMock.mock.calls[0]![1] as RequestInit;
    expect(new Headers(init.headers).get("idempotency-key")).toBe("key-1");
    expect(init.method).toBe("PUT");
  });
});
