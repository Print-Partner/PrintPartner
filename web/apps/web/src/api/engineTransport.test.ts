import { afterEach, describe, expect, it, vi } from "vitest";
import { setEngineUnauthorizedHandler } from "./contractRequest";
import {
  EngineHttpError,
  engineFetch,
  engineFetchMultipart,
  engineFetchStream,
  engineSendMultipart,
} from "./engineTransport";

const fetchMock = vi.fn<typeof fetch>();
vi.stubGlobal("fetch", fetchMock);

afterEach(() => {
  fetchMock.mockReset();
  setEngineUnauthorizedHandler(null);
});

describe("engine transport", () => {
  it("sends multipart requests with credentials and preserves caller headers", async () => {
    fetchMock.mockResolvedValueOnce(Response.json({ ok: true }));
    const form = new FormData();
    form.append("name", "plate");

    await expect(
      engineFetchMultipart<{ ok: boolean }>({
        path: "/uploads",
        form,
        headers: { "If-Match": '"basis"' },
      }),
    ).resolves.toEqual({ ok: true });

    const init = fetchMock.mock.calls[0]?.[1];
    expect(init?.credentials).toBe("include");
    expect(init?.body).toBe(form);
    expect(new Headers(init?.headers).get("If-Match")).toBe('"basis"');
    expect(new Headers(init?.headers).has("Content-Type")).toBe(false);
  });

  it("names the taken Build when a rename answers duplicate_build_name", async () => {
    fetchMock.mockResolvedValueOnce(
      Response.json({ code: "duplicate_build_name", name: "Taken" }, { status: 409 }),
    );

    await expect(
      engineFetchStream({ path: "/plans/1", method: "PATCH" }),
    ).rejects.toEqual(expect.objectContaining<Partial<EngineHttpError>>({
      status: 409,
      message: "Profile already exists: Taken",
    }));
  });

  it("centralizes unauthorized handling for streamed responses", async () => {
    const unauthorized = vi.fn();
    setEngineUnauthorizedHandler(unauthorized);
    fetchMock.mockResolvedValueOnce(Response.json({ detail: "Sign in" }, { status: 401 }));

    await expect(
      engineFetchStream({ path: "/assistant/chat", method: "POST" }),
    ).rejects.toEqual(expect.objectContaining<Partial<EngineHttpError>>({ status: 401 }));
    expect(unauthorized).toHaveBeenCalledOnce();
  });

  it("keeps the session for a rejected current password and still signs out for other 401s", async () => {
    const unauthorized = vi.fn();
    setEngineUnauthorizedHandler(unauthorized);
    fetchMock.mockResolvedValueOnce(
      Response.json(
        { code: "current_password_rejected", detail: "Current password is incorrect" },
        { status: 401 },
      ),
    );

    await expect(
      engineFetch("/auth/change-password", {
        method: "POST",
        body: JSON.stringify({
          current_password: "wrong-current-password",
          new_password: "correct-horse-battery-2",
        }),
      }),
    ).rejects.toMatchObject({
      name: "EngineHttpError",
      message: "Current password is incorrect",
      status: 401,
    });
    expect(unauthorized).not.toHaveBeenCalled();

    fetchMock.mockResolvedValueOnce(
      Response.json({ detail: "Current password is incorrect" }, { status: 401 }),
    );
    await expect(engineFetch("/auth/change-password", { method: "POST", body: "{}" })).rejects.toMatchObject({
      message: "Current password is incorrect",
      status: 401,
    });
    expect(unauthorized).toHaveBeenCalledOnce();

    fetchMock.mockResolvedValueOnce(
      Response.json({ detail: "Authentication required" }, { status: 401 }),
    );
    await expect(engineFetch("/auth/me")).rejects.toMatchObject({
      message: "Authentication required",
      status: 401,
    });
    expect(unauthorized).toHaveBeenCalledTimes(2);

    fetchMock.mockResolvedValueOnce(
      Response.json({ detail: "Invalid email or password" }, { status: 401 }),
    );
    await expect(
      engineFetch("/auth/login", {
        method: "POST",
        body: JSON.stringify({ email: "a@example.test", password: "wrong" }),
      }),
    ).rejects.toMatchObject({
      message: "Invalid email or password",
      status: 401,
    });
    expect(unauthorized).toHaveBeenCalledTimes(3);
  });

  it("accepts an empty successful response for multipart commands", async () => {
    fetchMock.mockResolvedValueOnce(new Response(null, { status: 204 }));

    await expect(
      engineSendMultipart({ path: "/parts/7/thumbnail", form: new FormData() }),
    ).resolves.toBeUndefined();
  });
});
