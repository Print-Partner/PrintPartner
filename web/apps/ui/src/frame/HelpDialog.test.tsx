import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { PLAN_HELP } from "../builds/planCopy";
import { routes } from "../routes";
import { DESTINATIONS, shortcutLabel } from "./destinations";

function openHelp(path = "/today") {
  const requests = vi
    .fn()
    .mockImplementation(() =>
      Promise.resolve(new Response("{}", { headers: { "content-type": "application/json" } })),
    );
  vi.stubGlobal("fetch", requests);
  const router = createMemoryRouter(routes, { initialEntries: [path] });
  render(<App router={router} queryClient={createQueryClient()} />);
  const opener = screen.getByRole("button", { name: "Help", exact: true });
  opener.focus();
  fireEvent.click(opener);
  return { router, requests, opener, dialog: screen.getByRole("dialog", { name: "Help" }) };
}

afterEach(() => {
  cleanup();
  localStorage.clear();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("Help explains destinations and draft acceptance", () => {
  it("opens from a visible button without needing a keyboard shortcut", () => {
    const { dialog, opener } = openHelp();
    expect(opener.getAttribute("aria-haspopup")).toBe("dialog");
    expect(within(dialog).getByRole("heading", { name: "Where to go" })).toBeTruthy();
    expect(within(dialog).getByRole("heading", { name: "Plan changes" })).toBeTruthy();
    expect(within(dialog).getByRole("heading", { name: "Keyboard shortcuts" })).toBeTruthy();
    expect(dialog.contains(document.activeElement)).toBe(true);
  });

  it("describes the five destinations in rail order", () => {
    const { dialog } = openHelp();
    const links = within(dialog).getAllByRole("link");
    expect(links.map((link) => link.textContent)).toEqual([
      "Today",
      "Builds",
      "Farm",
      "Library",
      "Settings",
    ]);
    for (const destination of DESTINATIONS) {
      expect(within(dialog).getByText(destination.help + ".")).toBeTruthy();
      expect(within(dialog).getByText(shortcutLabel(destination))).toBeTruthy();
    }
  });

  it.each(DESTINATIONS)("opens $label and closes Help", (destination) => {
    const { dialog, router } = openHelp();
    fireEvent.click(within(dialog).getByRole("link", { name: destination.label }));
    expect(router.state.location.pathname).toBe(destination.path);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("uses the shared draft and Accept copy without changing it", () => {
    const { dialog } = openHelp();
    for (const line of PLAN_HELP) expect(within(dialog).getByText(line)).toBeTruthy();
    expect(dialog.textContent).toContain("A saved draft is kept if the app closes.");
    expect(dialog.textContent).toContain("The Build does not change until you accept the draft.");
  });

  it("explains shortcut scope and the command palette", () => {
    const { dialog } = openHelp();
    expect(within(dialog).getByText(/G sequences and \? work outside text fields/)).toBeTruthy();
    const table = within(dialog).getByRole("table");
    expect(within(table).getByText("Cmd/Ctrl K")).toBeTruthy();
    expect(within(table).getByText("Esc")).toBeTruthy();
    expect(within(table).getByText("?")).toBeTruthy();
  });

  it("keeps Help reachable after single-key shortcuts are turned off and after reload", () => {
    const { dialog, opener } = openHelp();
    fireEvent.click(within(dialog).getByRole("checkbox"));
    fireEvent.click(within(dialog).getByRole("button", { name: "Close" }));
    fireEvent.keyDown(document.body, { key: "?" });
    expect(screen.queryByRole("dialog")).toBeNull();
    fireEvent.click(opener);
    expect((screen.getByRole("checkbox") as HTMLInputElement).checked).toBe(false);
    cleanup();
    const reopened = openHelp("/library");
    expect((within(reopened.dialog).getByRole("checkbox") as HTMLInputElement).checked).toBe(false);
  });

  it("returns focus to the Help button on close", async () => {
    const { dialog, opener } = openHelp();
    fireEvent.click(within(dialog).getByRole("button", { name: "Close" }));
    await waitFor(() => expect(document.activeElement).toBe(opener));
  });

  it("opening and closing Help sends no mutation request", () => {
    const { dialog, requests } = openHelp("/library");
    fireEvent.click(within(dialog).getByRole("button", { name: "Close" }));
    for (const [, init] of requests.mock.calls) {
      expect(init?.method ?? "GET").toBe("GET");
    }
  });
});
