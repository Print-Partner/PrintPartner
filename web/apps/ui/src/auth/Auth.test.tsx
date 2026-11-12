import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { routes } from "../routes";
import {
  LINK_ENDED,
  MISMATCH,
  OWNER_EXISTS,
  TOO_MANY,
  UNREACHABLE,
  WRONG,
} from "./messages";
import { nextPath } from "./next";

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
const problem = (status: number, detail: string) => json({ detail }, status);
const user = {
  user_id: "user-1",
  login: "owner",
  display_name: "Owner",
  email: "owner@example.com",
  provider: "email",
  is_admin: true,
  hasPassword: true,
};

type Answer = (init?: RequestInit) => Response | Promise<Response>;

/** A fake core: `answers` by path; every other path answers 401, as when signed out. */
function core(answers: Record<string, Answer>) {
  return vi.fn((path: string, init?: RequestInit) =>
    Promise.resolve().then(() => {
      const answer = answers[path.split("?")[0]];
      return answer ? answer(init) : problem(401, "refused");
    }),
  );
}

function renderAt(entries: string[], fetchImpl: ReturnType<typeof core>) {
  vi.stubGlobal("fetch", fetchImpl);
  const router = createMemoryRouter(routes, { initialEntries: entries });
  const queryClient = createQueryClient();
  queryClient.setDefaultOptions({
    queries: { ...queryClient.getDefaultOptions().queries, retry: false },
  });
  render(<App router={router} queryClient={queryClient} />);
  return { router, path: () => router.state.location.pathname + router.state.location.search };
}

const fill = async (label: string, value: string) =>
  fireEvent.change(await screen.findByLabelText(label), { target: { value } });
const sent = (fetchMock: ReturnType<typeof core>, path: string) =>
  fetchMock.mock.calls.filter(([called]) => called === path);

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("next", () => {
  it.each([
    ["/builds/3", "/builds/3"],
    ["/builds/3/plan?draft=1#parts", "/builds/3/plan?draft=1#parts"],
    ["/states/printers", "/states/printers"],
  ])("follows %s", (raw, expected) => expect(nextPath(raw, routes)).toBe(expected));

  it.each([
    null,
    "",
    "//example.com",
    "https://example.com",
    "/\\example.com",
    "/\texample.com",
    "builds/3",
    "/login",
    "/setup",
    "/nope/nothing",
  ])("ignores %s and opens Today", (raw) => expect(nextPath(raw, routes)).toBe("/today"));
});

describe("first run", () => {
  it("goes from the sign-in address to setup, creates the owner and opens Today", async () => {
    const fetchMock = core({
      "/auth/status": () => json({ setup_required: true }),
      "/auth/register": () => json({ user }),
      "/health": () => json({}),
    });
    const { path } = renderAt(["/login?next=%2Fbuilds%2F3"], fetchMock);
    await screen.findByRole("heading", { name: "Set up Print Partner" });
    await fill("Email", "owner@example.com");
    await fill("Password", "password-1234");
    await fill("Password again", "password-1234");
    fireEvent.click(screen.getByRole("button", { name: "Create account" }));
    await waitFor(() => expect(path()).toBe("/today"));
    const [[, init]] = sent(fetchMock, "/auth/register");
    expect(JSON.parse(String(init?.body))).toEqual({
      email: "owner@example.com",
      password: "password-1234",
    });
  });

  it("refuses two different passwords without sending them", async () => {
    const fetchMock = core({ "/auth/status": () => json({ setup_required: true }) });
    renderAt(["/setup"], fetchMock);
    await fill("Email", "owner@example.com");
    await fill("Password", "password-1234");
    await fill("Password again", "password-9999");
    fireEvent.click(screen.getByRole("button", { name: "Create account" }));
    expect((await screen.findByRole("alert")).textContent).toBe(MISMATCH);
    expect(screen.getByLabelText("Password again").getAttribute("aria-invalid")).toBe("true");
    expect(sent(fetchMock, "/auth/register")).toEqual([]);
  });

  it("points to sign-in when an owner already exists", async () => {
    const fetchMock = core({
      "/auth/status": () => json({ setup_required: true }),
      "/auth/register": () => problem(403, "refused"),
    });
    renderAt(["/setup"], fetchMock);
    await fill("Email", "owner@example.com");
    await fill("Password", "password-1234");
    await fill("Password again", "password-1234");
    fireEvent.click(await screen.findByRole("button", { name: "Create account" }));
    expect((await screen.findByRole("alert")).textContent).toBe(OWNER_EXISTS);
    expect(screen.getByRole("link", { name: "Go to sign-in" }).getAttribute("href")).toBe("/login");
  });

  it("sends an owner who already set up to sign-in", async () => {
    const { path } = renderAt(
      ["/setup"],
      core({ "/auth/status": () => json({ setup_required: false }) }),
    );
    await screen.findByRole("heading", { name: "Sign in" });
    expect(path()).toBe("/login");
  });
});

describe("sign-in", () => {
  const signIn = async () => {
    await fill("Email", "owner@example.com");
    await fill("Password", "password-1234");
    fireEvent.click(await screen.findByRole("button", { name: "Sign in" }));
  };

  it("returns to the page in next", async () => {
    const fetchMock = core({
      "/auth/status": () => json({ setup_required: false }),
      "/auth/login": () => json({ user }),
      "/health": () => json({}),
      "/api/v2/plans/3": () => json({}),
    });
    const { path } = renderAt(["/login?next=%2Fbuilds%2F3"], fetchMock);
    await screen.findByRole("heading", { name: "Sign in" });
    await signIn();
    await waitFor(() => expect(path()).toBe("/builds/3"));
  });

  it.each(["//example.com", "https://example.com"])("ignores next=%s", async (next) => {
    const { path } = renderAt(
      [`/login?next=${encodeURIComponent(next)}`],
      core({
        "/auth/status": () => json({ setup_required: false }),
        "/auth/login": () => json({ user }),
        "/health": () => json({}),
      }),
    );
    await signIn();
    await waitFor(() => expect(path()).toBe("/today"));
  });

  it.each([
    [401, WRONG],
    [429, TOO_MANY],
    [503, UNREACHABLE],
  ])("says what a %i means and stays signed out", async (status, message) => {
    const { path } = renderAt(
      ["/login"],
      core({
        "/auth/status": () => json({ setup_required: false }),
        "/auth/login": () => problem(status, "refused"),
      }),
    );
    await signIn();
    expect((await screen.findByRole("alert")).textContent).toBe(message);
    expect(path()).toBe("/login");
  });

  it("offers a retry while the core cannot be reached", async () => {
    let up = false;
    const fetchMock = vi.fn(() =>
      up ? Promise.resolve(json({ setup_required: false })) : Promise.reject(new TypeError("down")),
    );
    vi.stubGlobal("fetch", fetchMock);
    const router = createMemoryRouter(routes, { initialEntries: ["/login"] });
    render(<App router={router} queryClient={createQueryClient()} />);
    expect((await screen.findByRole("alert")).textContent).toBe(UNREACHABLE);
    up = true;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByRole("button", { name: "Sign in" })).toBeTruthy();
  });

  it("asks for a new link when the browser is no longer admitted", async () => {
    renderAt(["/login"], core({}));
    expect((await screen.findByRole("alert")).textContent).toBe(LINK_ENDED);
  });
});

describe("signed out", () => {
  it("opens sign-in with next when a read answers 401", async () => {
    const { path } = renderAt(["/builds/3"], core({ "/health": () => json({}) }));
    await waitFor(() => expect(path()).toBe("/login?next=%2Fbuilds%2F3"));
  });

  it("signs out in place of the page, and Back shows no signed-in content", async () => {
    let signedIn = true;
    const answers: Record<string, Answer> = {
      "/health": () => json({}),
      "/auth/status": () => json({ setup_required: false }),
      "/auth/logout": () => {
        signedIn = false;
        return json({ ok: true });
      },
    };
    // Every other read answers while signed in and refuses afterwards.
    const fetchMock = vi.fn((path: string, init?: RequestInit) =>
      Promise.resolve().then(() => {
        const answer = answers[path.split("?")[0]];
        if (answer) return answer(init);
        return signedIn ? json({}) : problem(401, "refused");
      }),
    );
    const { router, path } = renderAt(["/today", "/builds"], fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Sign out" }));
    await screen.findByText("You signed out.");
    expect(path()).toBe("/login");
    await act(() => router.navigate(-1));
    await waitFor(() => expect(path()).toBe("/login?next=%2Ftoday"));
    expect(screen.queryByRole("navigation", { name: "Main" })).toBeNull();
  });
});
