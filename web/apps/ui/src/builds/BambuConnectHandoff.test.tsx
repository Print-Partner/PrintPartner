import { cleanup, fireEvent, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fakeCore, json, problem, renderAt, writes } from "./sendQueueFake.test-utils";

const STAGE = "/api/v2/plans/2/bambu-connect-handoff";
const URL = "bambu-connect://import-file?path=%2Fplate.gcode&name=plate&version=1.0.0";
const OPEN = "Open in Bambu Connect";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function setup({ desktop = true, enabled = true, refused = false } = {}) {
  const opener = vi.fn(async (_url: string) => {});
  vi.stubGlobal("__PRINT_PARTNER_NATIVE_BAMBU_CONNECT__", desktop ? opener : undefined);
  const { fetchMock: rest } = fakeCore({ farm: "off" });
  const fetchMock = vi.fn(async (path: string, init?: RequestInit) => {
    if (path === "/api/v2/plans/2/features") return json({ effective: { bambu: { enabled } } });
    if (path === STAGE && init?.method === "POST") {
      return refused
        ? problem(403, "bambu_refused", "Bambu Connect is disabled for this Build")
        : json({ filename: "plate.gcode", connect_url: URL });
    }
    return rest(path, init);
  });
  renderAt(fetchMock);
  return { opener, fetchMock };
}

const select = (input: HTMLElement, filename: string) =>
  fireEvent.change(input, { target: { files: [new File(["G28"], filename)] } });

describe("Bambu Connect in Production", () => {
  it("only stages on submit, opens only on a separate click, and clears the link on a new file", async () => {
    const { opener, fetchMock } = setup();
    const input = await screen.findByLabelText("Sliced file for Bambu Connect");
    const stage = screen.getByRole("button", { name: "Stage file" });
    expect(stage).toHaveProperty("disabled", true);
    select(input, "plate.gcode");
    expect(writes(fetchMock)).toEqual([]);
    expect(screen.queryByRole("button", { name: OPEN })).toBeNull();
    expect(opener).not.toHaveBeenCalled();

    fireEvent.click(stage);
    const open = await screen.findByRole("button", { name: OPEN });
    expect(screen.getByText("plate.gcode is staged. Nothing has been sent.")).toBeTruthy();
    const [request] = writes(fetchMock);
    expect(request?.[0]).toBe(STAGE);
    expect((request?.[1]?.body as FormData).get("file")).toHaveProperty("name", "plate.gcode");
    expect(opener).not.toHaveBeenCalled();

    fireEvent.click(open);
    await waitFor(() => expect(opener).toHaveBeenCalledExactlyOnceWith(URL));
    await waitFor(() => expect(input).toHaveProperty("disabled", false));
    expect(writes(fetchMock)).toHaveLength(1);
    select(input, "next.3mf");
    expect(screen.queryByRole("button", { name: OPEN })).toBeNull();
  });

  it("does not stage an invalid file or offer Open after a refusal", async () => {
    const { opener, fetchMock } = setup({ refused: true });
    const input = await screen.findByLabelText("Sliced file for Bambu Connect");
    const stage = screen.getByRole("button", { name: "Stage file" });
    select(input, "wrong.stl");
    expect(screen.getByText(/Choose a nonempty/)).toBeTruthy();
    expect(stage).toHaveProperty("disabled", true);
    expect(writes(fetchMock)).toEqual([]);
    select(input, "plate.gcode");
    fireEvent.click(stage);
    expect(await screen.findByText("Bambu Connect is disabled for this Build")).toBeTruthy();
    expect(screen.queryByRole("button", { name: OPEN })).toBeNull();
    expect(opener).not.toHaveBeenCalled();
    expect(writes(fetchMock)).toHaveLength(1);
  });

  it.each([{ desktop: false }, { enabled: false }])("hides the handoff for %o", async (options) => {
    const { fetchMock } = setup(options);
    await screen.findByRole("heading", { name: "Production" });
    if (options.enabled === false) {
      await waitFor(() =>
        expect(fetchMock).toHaveBeenCalledWith("/api/v2/plans/2/features", expect.anything()),
      );
    }
    expect(screen.queryByRole("heading", { name: "Bambu Connect" })).toBeNull();
    expect(writes(fetchMock)).toEqual([]);
  });
});
