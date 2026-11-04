import { cleanup, render, screen, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { routes } from "../routes";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("type specimen", () => {
  it("renders every figure role in the tabular mono class", () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("{}", { status: 200 })));
    const router = createMemoryRouter(routes, { initialEntries: ["/type"] });
    render(<App router={router} queryClient={createQueryClient()} />);

    expect(screen.getByRole("heading", { name: "Type" })).toBeTruthy();
    for (const role of ["Count", "ETA", "Grams", "ID", "Order number"]) {
      const row = screen.getByRole("rowheader", { name: role }).closest("tr");
      const cells = within(row as HTMLElement).getAllByRole("cell");
      expect(cells).toHaveLength(2);
      for (const cell of cells) expect(cell.className).toBe("num");
    }
    expect(
      within(screen.getByRole("main"))
        .getAllByRole("listitem")
        .map((item) => item.textContent?.split(":")[0]),
    ).toEqual([
      "Plex Sans 400",
      "Plex Sans 500",
      "Plex Sans 600",
      "Plex Mono 400",
      "Plex Mono 500",
    ]);
  });
});
