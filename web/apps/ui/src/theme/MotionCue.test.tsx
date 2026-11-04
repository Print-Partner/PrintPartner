import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MotionCue } from "./MotionCue";

function stubReducedMotion(reduce: boolean) {
  vi.stubGlobal(
    "matchMedia",
    vi.fn((query: string) => ({
      matches: reduce && query === "(prefers-reduced-motion: reduce)",
      media: query,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    })),
  );
}

function renderCue() {
  render(
    <MotionCue message="Moved Left arm to Plate 3">
      <span>Left arm</span>
    </MotionCue>,
  );
  const status = screen.getByRole("status");
  return { status, cue: status.closest(".pp-motion-cue") as HTMLElement };
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("TOK-04 reduced motion keeps the information as text", () => {
  it("shows where the thing went as visible text under reduced motion", () => {
    stubReducedMotion(true);
    const { status, cue } = renderCue();
    expect(cue.dataset.motion).toBe("reduced");
    expect(status.textContent).toBe("Moved Left arm to Plate 3");
    expect(status.className).toBe("pp-motion-text");
    expect(screen.getByText("Left arm")).toBeTruthy();
  });

  it("gives the same text to assistive technology when motion is on", () => {
    stubReducedMotion(false);
    const { status, cue } = renderCue();
    expect(cue.dataset.motion).toBe("full");
    expect(status.textContent).toBe("Moved Left arm to Plate 3");
    expect(status.className).toBe("pp-visually-hidden");
  });
});
