import { cleanup, render, screen } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "./App";
import { routes } from "./routes";

const coreJson = () =>
  new Response("{}", { status: 200, headers: { "content-type": "application/json" } });

function renderAt(path: string) {
  const router = createMemoryRouter(routes, { initialEntries: [path] });
  const queryClient = createQueryClient();
  render(<App router={router} queryClient={queryClient} />);
  return { fetchMock: globalThis.fetch as ReturnType<typeof vi.fn> };
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("App shell", () => {
  it("renders the placeholder and reports a reachable core", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(coreJson()));
    const { fetchMock } = renderAt("/");

    expect(screen.getByRole("heading", { name: "Replacement UI" })).toBeTruthy();
    expect(await screen.findByText("Core reachable")).toBeTruthy();
    expect(fetchMock).toHaveBeenCalledWith(
      "/health",
      expect.objectContaining({ credentials: "same-origin" }),
    );
  });

  it("reports an unreachable core instead of a blank frame", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("Failed to fetch")));
    renderAt("/");

    expect(await screen.findByText("Core unreachable")).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Replacement UI" })).toBeTruthy();
  });

  it("shows Not found for unknown addresses", () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(coreJson()));
    renderAt("/builds/nope");

    expect(screen.getByRole("heading", { name: "Not found" })).toBeTruthy();
  });
});
