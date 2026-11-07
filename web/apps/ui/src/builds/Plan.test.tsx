import type {
  BuildHeaderView,
  DraftAcceptPreview,
  DraftHeaderView,
  DraftWorkspace,
  Health,
} from "@print-partner/rust-client";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { OFFLINE_REASON } from "../frame/connection";
import { routes } from "../routes";

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
const problem = (status: number, code: string) =>
  json({ type: "urn:print-partner:problem:conflict", title: "Conflict", status, code }, status);

const health = (orders: "on" | "off"): Health => ({
  ok: true,
  authenticated: true,
  authentication_required: true,
  capabilities: [],
  degraded_detail: null,
  discord_oauth_configured: false,
  github_oauth_configured: false,
  multi_user: false,
  owner_mapping_required: false,
  registration_open: false,
  single_user_auth: true,
  single_user_setup_required: false,
  modules: [
    orders === "on"
      ? { module: "orders", state: "on" }
      : { module: "orders", state: "off", reason: "default_off" },
    { module: "farm", state: "off", reason: "default_off" },
    { module: "spoolman", state: "off", reason: "default_off" },
  ],
});

const DIGEST = "a".repeat(64);
const BASE = { revision_id: 4, plan_version: 3 };
const BUILD = { name: "Bracket kit", order_number: "PO-7", customer: null, due_date: null };
const DRAFT = {
  draft_id: 9,
  state: "open",
  lifecycle_version: 2,
  snapshot_digest: DIGEST,
  base: BASE,
};

const part = (id: number, filename: string, included: boolean, override: number | null) => ({
  draft_part_id: id,
  base_revision_part_id: id,
  part_key: filename,
  filename,
  relative_path: `stl/${filename}`,
  source_layer: "base:Kit",
  role: "primary",
  quantity_inferred: 2,
  quantity_override: override,
  quantity_effective: override ?? 2,
  included,
});

const workspace = (draft = DRAFT): DraftWorkspace => ({
  profile_id: 2,
  draft,
  parts: [part(11, "bracket.stl", true, null), part(12, "gear.stl", false, 4)],
  diff: { base_is_current: true, added: [], removed: [], changed: [] },
  reconciliation: { kind: "ready", reused_units: 6, new_units: 0, surplus_units: 0 },
});

const draftHeader = (version: number | null): DraftHeaderView => ({
  draft_id: 9,
  build: BUILD,
  draft:
    version === null
      ? null
      : {
          header: { ...BUILD, name: "Bracket kit v2", customer: "Acme" },
          header_version: version,
          updated_at: "2026-11-07T00:00:00.000Z",
        },
});

const buildHeader: BuildHeaderView = { build: BUILD, base: BASE };

const unitRef = (filename: string, index: number) => ({
  token: `ppu_${String(index).padStart(32, "0")}`,
  filename,
  unit_index: index,
  printed: true,
  assembled: false,
});

const preview = (version: number, blocked = false): DraftAcceptPreview => ({
  kind: "changed",
  revision: {
    draft_id: 9,
    expected_snapshot_digest: DIGEST,
    expected_lifecycle_version: 2,
    expected_base: BASE,
    expected_header_version: version,
  },
  snapshot: {
    saved_before_accept: true,
    detail: "Print Partner saves a snapshot of the accepted Plan before it accepts this draft.",
  },
  changes: {
    fields: [
      {
        draft_part_id: 12,
        filename: "gear.stl",
        field: "quantity_override",
        old: null,
        new: { kind: "integer", value: 4 },
      },
    ],
    header: [{ field: "name", old: "Bracket kit", new: "Bracket kit v2" }],
    inputs: [],
    parts_added: [],
    parts_removed: [],
    required_units: { added: [], removed: [], printed_affected: [unitRef("gear.stl", 0)] },
    plates: {
      cleared: [{ plate_id: "plate_1", ordinal: 1, printer_name: "Voron 2.4", unit_count: 3 }],
    },
    stock: { kind: "off" },
    duplicate_orders: [{ profile_id: 5, name: "Other kit", order_number: "PO-7" }],
    production: null,
    blocked_by: blocked ? [{ kind: "print_job", operation_id: "op-1", state: "running" }] : [],
  },
});

const RECEIPT = {
  profile_id: 2,
  draft_id: 9,
  revision_id: 5,
  plan_version: 4,
  draft_lifecycle_version: 3,
  revision_digest: "b".repeat(64),
  required_unit_mapping_digest: "c".repeat(64),
  applied_at: "2026-11-07T00:00:00.000Z",
};

type Init = RequestInit | undefined;
type Handler = (path: string, init: Init) => Response | Promise<Response> | undefined;

interface Core {
  open?: boolean;
  version?: number | null;
  orders?: "on" | "off";
  write?: Handler;
  preview?: () => Response | Promise<Response>;
  draft?: () => DraftWorkspace;
}

/** A fake core: the Plan reads for Build 2, and `write` for anything else. */
function core({
  open = true,
  version = 1,
  orders = "on",
  write,
  preview: read,
  draft = workspace,
}: Core = {}) {
  return vi.fn((path: string, init?: RequestInit) => {
    const answer = (): Response | Promise<Response> => {
      if (path === "/health") return json(health(orders));
      const written = init?.method && init.method !== "GET" ? write?.(path, init) : undefined;
      if (written) return written;
      if (path === "/api/v2/plans/2/drafts") {
        return json({ profile_id: 2, drafts: open ? [DRAFT] : [] });
      }
      if (path === "/api/v2/plans/2/drafts/header" && !init?.method) return json(buildHeader);
      if (path === "/api/v2/plans/2/drafts/9") return json(draft());
      if (path === "/api/v2/plans/2/drafts/9/header") return json(draftHeader(version));
      if (path === "/api/v2/plans/2/drafts/9/accept-preview") {
        return read ? read() : json(preview(version ?? 0));
      }
      return json({ type: "about:blank", title: "Not Found", status: 404 }, 404);
    };
    return Promise.resolve(answer());
  });
}

function renderAt(fetchImpl: ReturnType<typeof core>) {
  vi.stubGlobal("fetch", fetchImpl);
  const router = createMemoryRouter(routes, { initialEntries: ["/builds/2/plan"] });
  const queryClient = createQueryClient();
  queryClient.setDefaultOptions({
    queries: { ...queryClient.getDefaultOptions().queries, retryDelay: 0 },
    mutations: { ...queryClient.getDefaultOptions().mutations, retryDelay: 0 },
  });
  render(<App router={router} queryClient={queryClient} />);
}

const writes = (fetchMock: ReturnType<typeof core>) =>
  fetchMock.mock.calls
    .filter(([, init]) => init?.method && init.method !== "GET")
    .map(([path, init]) => ({
      path,
      method: init!.method,
      key: new Headers(init!.headers).get("idempotency-key"),
      body: JSON.parse(String(init!.body)),
    }));

const field = (name: string) => screen.getByRole<HTMLInputElement>("textbox", { name });

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("UI6-01 Plan edits autosave to the draft", () => {
  it("saves a header edit to the open draft, naming the draft and header it read", async () => {
    const fetchMock = core({
      write: (path) =>
        path === "/api/v2/plans/2/drafts/header"
          ? json({
              receipt: { profile_id: 2, draft_id: 9, header: draftHeader(2).draft },
              header: draftHeader(2),
            })
          : undefined,
    });
    renderAt(fetchMock);
    const name = await screen.findByRole<HTMLInputElement>("textbox", { name: "Name" });
    expect(name.value).toBe("Bracket kit v2");
    expect(field("Customer").value).toBe("Acme");
    fireEvent.change(name, { target: { value: "  Bracket kit v3 " } });
    fireEvent.keyDown(name, { key: "Enter" });
    await waitFor(() => expect(writes(fetchMock)).toHaveLength(1));
    const [sent] = writes(fetchMock);
    expect(sent).toMatchObject({ path: "/api/v2/plans/2/drafts/header", method: "PUT" });
    expect(sent!.key).toMatch(/\S/);
    expect(sent!.body).toEqual({
      expected_base: BASE,
      expected_draft: DRAFT,
      expected_header_version: 1,
      header: { name: "Bracket kit v3", order_number: "PO-7", customer: "Acme", due_date: null },
    });
    expect(writes(fetchMock).some((w) => w.path.endsWith("/apply"))).toBe(false);
  });

  it("keeps a field being typed while an earlier field's save lands", async () => {
    let saved = draftHeader(1);
    let release = () => {};
    const inner = core();
    const fetchMock = vi.fn((path: string, init?: RequestInit) => {
      if (path === "/api/v2/plans/2/drafts/9/header") return Promise.resolve(json(saved));
      if (path !== "/api/v2/plans/2/drafts/header" || init?.method !== "PUT") {
        return inner(path, init);
      }
      const header = JSON.parse(String(init.body)).header;
      const next = {
        ...saved,
        draft: { ...saved.draft!, header, header_version: saved.draft!.header_version + 1 },
      };
      return new Promise<Response>((resolve) => {
        release = () => {
          saved = next;
          resolve(
            json({ receipt: { profile_id: 2, draft_id: 9, header: next.draft }, header: next }),
          );
        };
      });
    });
    renderAt(fetchMock as unknown as ReturnType<typeof core>);
    const name = await screen.findByRole<HTMLInputElement>("textbox", { name: "Name" });
    fireEvent.change(name, { target: { value: "Bracket kit v3" } });
    fireEvent.keyDown(name, { key: "Enter" });
    await waitFor(() =>
      expect(writes(fetchMock as unknown as ReturnType<typeof core>)).toHaveLength(1),
    );
    fireEvent.change(field("Customer"), { target: { value: "Beta" } });
    act(() => release());
    await screen.findByText(/Saved to the draft/);
    expect(field("Name").value).toBe("Bracket kit v3");
    expect(field("Customer").value).toBe("Beta");
    fireEvent.blur(field("Customer"));
    await waitFor(() =>
      expect(writes(fetchMock as unknown as ReturnType<typeof core>)).toHaveLength(2),
    );
    act(() => release());
    expect(writes(fetchMock as unknown as ReturnType<typeof core>)[1]!.body).toMatchObject({
      expected_header_version: 2,
      header: { name: "Bracket kit v3", customer: "Beta" },
    });
  });

  it("Start a draft right after an edit sends the edit once, not the accepted header", async () => {
    let release = () => {};
    const fetchMock = core({
      open: false,
      write: () =>
        new Promise<Response>((resolve) => {
          release = () =>
            resolve(
              json({
                receipt: { profile_id: 2, draft_id: 9, header: draftHeader(1).draft },
                header: draftHeader(1),
              }),
            );
        }),
    });
    renderAt(fetchMock);
    const name = await screen.findByRole<HTMLInputElement>("textbox", { name: "Name" });
    fireEvent.change(name, { target: { value: "Bracket kit v3" } });
    fireEvent.blur(name);
    fireEvent.click(screen.getByRole("button", { name: "Start a draft" }));
    await waitFor(() => expect(writes(fetchMock)).toHaveLength(1));
    act(() => release());
    await screen.findByText(/Saved to the draft/);
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(writes(fetchMock).map((w) => w.body.header.name)).toEqual(["Bracket kit v3"]);
  });

  it("opens the gate only after a save started by leaving a field has landed", async () => {
    let release = () => {};
    const fetchMock = core({
      write: (path) =>
        path === "/api/v2/plans/2/drafts/header"
          ? new Promise<Response>((resolve) => {
              release = () =>
                resolve(
                  json({
                    receipt: { profile_id: 2, draft_id: 9, header: draftHeader(2).draft },
                    header: draftHeader(2),
                  }),
                );
            })
          : undefined,
    });
    renderAt(fetchMock);
    const name = await screen.findByRole<HTMLInputElement>("textbox", { name: "Name" });
    fireEvent.change(name, { target: { value: "Bracket kit v3" } });
    fireEvent.blur(name);
    fireEvent.click(screen.getByRole("button", { name: "Review and accept" }));
    await waitFor(() => expect(writes(fetchMock)).toHaveLength(1));
    const previews = () =>
      fetchMock.mock.calls.filter(([path]) => String(path).endsWith("/accept-preview")).length;
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(previews()).toBe(0);
    act(() => release());
    await screen.findByRole("dialog", { name: "Accept this draft" });
    expect(previews()).toBeGreaterThan(0);
  });

  it("sends a field changed back to its saved value while an earlier save is running", async () => {
    let saved = draftHeader(1);
    const releases: (() => void)[] = [];
    const inner = core();
    const fetchMock = vi.fn((path: string, init?: RequestInit) => {
      if (path === "/api/v2/plans/2/drafts/9/header") return Promise.resolve(json(saved));
      if (path !== "/api/v2/plans/2/drafts/header" || init?.method !== "PUT") {
        return inner(path, init);
      }
      const header = JSON.parse(String(init.body)).header;
      return new Promise<Response>((resolve) => {
        releases.push(() => {
          const version = saved.draft!.header_version + 1;
          saved = { ...saved, draft: { ...saved.draft!, header, header_version: version } };
          resolve(
            json({ receipt: { profile_id: 2, draft_id: 9, header: saved.draft }, header: saved }),
          );
        });
      });
    });
    const calls = fetchMock as unknown as ReturnType<typeof core>;
    renderAt(calls);
    const customer = await screen.findByRole<HTMLInputElement>("textbox", { name: "Customer" });
    fireEvent.change(customer, { target: { value: "Beta" } });
    fireEvent.blur(customer);
    await waitFor(() => expect(writes(calls)).toHaveLength(1));
    fireEvent.change(customer, { target: { value: "Acme" } });
    fireEvent.blur(customer);
    act(() => releases.shift()!());
    await waitFor(() => expect(writes(calls)).toHaveLength(2));
    act(() => releases.shift()!());
    await screen.findByText(/Saved to the draft/);
    await waitFor(() => expect(saved.draft!.header_version).toBe(3));
    expect(writes(calls).map((w) => w.body.header.customer)).toEqual(["Beta", "Acme"]);
    expect(field("Customer").value).toBe("Acme");
  });

  it("retries a refused header save on the next blur, even while a Part write runs", async () => {
    const puts: string[] = [];
    const held: (() => void)[] = [];
    const fetchMock = core({
      write: (path, init) => {
        if (path === "/api/v2/plans/2/drafts/header") {
          puts.push(JSON.parse(String(init!.body)).header.name);
          return problem(409, "header_changed");
        }
        const receipt = {
          profile_id: 2,
          draft_id: 9,
          reconciliation_id: 1,
          snapshot_digest: DIGEST,
          saved_at: "2026-11-07T00:00:00.000Z",
        };
        return new Promise<Response>((resolve) => {
          held.push(() => resolve(json({ receipt, workspace: workspace() })));
        });
      },
    });
    renderAt(fetchMock);
    const name = await screen.findByRole<HTMLInputElement>("textbox", { name: "Name" });
    fireEvent.change(name, { target: { value: "Bracket kit v3" } });
    fireEvent.blur(name);
    await screen.findByText(/Not saved/);
    fireEvent.click(screen.getByRole("checkbox", { name: "Include gear.stl" }));
    await waitFor(() => expect(writes(fetchMock).some((w) => w.path.endsWith("/save"))).toBe(true));
    fireEvent.blur(field("Name"));
    act(() => held.splice(0).forEach((release) => release()));
    await waitFor(() => expect(puts).toEqual(["Bracket kit v3", "Bracket kit v3"]));
  });

  it("toggles Included from the last click while the first toggle is saving", async () => {
    const held: (() => void)[] = [];
    const fetchMock = core({
      write: (path) =>
        path === "/api/v2/plans/2/drafts/save"
          ? new Promise<Response>((resolve) => {
              held.push(() =>
                resolve(
                  json({
                    receipt: {
                      profile_id: 2,
                      draft_id: 9,
                      reconciliation_id: 1,
                      snapshot_digest: DIGEST,
                      saved_at: "2026-11-07T00:00:00.000Z",
                    },
                    workspace: workspace(),
                  }),
                ),
              );
            })
          : undefined,
    });
    renderAt(fetchMock);
    const box = await screen.findByRole<HTMLInputElement>("checkbox", { name: "Include gear.stl" });
    expect(box.checked).toBe(false);
    fireEvent.click(box);
    await waitFor(() => expect(box.checked).toBe(true));
    fireEvent.click(box);
    await waitFor(() => expect(box.checked).toBe(false));
    act(() => held.splice(0).forEach((release) => release()));
    await waitFor(() => expect(writes(fetchMock)).toHaveLength(2));
    act(() => held.splice(0).forEach((release) => release()));
    expect(writes(fetchMock).map((w) => w.body.decisions[0].value)).toEqual([true, false]);
  });

  it("shows the saved Included value after a refused toggle, even while a later save runs", async () => {
    const fetchMock = core({
      write: (path) => {
        if (path === "/api/v2/plans/2/drafts/save") return problem(409, "draft_changed");
        if (path === "/api/v2/plans/2/drafts/header") return new Promise<Response>(() => {});
        return undefined;
      },
    });
    renderAt(fetchMock);
    const box = await screen.findByRole<HTMLInputElement>("checkbox", { name: "Include gear.stl" });
    fireEvent.click(box);
    await screen.findByText(
      "Not saved: The draft changed somewhere else. The latest draft is shown; make the change again.",
    );
    expect(screen.queryByText(/preview/)).toBeNull();
    await waitFor(() => expect(box.checked).toBe(false));
    fireEvent.change(field("Name"), { target: { value: "Bracket kit v3" } });
    fireEvent.blur(field("Name"));
    await waitFor(() => expect(writes(fetchMock).some((w) => w.method === "PUT")).toBe(true));
    await screen.findByText(/Saving to the draft/);
    expect(box.checked).toBe(false);
  });

  it("reports the write that settled last, not an older refusal", async () => {
    const receipt = {
      profile_id: 2,
      draft_id: 9,
      reconciliation_id: 1,
      snapshot_digest: DIGEST,
      saved_at: "2026-11-07T00:00:00.000Z",
    };
    const fetchMock = core({
      write: (path) => {
        if (path === "/api/v2/plans/2/drafts/header") return problem(409, "header_changed");
        if (path === "/api/v2/plans/2/drafts/save")
          return json({ receipt, workspace: workspace() });
        return undefined;
      },
    });
    renderAt(fetchMock);
    const name = await screen.findByRole<HTMLInputElement>("textbox", { name: "Name" });
    fireEvent.change(name, { target: { value: "Bracket kit v3" } });
    fireEvent.blur(name);
    await screen.findByText(/Not saved/);
    fireEvent.click(screen.getByRole("checkbox", { name: "Include gear.stl" }));
    await screen.findByText("Saved to the draft. The Build changes only when you accept.");
    expect(screen.queryByText(/Not saved/)).toBeNull();
  });

  it("refuses a blank name without sending", async () => {
    const fetchMock = core();
    renderAt(fetchMock);
    const name = await screen.findByRole<HTMLInputElement>("textbox", { name: "Name" });
    fireEvent.change(name, { target: { value: "   " } });
    fireEvent.blur(name);
    expect(await screen.findByText("A Build needs a name.")).toBeTruthy();
    expect(name.getAttribute("aria-invalid")).toBe("true");
    expect(writes(fetchMock)).toHaveLength(0);
  });

  it("shows the accepted header with no draft, and Start a draft saves it unchanged", async () => {
    const fetchMock = core({ open: false });
    renderAt(fetchMock);
    expect(await screen.findByText(/No draft is open/)).toBeTruthy();
    expect(field("Name").value).toBe("Bracket kit");
    expect(screen.queryByRole("table", { name: /Parts/ })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Start a draft" }));
    await waitFor(() => expect(writes(fetchMock)).toHaveLength(1));
    expect(writes(fetchMock)[0]!.body).toEqual({
      expected_base: BASE,
      expected_draft: null,
      expected_header_version: null,
      header: BUILD,
    });
  });

  it("autosaves Part choices as one decision each, against the draft it read", async () => {
    const saved = workspace({ ...DRAFT, lifecycle_version: 3 });
    let current = workspace();
    const fetchMock = core({
      draft: () => current,
      write: (path) => {
        if (path !== "/api/v2/plans/2/drafts/save") return undefined;
        current = saved;
        return json({
          receipt: {
            profile_id: 2,
            draft_id: 9,
            reconciliation_id: 1,
            snapshot_digest: DIGEST,
            saved_at: "2026-11-07T00:00:00.000Z",
          },
          workspace: saved,
        });
      },
    });
    renderAt(fetchMock);
    fireEvent.click(await screen.findByRole("checkbox", { name: "Include gear.stl" }));
    await waitFor(() => expect(writes(fetchMock)).toHaveLength(1));
    const quantity = screen.getByRole<HTMLInputElement>("textbox", {
      name: /Quantity of bracket\.stl/,
    });
    fireEvent.change(quantity, { target: { value: "0" } });
    fireEvent.blur(quantity);
    expect(await screen.findByText(/A whole number from 1 to 10,000/)).toBeTruthy();
    expect(writes(fetchMock)).toHaveLength(1);
    fireEvent.change(quantity, { target: { value: "5" } });
    fireEvent.keyDown(quantity, { key: "Enter" });
    await waitFor(() => expect(writes(fetchMock)).toHaveLength(2));
    const [include, count] = writes(fetchMock);
    const target = (file: string) => ({
      part_key: file,
      relative_path: `stl/${file}`,
      source_layer: "base:Kit",
    });
    expect(include!.body).toEqual({
      decisions: [{ kind: "set_included", target: target("gear.stl"), value: true }],
      expected_base: BASE,
      expected_draft: DRAFT,
      remap_checkoff_links: false,
    });
    expect(count!.body.decisions).toEqual([
      { kind: "set_quantity_override", target: target("bracket.stl"), value: 5 },
    ]);
    expect(count!.body.expected_draft.lifecycle_version).toBe(3);
    expect(include!.key).not.toBe(count!.key);
  });

  it("hides the order fields when the Orders module is off and says why", async () => {
    renderAt(core({ orders: "off" }));
    await screen.findByRole("textbox", { name: "Name" });
    await waitFor(() => expect(screen.queryByRole("textbox", { name: "Customer" })).toBeNull());
    expect(screen.getByText("The Orders module is off.")).toBeTruthy();
  });
});

describe("UI6-02 Accept gate", () => {
  it("shows old and new values, printed units, plates, stock and duplicate orders, and the snapshot", async () => {
    renderAt(core());
    fireEvent.click(await screen.findByRole("button", { name: "Review and accept" }));
    const gate = await screen.findByRole("dialog", { name: "Accept this draft" });
    await within(gate).findByText(/saves a snapshot of the accepted Plan before it accepts/);
    const header = within(gate).getByRole("row", { name: /Name/ });
    expect(header.textContent).toContain("Bracket kit");
    expect(header.textContent).toContain("Bracket kit v2");
    expect(within(gate).getByText("gear.stl: Quantity none to 4")).toBeTruthy();
    expect(within(gate).getByText("gear.stl, unit 1: printed")).toBeTruthy();
    expect(within(gate).getByText("Plate 1 on Voron 2.4 is cleared (3 units)")).toBeTruthy();
    expect(within(gate).getByText(/Spoolman is off/)).toBeTruthy();
    expect(within(gate).getByText("Order PO-7 is also on Other kit")).toBeTruthy();
  });

  it("keeps Accept off with the reason while work blocks it", async () => {
    const fetchMock = core({ preview: () => json(preview(1, true)) });
    renderAt(fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Review and accept" }));
    const gate = await screen.findByRole("dialog", { name: "Accept this draft" });
    await within(gate).findByText(/Accept waits for running work to finish: print job/);
    const accept = within(gate).getByRole("button", { name: "Accept" });
    expect(accept.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(accept);
    expect(writes(fetchMock)).toHaveLength(0);
  });
  it("keeps Accept off while checkoff links or queued sends point at the Plan", async () => {
    const held = preview(1);
    if (held.kind === "changed") {
      held.changes.production = {
        checkoff_link_count: 2,
        send_queue_item_count: 1,
        unmappable: [],
      };
    }
    const fetchMock = core({ preview: () => json(held) });
    renderAt(fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Review and accept" }));
    const gate = await screen.findByRole("dialog", { name: "Accept this draft" });
    await within(gate).findByText(/2 checkoff links and 1 queued send point at this Plan/);
    const accept = within(gate).getByRole("button", { name: "Accept" });
    expect(accept.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(accept);
    expect(writes(fetchMock)).toHaveLength(0);
  });
});

describe("UI6-03 and UI6-04 Accept sends the previewed revision", () => {
  it("accepts exactly the previewed revision with a key, then shows the new version", async () => {
    const fetchMock = core({
      write: (path) => (path === "/api/v2/plans/2/drafts/9/apply" ? json(RECEIPT) : undefined),
    });
    renderAt(fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Review and accept" }));
    const gate = await screen.findByRole("dialog", { name: "Accept this draft" });
    await within(gate).findByText(/saves a snapshot/);
    fireEvent.click(within(gate).getByRole("button", { name: "Accept" }));
    expect(await screen.findByText(/Accepted\. The Plan is now version/)).toBeTruthy();
    const [sent] = writes(fetchMock);
    expect(sent).toMatchObject({ path: "/api/v2/plans/2/drafts/9/apply", method: "POST" });
    expect(sent!.key).toMatch(/\S/);
    expect(sent!.body).toEqual({
      expected_snapshot_digest: DIGEST,
      expected_lifecycle_version: 2,
      expected_base: BASE,
      expected_header_version: 1,
      remap_checkoff_links: false,
    });
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("keeps Accept off while it previews again after a refusal", async () => {
    let previews = 0;
    let release = () => {};
    const fetchMock = core({
      preview: () => {
        previews += 1;
        if (previews === 1) return json(preview(1));
        return new Promise<Response>((resolve) => {
          release = () => resolve(json(preview(2)));
        });
      },
      write: (path) =>
        path === "/api/v2/plans/2/drafts/9/apply" ? problem(409, "draft_changed") : undefined,
    });
    renderAt(fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Review and accept" }));
    const gate = await screen.findByRole("dialog", { name: "Accept this draft" });
    await within(gate).findByText(/saves a snapshot/);
    fireEvent.click(within(gate).getByRole("button", { name: "Accept" }));
    await within(gate).findByText(/The draft changed after this preview/);
    await waitFor(() => expect(previews).toBe(2));
    const accept = within(gate).getByRole("button", { name: "Accept" });
    await waitFor(() => expect(accept.getAttribute("aria-disabled")).toBe("true"));
    fireEvent.click(accept);
    expect(writes(fetchMock).filter((w) => w.path.endsWith("/apply"))).toHaveLength(1);
    act(() => release());
    await waitFor(() => expect(accept.getAttribute("aria-disabled")).toBeNull());
  });

  it("keeps Accept off and hides the old changes when the preview after a refusal fails", async () => {
    let previews = 0;
    const fetchMock = core({
      preview: () => (++previews === 1 ? json(preview(1)) : problem(409, "base_changed")),
      write: (path) =>
        path === "/api/v2/plans/2/drafts/9/apply" ? problem(409, "draft_changed") : undefined,
    });
    renderAt(fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Review and accept" }));
    const gate = await screen.findByRole("dialog", { name: "Accept this draft" });
    await within(gate).findByText(/saves a snapshot/);
    fireEvent.click(within(gate).getByRole("button", { name: "Accept" }));
    await within(gate).findByRole("button", { name: "Check again" });
    expect(previews).toBe(2);
    expect(within(gate).queryByText(/saves a snapshot/)).toBeNull();
    const accept = within(gate).getByRole("button", { name: "Accept" });
    expect(accept.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(accept);
    expect(writes(fetchMock).filter((w) => w.path.endsWith("/apply"))).toHaveLength(1);
  });

  it("refuses a stale Accept, previews again and sends the next Accept with a new key", async () => {
    let accepts = 0;
    let previews = 0;
    const fetchMock = core({
      preview: () => json(preview(++previews)),
      write: (path) => {
        if (path !== "/api/v2/plans/2/drafts/9/apply") return undefined;
        accepts += 1;
        return accepts === 1 ? problem(409, "draft_changed") : json(RECEIPT);
      },
    });
    renderAt(fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Review and accept" }));
    const gate = await screen.findByRole("dialog", { name: "Accept this draft" });
    await within(gate).findByText(/saves a snapshot/);
    fireEvent.click(within(gate).getByRole("button", { name: "Accept" }));
    expect(await within(gate).findByText(/The draft changed after this preview/)).toBeTruthy();
    await waitFor(() => expect(previews).toBe(2));
    await waitFor(() =>
      expect(
        within(gate).getByRole("button", { name: "Accept" }).getAttribute("aria-disabled"),
      ).toBeNull(),
    );
    fireEvent.click(within(gate).getByRole("button", { name: "Accept" }));
    await screen.findByText(/Accepted\. The Plan is now version/);
    const [stale, fresh] = writes(fetchMock);
    expect(stale!.body.expected_header_version).toBe(1);
    expect(fresh!.body.expected_header_version).toBe(2);
    expect(fresh!.key).not.toBe(stale!.key);
  });

  it("shows a refused preview in words", async () => {
    renderAt(core({ preview: () => problem(409, "duplicate_build_name") }));
    fireEvent.click(await screen.findByRole("button", { name: "Review and accept" }));
    const gate = await screen.findByRole("dialog", { name: "Accept this draft" });
    expect(await within(gate).findByText(/Another Build already has this name/)).toBeTruthy();
    expect(within(gate).getByRole("button", { name: "Accept" }).getAttribute("aria-disabled")).toBe(
      "true",
    );
  });
});

describe("UI6-05 keyboard, focus, disabled reasons and states", () => {
  it("opens the gate with focus inside and returns focus on Escape", async () => {
    renderAt(core());
    const open = await screen.findByRole("button", { name: "Review and accept" });
    open.focus();
    fireEvent.click(open);
    const gate = await screen.findByRole("dialog", { name: "Accept this draft" });
    await waitFor(() => expect(gate.contains(document.activeElement)).toBe(true));
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(document.activeElement).toBe(open);
  });

  it("turns every change off offline, with the reason", async () => {
    let down = false;
    const handler = core();
    const fetchMock = vi.fn((path: string, init?: RequestInit) =>
      down ? Promise.reject(new TypeError("Failed to fetch")) : handler(path, init),
    );
    vi.useFakeTimers({ shouldAdvanceTime: true });
    renderAt(fetchMock as unknown as ReturnType<typeof core>);
    await screen.findByRole("textbox", { name: "Name" });
    down = true;
    await act(() => vi.advanceTimersByTimeAsync(31_000));
    await waitFor(() => expect(screen.getByText("Offline")).toBeTruthy());
    expect(field("Name").readOnly).toBe(true);
    const review = screen.getByRole("button", { name: "Review and accept" });
    expect(review.getAttribute("aria-disabled")).toBe("true");
    expect(screen.getAllByText(OFFLINE_REASON).length).toBeGreaterThan(0);
    fireEvent.click(screen.getByRole("checkbox", { name: "Include gear.stl" }));
    expect(fetchMock.mock.calls.some(([, init]) => init?.method && init.method !== "GET")).toBe(
      false,
    );
  });

  it("says it could not load and offers Retry", async () => {
    let fail = true;
    const handler = core();
    const fetchMock = vi.fn((path: string, init?: RequestInit) =>
      fail && path === "/api/v2/plans/2/drafts"
        ? Promise.resolve(json({ type: "about:blank", title: "Error", status: 500 }, 500))
        : handler(path, init),
    );
    renderAt(fetchMock as unknown as ReturnType<typeof core>);
    expect(await screen.findByText("Could not load Plan.", {}, { timeout: 5000 })).toBeTruthy();
    fail = false;
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByRole("textbox", { name: "Name" })).toBeTruthy();
  });
});
