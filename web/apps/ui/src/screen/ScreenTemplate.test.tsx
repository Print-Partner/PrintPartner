import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ConnectionOverride, OFFLINE_REASON, type ConnectionStatus } from "../frame/connection";
import {
  MutationButton,
  SKELETON_DELAY_MS,
  ScreenTemplate,
  type ScreenState,
} from "./ScreenTemplate";

type Rows = { name: string; count: number }[];
const ROWS: Rows = [{ name: "Left arm", count: 0 }];
const AT = Date.UTC(2026, 10, 4, 18, 30);

function renderState(state: ScreenState<Rows>, connection: ConnectionStatus = "online") {
  const { container } = render(
    <MemoryRouter>
      <ConnectionOverride status={connection}>
        <ScreenTemplate
          title="Builds"
          state={state}
          renderData={(rows) => (
            <ul aria-label="rows">
              {rows.map((r) => (
                <li key={r.name}>{`${r.name}: ${r.count}`}</li>
              ))}
            </ul>
          )}
          empty={{ message: "No Builds yet.", action: <button type="button">New Build</button> }}
        />
      </ConnectionOverride>
    </MemoryRouter>,
  );
  return container.querySelector("section")!;
}

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("FRM-03 screen template", () => {
  it("loading reserves the layout, shows skeletons after 150 ms, and shows no numbers", () => {
    vi.useFakeTimers();
    const section = renderState({ kind: "loading" });
    const loading = section.querySelector<HTMLElement>(".pp-loading")!;
    expect(loading.getAttribute("aria-busy")).toBe("true");
    expect(section.querySelectorAll(".pp-skeleton-row")).toHaveLength(3);
    act(() => vi.advanceTimersByTime(SKELETON_DELAY_MS - 1));
    expect(loading.dataset.skeleton).toBe("reserved");
    act(() => vi.advanceTimersByTime(1));
    expect(loading.dataset.skeleton).toBe("shown");
    expect(screen.getByRole("status").textContent).toBe("Loading Builds");
    expect(screen.queryByRole("list", { name: "rows" })).toBeNull();
    expect(section.textContent).not.toMatch(/\d/);
  });

  it("empty shows the message and the next useful action", () => {
    expect(renderState({ kind: "empty" }).dataset.state).toBe("empty");
    expect(screen.getByText("No Builds yet.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "New Build" })).toBeTruthy();
  });

  it("error keeps last-known data with its timestamp and a Retry", () => {
    const retry = vi.fn();
    renderState({
      kind: "error",
      message: "The core answered 503.",
      lastData: ROWS,
      updatedAt: AT,
      retry,
    });
    const alert = screen.getByRole("alert");
    expect(alert.textContent).toContain("Could not refresh Builds. Showing data from");
    expect(alert.textContent).toContain("The core answered 503.");
    expect(alert.querySelector("time")?.getAttribute("datetime")).toBe("2026-11-04T18:30:00.000Z");
    expect(screen.getByRole("list", { name: "rows" }).textContent).toBe("Left arm: 0");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(retry).toHaveBeenCalledOnce();
  });

  it("error with nothing loaded yet is never a blank page", () => {
    renderState({ kind: "error", message: "The core answered 500.", retry: () => {} });
    expect(screen.getByRole("alert").textContent).toContain("Could not load Builds.");
    expect(screen.getByRole("button", { name: "Retry" })).toBeTruthy();
  });

  it("module off names the module and what would appear, and links to Settings", () => {
    renderState({ kind: "module-off", module: "Farm", wouldShow: "printers and the queue" });
    expect(screen.getByText("The Farm module is off.")).toBeTruthy();
    expect(
      screen.getByText("When it is on, this screen shows printers and the queue."),
    ).toBeTruthy();
    expect(screen.getByRole("link", { name: "See Modules in Settings" }).getAttribute("href")).toBe(
      "/settings#modules",
    );
  });

  it("offline keeps loaded records readable and says why changes are off", () => {
    renderState({ kind: "offline", lastData: ROWS, updatedAt: AT }, "offline");
    const status = screen.getByRole("status");
    expect(status.textContent).toContain(`${OFFLINE_REASON} Showing what was loaded at`);
    expect(status.querySelector("time")).toBeTruthy();
    expect(screen.getByRole("list", { name: "rows" })).toBeTruthy();
  });
});

describe("FRM-04 offline disables mutations with the reason", () => {
  const renderButton = (status: ConnectionStatus, onClick = vi.fn()) => {
    render(
      <ConnectionOverride status={status}>
        <MutationButton onClick={onClick}>Record a print</MutationButton>
      </ConnectionOverride>,
    );
    return { onClick, button: screen.getByRole("button", { name: "Record a print" }) };
  };

  it("online, the button runs and has no reason attached", () => {
    const { button, onClick } = renderButton("online");
    expect(button.getAttribute("aria-disabled")).toBeNull();
    expect(button.getAttribute("aria-describedby")).toBeNull();
    fireEvent.click(button);
    expect(onClick).toHaveBeenCalledOnce();
  });

  it("offline, the button does nothing and gives the reason in text and description", () => {
    const { button, onClick } = renderButton("offline");
    expect(button.getAttribute("aria-disabled")).toBe("true");
    button.focus(); // still focusable, so keyboard and screen-reader users reach the reason
    expect(document.activeElement).toBe(button);
    const reason = document.getElementById(button.getAttribute("aria-describedby")!);
    expect(reason?.textContent).toBe(OFFLINE_REASON);
    expect(screen.getByText(OFFLINE_REASON)).toBe(reason);
    fireEvent.click(button);
    expect(onClick).not.toHaveBeenCalled();
  });
});
