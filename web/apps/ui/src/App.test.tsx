import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { onlineManager } from "@tanstack/react-query";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "./App";
import { HEALTH_TIMEOUT_MS } from "./frame/connection";
import { SEQUENCE_MS } from "./frame/useShortcuts";
import { routes } from "./routes";

const coreJson = () => new Response("{}", { headers: { "content-type": "application/json" } });

function renderAt(path: string, fetchImpl = vi.fn().mockResolvedValue(coreJson())) {
  vi.stubGlobal("fetch", fetchImpl);
  const router = createMemoryRouter(routes, { initialEntries: [path] });
  render(<App router={router} queryClient={createQueryClient()} />);
  return { router, fetchMock: fetchImpl, path: () => router.state.location.pathname };
}

const press = (key: string, init: KeyboardEventInit = {}, target: Element = document.body) =>
  act(() => void fireEvent.keyDown(target, { key, ...init }));
const heading = () => screen.getByRole("heading", { level: 1 }).textContent;

afterEach(() => {
  cleanup();
  onlineManager.setOnline(true);
  vi.restoreAllMocks();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("App frame", () => {
  it("opens on Today and reports a reachable core", async () => {
    const { fetchMock, path } = renderAt("/");
    expect(path()).toBe("/today");
    expect(heading()).toBe("Today");
    expect(await screen.findByText("Core reachable")).toBeTruthy();
    expect(fetchMock).toHaveBeenCalledWith(
      "/health",
      expect.objectContaining({ credentials: "same-origin" }),
    );
  });

  it.each(["/nope/nothing", "/states/__proto__", "/states/constructor", "/states/toString"])(
    "shows Not found inside the frame for %s",
    (address) => {
      renderAt(address);
      expect(heading()).toBe("Not found");
      expect(screen.getByRole("navigation", { name: "Main" })).toBeTruthy();
    },
  );

  it("keeps a pulse line slot that shows no false zeros", () => {
    renderAt("/today");
    expect(screen.getByRole("region", { name: "Printer pulse" }).textContent).toBe(
      "Printer counts not loaded",
    );
  });
});

describe("FRM-01 rail and G shortcuts", () => {
  it("lists the five destinations in order with their shortcuts", () => {
    renderAt("/today");
    const links = within(screen.getByRole("navigation", { name: "Main" })).getAllByRole("link");
    expect(links.map((l) => l.textContent)).toEqual([
      "TodayG T",
      "BuildsG B",
      "FarmG F",
      "LibraryG L",
      "SettingsG ,",
    ]);
    expect(links[0].getAttribute("aria-current")).toBe("page");
  });

  it.each([
    ["t", "/today", "Today"],
    ["b", "/builds", "Builds"],
    ["f", "/farm", "Farm"],
    ["l", "/library", "Library"],
    [",", "/settings", "Settings"],
  ])("G then %s goes to %s", (key, to, title) => {
    const { path } = renderAt(key === "t" ? "/settings" : "/today");
    press("g");
    press(key);
    expect(path()).toBe(to);
    expect(heading()).toBe(title);
  });

  it("ignores the second key after the sequence window", () => {
    vi.useFakeTimers({ toFake: ["performance", "setTimeout", "setInterval", "Date"] });
    const { path } = renderAt("/today");
    press("g");
    act(() => vi.advanceTimersByTime(SEQUENCE_MS + 1));
    press("b");
    expect(path()).toBe("/today");
  });

  it("ignores shortcuts typed into a text field", () => {
    const { path } = renderAt("/today");
    const field = document.body.appendChild(document.createElement("input"));
    press("g", {}, field);
    press("b", {}, field);
    press("?", {}, field);
    expect(path()).toBe("/today");
    expect(screen.queryByRole("dialog")).toBeNull();
    field.remove();
  });
});

describe("FRM-02 palette and shortcut list by keyboard", () => {
  it.each([{ ctrlKey: true }, { metaKey: true }])(
    "%o K toggles the palette with focus in it",
    (mod) => {
      renderAt("/today");
      press("k", mod);
      const dialog = screen.getByRole("dialog", { name: "Command palette" });
      expect(document.activeElement).toBe(within(dialog).getByRole("combobox"));
      press("k", mod, document.activeElement!);
      expect(screen.queryByRole("dialog")).toBeNull();
    },
  );

  it("filters, moves with arrows, scrolls the active option into view and runs with Enter", () => {
    const scroll = vi.fn();
    Element.prototype.scrollIntoView = scroll;
    const { path } = renderAt("/today");
    press("k", { ctrlKey: true });
    const input = screen.getByRole("combobox");
    fireEvent.change(input, { target: { value: "go to" } });
    expect(screen.getAllByRole("option")).toHaveLength(5);
    press("ArrowDown", {}, input);
    press("ArrowDown", {}, input);
    const active = document.getElementById(input.getAttribute("aria-activedescendant")!);
    expect(active?.textContent).toBe("Go to FarmG F");
    expect(active?.getAttribute("aria-selected")).toBe("true");
    expect(scroll.mock.contexts.at(-1)).toBe(active);
    press("Enter", {}, input);
    expect(path()).toBe("/farm");
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("closes on Escape and returns focus to where it was", () => {
    renderAt("/today");
    const opener = screen.getByRole("button", { name: /Commands/ });
    opener.focus();
    press("k", { ctrlKey: true }, opener);
    // Escape during IME composition belongs to the composition.
    press("Escape", { isComposing: true }, screen.getByRole("combobox"));
    expect(screen.getByRole("dialog")).toBeTruthy();
    press("Escape", {}, screen.getByRole("combobox"));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.activeElement).toBe(opener);
  });

  it("? lists every shortcut, keeps Tab inside, and blocks shortcuts behind it", () => {
    renderAt("/today");
    press("?");
    const dialog = screen.getByRole("dialog", { name: "Keyboard shortcuts" });
    const keys = within(dialog)
      .getAllByRole("row")
      .slice(1)
      .map((r) => r.querySelector("kbd")?.textContent);
    expect(keys).toEqual(["G T", "G B", "G F", "G L", "G ,", "Cmd/Ctrl K", "?", "Esc"]);
    const close = within(dialog).getByRole("button", { name: "Close" });
    expect(document.activeElement).toBe(close);
    press("Tab", {}, close);
    expect(document.activeElement).toBe(close);
    press("g", {}, close);
    press("b", {}, close);
    expect(heading()).toBe("Today");
    fireEvent.click(close);
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});

describe("FRM-04 offline frame", () => {
  it("says Offline and shows the offline state when the core does not answer", async () => {
    renderAt("/builds", vi.fn().mockRejectedValue(new TypeError("Failed to fetch")));
    expect(await screen.findByText("Offline")).toBeTruthy();
    const section = screen.getByRole("heading", { name: "Builds" }).closest("section")!;
    expect(section.dataset.state).toBe("offline");
    expect(within(section).getByRole("status").textContent).toContain("cannot reach its core");
    press("g"); // navigation keeps working offline
    press("l");
    expect(heading()).toBe("Library");
  });

  it("keeps probing while the browser reports no network", async () => {
    onlineManager.setOnline(false);
    renderAt("/today", vi.fn().mockRejectedValue(new TypeError("Failed to fetch")));
    expect(await screen.findByText("Offline")).toBeTruthy();
  });

  it("counts a core that never answers as offline after the timeout", async () => {
    const timeout = vi.spyOn(AbortSignal, "timeout").mockImplementation(() => AbortSignal.abort());
    const hang = vi.fn((_: string, init: RequestInit) => {
      return new Promise<Response>((_, reject) => {
        if (init.signal?.aborted) reject(init.signal.reason);
        init.signal?.addEventListener("abort", () => reject(init.signal?.reason));
      });
    });
    renderAt("/today", hang);
    expect(await screen.findByText("Offline")).toBeTruthy();
    expect(timeout).toHaveBeenCalledWith(HEALTH_TIMEOUT_MS);
  });
});
