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
import { DRAFT_OPEN, NO_DRAFT } from "./planCopy";

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

const overridden = (quantity: number): DraftWorkspace => ({
  ...workspace(),
  parts: [part(11, "bracket.stl", true, quantity), part(12, "gear.stl", false, 4)],
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

const preview = (
  version: number,
  blocked = false,
): Extract<DraftAcceptPreview, { kind: "changed" }> => ({
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
  return queryClient;
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

describe("Order fields in the Accept gate", () => {
  it("creates a draft from order edits and previews all three saved fields", async () => {
    let saved: DraftHeaderView["draft"] = null;
    const inner = core({ open: false });
    const fetchMock = vi.fn((path: string, init?: RequestInit) => {
      if (path === "/api/v2/plans/2/drafts/header" && init?.method === "PUT") {
        saved = {
          header: JSON.parse(String(init.body)).header,
          header_version: 1,
          updated_at: "2026-11-11T00:00:00.000Z",
        };
        return Promise.resolve(
          json({
            receipt: { ...RECEIPT, header: saved },
            header: { draft_id: 9, build: BUILD, draft: saved },
          }),
        );
      }
      if (saved) {
        if (path === "/api/v2/plans/2/drafts")
          return Promise.resolve(json({ ...workspace(), drafts: [DRAFT] }));
        if (path === "/api/v2/plans/2/drafts/9/header")
          return Promise.resolve(json({ draft_id: 9, build: BUILD, draft: saved }));
        if (path.endsWith("/accept-preview")) {
          const result = preview(1);
          result.changes.header = [
            { field: "order_number", old: BUILD.order_number, new: saved.header.order_number },
            { field: "customer", old: null, new: saved.header.customer },
            { field: "due_date", old: null, new: saved.header.due_date },
          ];
          return Promise.resolve(json(result));
        }
      }
      return inner(path, init);
    });
    renderAt(fetchMock);
    await screen.findByText(NO_DRAFT);
    fireEvent.change(field("Order number"), { target: { value: "PO-42" } });
    fireEvent.change(field("Customer"), { target: { value: "Synthetic customer" } });
    const due = screen.getByLabelText("Due date");
    fireEvent.change(due, { target: { value: "2026-12-15" } });
    fireEvent.blur(due);
    await screen.findByText(/Saved to the draft/);
    expect(writes(fetchMock)).toHaveLength(1);
    expect(writes(fetchMock)[0]!.body).toEqual({
      expected_base: BASE,
      expected_draft: null,
      expected_header_version: null,
      header: {
        ...BUILD,
        order_number: "PO-42",
        customer: "Synthetic customer",
        due_date: "2026-12-15",
      },
    });
    await screen.findByText(DRAFT_OPEN);
    fireEvent.click(screen.getByRole("button", { name: "Review and accept" }));
    const gate = await screen.findByRole("dialog");
    for (const [label, value] of [
      ["Order number", "PO-42"],
      ["Customer", "Synthetic customer"],
      ["Due date", "2026-12-15"],
    ]) {
      const row = await within(gate).findByRole("row", { name: new RegExp(label!) });
      expect(within(row).getByText(value!)).toBeTruthy();
    }
    expect(writes(fetchMock)).toHaveLength(1);
  });

  it.each(["preview", "accept"])(
    "names every holding Build in a duplicate-order %s refusal",
    async (stage) => {
      const refusal = () =>
        json(
          { code: "duplicate_order", builds: [{ name: "Other kit" }, { name: "Spare kit" }] },
          409,
        );
      const fetchMock = core({
        preview: stage === "preview" ? refusal : undefined,
        write: (path) => (path.endsWith("/apply") ? refusal() : undefined),
      });
      renderAt(fetchMock);
      fireEvent.click(await screen.findByRole("button", { name: "Review and accept" }));
      const gate = await screen.findByRole("dialog");
      if (stage === "accept") {
        await within(gate).findByText(/Print Partner saves a snapshot/);
        fireEvent.click(within(gate).getByRole("button", { name: /^Accept$/ }));
      }
      const alert = await within(gate).findByRole("alert");
      expect(alert.textContent).toContain("Order already held by: Other kit, Spare kit");
      expect(writes(fetchMock)).toHaveLength(stage === "accept" ? 1 : 0);
      if (stage === "preview")
        expect(
          within(gate)
            .getByRole("button", { name: /^Accept$/ })
            .getAttribute("aria-disabled"),
        ).toBe("true");
    },
  );
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

  it("keeps a typed header value on the page when its save answers 401", async () => {
    const fetchMock = core({
      write: (path) =>
        path === "/api/v2/plans/2/drafts/header" ? json({ detail: "refused" }, 401) : undefined,
    });
    renderAt(fetchMock);
    const name = await screen.findByRole<HTMLInputElement>("textbox", { name: "Name" });
    fireEvent.change(name, { target: { value: "Bracket kit v3" } });
    fireEvent.keyDown(name, { key: "Enter" });
    const link = await screen.findByRole("link", { name: "Sign in to save." });
    expect(link.closest("p")?.textContent).toBe("You were signed out. Sign in to save.");
    expect(link.getAttribute("href")).toBe("/login?next=%2Fbuilds%2F2%2Fplan");
    // A write's 401 never navigates: the Plan and the typed value stay.
    expect(field("Name").value).toBe("Bracket kit v3");
    expect(writes(fetchMock)).toHaveLength(1);
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

  it("sends a quantity once when Enter is followed by a blur while it saves", async () => {
    const held: (() => void)[] = [];
    const receipt = {
      profile_id: 2,
      draft_id: 9,
      reconciliation_id: 1,
      snapshot_digest: DIGEST,
      saved_at: "2026-11-07T00:00:00.000Z",
    };
    const fetchMock = core({
      write: (path) =>
        path === "/api/v2/plans/2/drafts/save"
          ? new Promise<Response>((resolve) => {
              held.push(() => resolve(json({ receipt, workspace: overridden(5) })));
            })
          : undefined,
    });
    renderAt(fetchMock);
    const quantity = await screen.findByRole<HTMLInputElement>("textbox", {
      name: /Quantity of bracket\.stl/,
    });
    fireEvent.change(quantity, { target: { value: "5" } });
    fireEvent.keyDown(quantity, { key: "Enter" });
    await waitFor(() => expect(writes(fetchMock)).toHaveLength(1));
    fireEvent.blur(quantity);
    act(() => held.splice(0).forEach((release) => release()));
    await screen.findByText("Saved to the draft. The Build changes only when you accept.");
    act(() => held.splice(0).forEach((release) => release()));
    expect(writes(fetchMock)).toHaveLength(1);
    expect(quantity.isConnected).toBe(true);
  });

  it("keeps focus and later typing in a quantity when its save lands", async () => {
    const held: (() => void)[] = [];
    const receipt = {
      profile_id: 2,
      draft_id: 9,
      reconciliation_id: 1,
      snapshot_digest: DIGEST,
      saved_at: "2026-11-07T00:00:00.000Z",
    };
    let current = workspace();
    const fetchMock = core({
      draft: () => current,
      write: (path) =>
        path === "/api/v2/plans/2/drafts/save"
          ? new Promise<Response>((resolve) => {
              held.push(() => {
                current = overridden(5);
                resolve(json({ receipt, workspace: current }));
              });
            })
          : undefined,
    });
    renderAt(fetchMock);
    const quantity = await screen.findByRole<HTMLInputElement>("textbox", {
      name: /Quantity of bracket\.stl/,
    });
    quantity.focus();
    fireEvent.change(quantity, { target: { value: "5" } });
    fireEvent.keyDown(quantity, { key: "Enter" });
    await waitFor(() => expect(writes(fetchMock)).toHaveLength(1));
    fireEvent.change(quantity, { target: { value: "6" } });
    act(() => held.splice(0).forEach((release) => release()));
    await screen.findByText("Saved to the draft. The Build changes only when you accept.");
    expect(quantity.isConnected).toBe(true);
    expect(document.activeElement).toBe(quantity);
    expect(quantity.value).toBe("6");
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

  it("says an open draft saves as it changes and Accept snapshots the accepted Plan first", async () => {
    renderAt(core());
    expect(await screen.findByText(DRAFT_OPEN)).toBeTruthy();
    expect(DRAFT_OPEN).toMatch(/Changes save to it as you make them/);
    expect(DRAFT_OPEN).toMatch(/the Build changes only when you accept/);
    expect(DRAFT_OPEN).toMatch(/Accept first saves a snapshot of the accepted Plan when it can/);
  });

  it("shows the accepted header with no draft, and Start a draft saves it unchanged", async () => {
    const fetchMock = core({ open: false });
    renderAt(fetchMock);
    expect(await screen.findByText(NO_DRAFT)).toBeTruthy();
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

  it("drops the draft save status once the draft is accepted", async () => {
    const fetchMock = core({
      write: (path) => {
        if (path === "/api/v2/plans/2/drafts/9/apply") return json(RECEIPT);
        if (path !== "/api/v2/plans/2/drafts/save") return undefined;
        return json({
          receipt: {
            profile_id: 2,
            draft_id: 9,
            reconciliation_id: 1,
            snapshot_digest: DIGEST,
            saved_at: "2026-11-07T00:00:00.000Z",
          },
          workspace: workspace(),
        });
      },
    });
    renderAt(fetchMock);
    fireEvent.click(await screen.findByRole("checkbox", { name: "Include gear.stl" }));
    await screen.findByText("Saved to the draft. The Build changes only when you accept.");
    fireEvent.click(screen.getByRole("button", { name: "Review and accept" }));
    const gate = await screen.findByRole("dialog", { name: "Accept this draft" });
    await within(gate).findByText(/saves a snapshot/);
    fireEvent.click(within(gate).getByRole("button", { name: "Accept" }));
    expect(await screen.findByText(/Accepted\. The Plan is now version/)).toBeTruthy();
    expect(screen.queryByText(/Saved to the draft/)).toBeNull();
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
    expect(
      within(gate).getByText("The preview failed. Check again before you accept."),
    ).toBeTruthy();
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

  it("resends an Accept whose outcome is unknown with the same key, without a new preview", async () => {
    let accepts = 0;
    let previews = 0;
    const fetchMock = core({
      preview: () => json(preview(++previews)),
      write: (path) => {
        if (path !== "/api/v2/plans/2/drafts/9/apply") return undefined;
        accepts += 1;
        return accepts === 1 ? problem(503, "outcome_unknown") : json(RECEIPT);
      },
    });
    renderAt(fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Review and accept" }));
    const gate = await screen.findByRole("dialog", { name: "Accept this draft" });
    await within(gate).findByText(/saves a snapshot/);
    fireEvent.click(within(gate).getByRole("button", { name: "Accept" }));
    await screen.findByText(/Accepted\. The Plan is now version/);
    const [first, replay] = writes(fetchMock);
    expect(replay!.key).toBe(first!.key);
    expect(previews).toBe(1);
  });

  it("keeps the Accept key after unknown outcomes, so the next Accept replays it", async () => {
    let accepts = 0;
    let previews = 0;
    const fetchMock = core({
      preview: () => json(preview(++previews)),
      write: (path) => {
        if (path !== "/api/v2/plans/2/drafts/9/apply") return undefined;
        accepts += 1;
        return accepts <= 3 ? problem(503, "outcome_unknown") : json(RECEIPT);
      },
    });
    renderAt(fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Review and accept" }));
    const gate = await screen.findByRole("dialog", { name: "Accept this draft" });
    await within(gate).findByText(/saves a snapshot/);
    fireEvent.click(within(gate).getByRole("button", { name: "Accept" }));
    await waitFor(() => expect(accepts).toBe(3));
    expect(
      await within(gate).findByText(/could not confirm whether Accept went through/),
    ).toBeTruthy();
    await waitFor(() =>
      expect(
        within(gate).getByRole("button", { name: "Accept" }).getAttribute("aria-disabled"),
      ).toBeNull(),
    );
    fireEvent.click(within(gate).getByRole("button", { name: "Accept" }));
    await screen.findByText(/Accepted\. The Plan is now version/);
    expect(new Set(writes(fetchMock).map((sent) => sent.key)).size).toBe(1);
    expect(previews).toBe(1);
  });

  it("resends a draft save whose outcome is unknown with the same key", async () => {
    let saves = 0;
    const fetchMock = core({
      write: (path) => {
        if (path !== "/api/v2/plans/2/drafts/save") return undefined;
        saves += 1;
        return saves === 1
          ? problem(503, "outcome_unknown")
          : json({
              receipt: {
                profile_id: 2,
                draft_id: 9,
                reconciliation_id: 1,
                snapshot_digest: DIGEST,
                saved_at: "2026-11-07T00:00:00.000Z",
              },
              workspace: workspace(),
            });
      },
    });
    renderAt(fetchMock);
    fireEvent.click(await screen.findByRole("checkbox", { name: "Include gear.stl" }));
    await screen.findByText("Saved to the draft. The Build changes only when you accept.");
    const [first, replay] = writes(fetchMock);
    expect(replay!.key).toBe(first!.key);
  });

  it("says a header save whose outcome stays unknown is not confirmed, resending one key", async () => {
    const fetchMock = core({
      write: (path) =>
        path === "/api/v2/plans/2/drafts/header" ? problem(503, "outcome_unknown") : undefined,
    });
    renderAt(fetchMock);
    const name = await screen.findByRole<HTMLInputElement>("textbox", { name: "Name" });
    fireEvent.change(name, { target: { value: "Bracket kit v3" } });
    fireEvent.blur(name);
    expect(
      await screen.findByText(
        /^Save not confirmed: The core could not confirm this save\. Check the field/,
      ),
    ).toBeTruthy();
    expect(name.value).toBe("Bracket kit v3");
    const sent = writes(fetchMock);
    expect(sent).toHaveLength(3);
    expect(new Set(sent.map((write) => write.key)).size).toBe(1);
  });

  it("asks to check again when a preview's outcome is unknown, without Accept's text", async () => {
    renderAt(core({ preview: () => problem(503, "outcome_unknown") }));
    fireEvent.click(await screen.findByRole("button", { name: "Review and accept" }));
    const gate = await screen.findByRole("dialog", { name: "Accept this draft" });
    expect(
      await within(gate).findByText("The core could not answer for certain. Check again."),
    ).toBeTruthy();
    expect(within(gate).queryByText(/Accept went through/)).toBeNull();
  });

  it("resends a lost Part write with its first body after the Plan refetches", async () => {
    const held: (() => void)[] = [];
    let saves = 0;
    const fetchMock = core({
      write: (path) => {
        if (path !== "/api/v2/plans/2/drafts/save") return undefined;
        saves += 1;
        if (saves === 1) {
          return new Promise<Response>((_, reject) => {
            held.push(() => reject(new TypeError("Failed to fetch")));
          });
        }
        return json({
          receipt: {
            profile_id: 2,
            draft_id: 9,
            reconciliation_id: 1,
            snapshot_digest: DIGEST,
            saved_at: "2026-11-07T00:00:00.000Z",
          },
          workspace: workspace(),
        });
      },
    });
    const client = renderAt(fetchMock);
    fireEvent.click(await screen.findByRole("checkbox", { name: "Include gear.stl" }));
    await waitFor(() => expect(held).toHaveLength(1));
    act(() => {
      client.setQueryData<{ draft: { workspace: DraftWorkspace } }>(
        ["core", "plan", 2],
        (data) =>
          data && {
            ...data,
            draft: { ...data.draft, workspace: workspace({ ...DRAFT, lifecycle_version: 3 }) },
          },
      );
      held.splice(0).forEach((reject) => reject());
    });
    await screen.findByText("Saved to the draft. The Build changes only when you accept.");
    const [first, resend] = writes(fetchMock);
    expect(resend!.key).toBe(first!.key);
    expect(resend!.body).toEqual(first!.body);
  });

  it("resends a lost header write with its first body after the Plan refetches", async () => {
    const held: (() => void)[] = [];
    let saves = 0;
    const fetchMock = core({
      write: (path) => {
        if (path !== "/api/v2/plans/2/drafts/header") return undefined;
        saves += 1;
        if (saves === 1) {
          return new Promise<Response>((_, reject) => {
            held.push(() => reject(new TypeError("Failed to fetch")));
          });
        }
        return problem(503, "outcome_unknown");
      },
    });
    const client = renderAt(fetchMock);
    const name = await screen.findByRole<HTMLInputElement>("textbox", { name: "Name" });
    fireEvent.change(name, { target: { value: "Bracket kit v3" } });
    fireEvent.blur(name);
    await waitFor(() => expect(held).toHaveLength(1));
    act(() => {
      client.setQueryData<{ draft: { header: DraftHeaderView } }>(
        ["core", "plan", 2],
        (data) => data && { ...data, draft: { ...data.draft, header: draftHeader(2) } },
      );
      held.splice(0).forEach((reject) => reject());
    });
    await screen.findByText(/^Save not confirmed/);
    const [first, ...resends] = writes(fetchMock);
    expect(resends.length).toBeGreaterThan(0);
    for (const resend of resends) {
      expect(resend.key).toBe(first!.key);
      expect(resend.body).toEqual(first!.body);
    }
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

describe("GRE-1276 the Accept gate matches printed units first", () => {
  const RECONCILE = "/api/v2/plans/2/drafts/9/reconciliation";
  const MATCHED = "d".repeat(64);
  const unmatched = (): DraftWorkspace => ({
    ...workspace(),
    reconciliation: { kind: "unresolved", conflicts: [] },
  });
  const matched = (): DraftWorkspace => ({
    ...workspace({ ...DRAFT, snapshot_digest: MATCHED }),
    reconciliation: { kind: "ready", reused_units: 4, new_units: 2, surplus_units: 1 },
  });
  const needsMatch = () =>
    json(
      {
        type: "urn:print-partner:problem:conflict",
        title: "Conflict",
        status: 409,
        code: "reconciliation_required",
        reason: "missing",
      },
      409,
    );
  const reconciles = (fetchMock: ReturnType<typeof core>) =>
    writes(fetchMock).filter((sent) => sent.path === RECONCILE);
  const accept = async (gate: HTMLElement) => {
    const button = within(gate).getByRole("button", { name: "Accept" });
    await waitFor(() => expect(button.getAttribute("aria-disabled")).toBeNull());
    fireEvent.click(button);
    await screen.findByText(/Accepted\. The Plan is now version/);
  };

  it("settles the automatic match, resending a lost send with its key, then accepts", async () => {
    let settled = false;
    let sends = 0;
    const fetchMock = core({
      draft: () => (settled ? matched() : unmatched()),
      preview: () => (settled ? json(preview(1)) : needsMatch()),
      write: (path) => {
        if (path === "/api/v2/plans/2/drafts/9/apply") return json(RECEIPT);
        if (path !== RECONCILE) return undefined;
        sends += 1;
        if (sends === 1) return Promise.reject(new TypeError("Failed to fetch"));
        settled = true;
        return json(matched());
      },
    });
    renderAt(fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Review and accept" }));
    const gate = await screen.findByRole("dialog", { name: "Accept this draft" });
    const done = await within(gate).findByText(
      "Printed units matched: 4 units kept, 2 new units, 1 unit no longer needed.",
    );
    await waitFor(() => expect(document.activeElement).toBe(done));
    const [first, resend] = reconciles(fetchMock);
    expect(first!.method).toBe("PUT");
    expect(first!.key).toBeTruthy();
    expect(first!.body).toEqual({ decisions: [], expected_snapshot_digest: DIGEST });
    expect(resend).toEqual(first);
    expect(within(gate).queryByText(/Another Build/)).toBeNull();
    await accept(gate);
    expect(reconciles(fetchMock)).toHaveLength(2);
  });

  it("keeps Accept off and shows the step while the match is saving", async () => {
    const held: ((value: Response) => void)[] = [];
    const fetchMock = core({
      draft: unmatched,
      preview: needsMatch,
      write: (path) =>
        path === RECONCILE ? new Promise<Response>((resolve) => held.push(resolve)) : undefined,
    });
    renderAt(fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Review and accept" }));
    const gate = await screen.findByRole("dialog", { name: "Accept this draft" });
    await within(gate).findByRole("heading", { name: "Match printed units" });
    await within(gate).findByText("Matching printed units to this draft.");
    const button = within(gate).getByRole("button", { name: "Accept" });
    expect(button.getAttribute("aria-disabled")).toBe("true");
    expect(within(gate).getByText("Match the printed units first.")).toBeTruthy();
    expect(within(gate).queryByRole("button", { name: "Match printed units" })).toBeNull();
    fireEvent.click(button);
    // Let any mutation the click started reach fetch before checking that none did.
    await act(() => new Promise((resolve) => setTimeout(resolve, 20)));
    expect(writes(fetchMock).some((sent) => sent.path.endsWith("/apply"))).toBe(false);
    act(() => held.splice(0).forEach((resolve) => resolve(json(matched()))));
  });

  it("changes nothing for a draft that needs no match", async () => {
    const fetchMock = core({
      write: (path) => (path.endsWith("/apply") ? json(RECEIPT) : undefined),
    });
    renderAt(fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Review and accept" }));
    const gate = await screen.findByRole("dialog", { name: "Accept this draft" });
    await within(gate).findByText(/saves a snapshot/);
    expect(within(gate).queryByRole("heading", { name: "Match printed units" })).toBeNull();
    expect(within(gate).queryByText(/Printed units matched/)).toBeNull();
    await accept(gate);
    expect(reconciles(fetchMock)).toHaveLength(0);
  });

  it("asks the user's decisions in plain words and sends them", async () => {
    let settled = false;
    const conflicted = (): DraftWorkspace => ({
      ...workspace(),
      parts: [part(11, "bracket.stl", true, null), part(12, "gear.stl", true, 4)],
      diff: {
        base_is_current: true,
        added: [],
        changed: [],
        removed: [
          {
            revision_part_id: 20,
            filename: "bracket-old.stl",
            relative_path: "stl/bracket-old.stl",
            source_layer: "base:Kit",
          },
        ],
      },
      reconciliation: {
        kind: "unresolved",
        conflicts: [
          {
            kind: "ambiguous_exact_match",
            target_draft_part_id: 11,
            candidate_revision_part_ids: [11, 20],
          },
          {
            kind: "unsafe_predecessor",
            target_draft_part_id: 12,
            predecessor_revision_part_id: 12,
          },
        ],
      },
    });
    const fetchMock = core({
      draft: () => (settled ? matched() : conflicted()),
      preview: () => (settled ? json(preview(1)) : needsMatch()),
      write: (path) => {
        if (path === "/api/v2/plans/2/drafts/9/apply") return json(RECEIPT);
        if (path !== RECONCILE) return undefined;
        settled = true;
        return json(matched());
      },
    });
    renderAt(fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Review and accept" }));
    const gate = await screen.findByRole("dialog", { name: "Accept this draft" });
    const ambiguous = await within(gate).findByRole("group", {
      name: "bracket.stl matches more than one Part in the accepted Plan. Which printed units does it keep?",
    });
    const unsafe = within(gate).getByRole("group", {
      name: /^gear\.stl changed since it was accepted/,
    });
    expect(reconciles(fetchMock)).toHaveLength(0);
    const save = within(gate).getByRole("button", { name: "Save these choices" });
    expect(save.getAttribute("aria-disabled")).toBe("true");
    expect(within(gate).getByText("Choose for 2 more Parts first.")).toBeTruthy();
    fireEvent.click(
      within(ambiguous).getByRole("radio", {
        name: "Keep the printed units of the accepted bracket-old.stl (Part 20).",
      }),
    );
    fireEvent.click(
      within(unsafe).getByRole("radio", {
        name: "Keep the units already printed for the accepted gear.stl.",
      }),
    );
    fireEvent.click(save);
    await within(gate).findByText(/^Printed units matched/);
    expect(reconciles(fetchMock).map((sent) => sent.body)).toEqual([
      {
        expected_snapshot_digest: DIGEST,
        decisions: [
          {
            kind: "select_exact_predecessor",
            target_draft_part_id: 11,
            predecessor_revision_part_id: 20,
          },
          {
            kind: "accept_prior_completion",
            target_draft_part_id: 12,
            predecessor_revision_part_id: 12,
          },
        ],
      },
    ]);
    await accept(gate);
  });

  it("offers to start over when a choice claims units another Part keeps", async () => {
    const ambiguous = (): DraftWorkspace => ({
      ...workspace(),
      reconciliation: {
        kind: "unresolved",
        conflicts: [
          {
            kind: "ambiguous_exact_match",
            target_draft_part_id: 11,
            candidate_revision_part_ids: [11, 12],
          },
        ],
      },
    });
    const claimed = (): DraftWorkspace => ({
      ...workspace({ ...DRAFT, snapshot_digest: MATCHED }),
      reconciliation: {
        kind: "unresolved",
        conflicts: [
          {
            kind: "predecessor_claimed",
            target_draft_part_id: 12,
            predecessor_revision_part_id: 12,
          },
        ],
      },
    });
    let sends = 0;
    const fetchMock = core({
      draft: () => (sends === 1 ? claimed() : ambiguous()),
      preview: needsMatch,
      write: (path) => {
        if (path !== RECONCILE) return undefined;
        sends += 1;
        return json(sends === 1 ? claimed() : ambiguous());
      },
    });
    renderAt(fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Review and accept" }));
    const gate = await screen.findByRole("dialog", { name: "Accept this draft" });
    const group = await within(gate).findByRole("group", { name: /^bracket\.stl matches/ });
    fireEvent.click(within(group).getByRole("radio", { name: /accepted gear\.stl \(Part 12\)/ }));
    fireEvent.click(within(gate).getByRole("button", { name: "Save these choices" }));
    expect(
      await within(gate).findByText(
        "gear.stl and another Part both use the printed units of the accepted gear.stl. Only one can keep them. Start the match over to choose again, or change the Parts.",
      ),
    ).toBeTruthy();
    const heading = within(gate).getByRole("heading", { name: "Match printed units" });
    await waitFor(() => expect(document.activeElement).toBe(heading));
    fireEvent.click(within(gate).getByRole("button", { name: "Start the match over" }));
    await within(gate).findByRole("group", { name: /^bracket\.stl matches/ });
    const [first, second] = reconciles(fetchMock);
    expect(first!.body.decisions).toEqual([
      {
        kind: "select_exact_predecessor",
        target_draft_part_id: 11,
        predecessor_revision_part_id: 12,
      },
    ]);
    expect(second!.body).toEqual({ decisions: [], expected_snapshot_digest: MATCHED });
    expect(within(gate).getByRole("button", { name: "Accept" }).getAttribute("aria-disabled")).toBe(
      "true",
    );
  });

  describe("keeps a pick only while it is still one of its Part's options", () => {
    type Conflict = Extract<
      DraftWorkspace["reconciliation"],
      { kind: "unresolved" }
    >["conflicts"][number];
    const UNSAFE_GEAR: Conflict = {
      kind: "unsafe_predecessor",
      target_draft_part_id: 12,
      predecessor_revision_part_id: 12,
    };
    const ambiguousBracket = (candidates: number[]): Conflict => ({
      kind: "ambiguous_exact_match",
      target_draft_part_id: 11,
      candidate_revision_part_ids: candidates,
    });
    const conflicted = (conflicts: Conflict[], digest = DIGEST): DraftWorkspace => ({
      ...workspace({ ...DRAFT, snapshot_digest: digest }),
      parts: [part(11, "bracket.stl", true, null), part(12, "gear.stl", true, 4)],
      diff: {
        base_is_current: true,
        added: [],
        changed: [],
        removed: [20, 30].map((id) => ({
          revision_part_id: id,
          filename: `bracket-${id}.stl`,
          relative_path: `stl/bracket-${id}.stl`,
          source_layer: "base:Kit",
        })),
      },
      reconciliation: { kind: "unresolved", conflicts },
    });

    /** Answers both conflicts, saves, and lets the core come back with `after`. */
    async function answerThenChange(after: Conflict[]) {
      let sends = 0;
      const fetchMock = core({
        draft: () =>
          sends === 0
            ? conflicted([ambiguousBracket([11, 20]), UNSAFE_GEAR])
            : conflicted(after, MATCHED),
        preview: needsMatch,
        write: (path) => {
          if (path !== RECONCILE) return undefined;
          sends += 1;
          return json(conflicted(after, MATCHED));
        },
      });
      renderAt(fetchMock);
      fireEvent.click(await screen.findByRole("button", { name: "Review and accept" }));
      const gate = await screen.findByRole("dialog", { name: "Accept this draft" });
      const bracket = await within(gate).findByRole("group", { name: /^bracket\.stl / });
      fireEvent.click(within(bracket).getByRole("radio", { name: /bracket-20\.stl \(Part 20\)/ }));
      const gear = within(gate).getByRole("group", { name: /^gear\.stl changed/ });
      fireEvent.click(within(gear).getByRole("radio", { name: /^Keep the units already printed/ }));
      fireEvent.click(within(gate).getByRole("button", { name: "Save these choices" }));
      await waitFor(() => expect(sends).toBe(1));
      return { gate, fetchMock };
    }

    const checked = (group: HTMLElement) =>
      within(group)
        .getAllByRole<HTMLInputElement>("radio")
        .filter((radio) => radio.checked);
    const save = (gate: HTMLElement) =>
      within(gate).getByRole("button", { name: "Save these choices" });

    it("drops the pick when the conflict's kind changes, and Save waits for a new one", async () => {
      const { gate, fetchMock } = await answerThenChange([
        { kind: "unsafe_predecessor", target_draft_part_id: 11, predecessor_revision_part_id: 20 },
        UNSAFE_GEAR,
      ]);
      const bracket = await within(gate).findByRole("group", { name: /^bracket\.stl changed/ });
      expect(checked(bracket)).toHaveLength(0);
      expect(save(gate).getAttribute("aria-disabled")).toBe("true");
      expect(within(gate).getByText("Choose for 1 more Part first.")).toBeTruthy();
      fireEvent.click(save(gate));
      await act(() => new Promise((resolve) => setTimeout(resolve, 20)));
      expect(reconciles(fetchMock)).toHaveLength(1);
      fireEvent.click(
        within(bracket).getByRole("radio", { name: /^Keep the units already printed/ }),
      );
      await waitFor(() => expect(save(gate).getAttribute("aria-disabled")).toBeNull());
    });

    it("drops the pick when the same kind comes back with other candidates", async () => {
      const { gate } = await answerThenChange([ambiguousBracket([11, 30]), UNSAFE_GEAR]);
      const bracket = await within(gate).findByRole("group", { name: /^bracket\.stl matches/ });
      await within(bracket).findByRole("radio", { name: /bracket-30\.stl \(Part 30\)/ });
      expect(checked(bracket)).toHaveLength(0);
      expect(save(gate).getAttribute("aria-disabled")).toBe("true");
    });

    it("keeps the picks of conflicts that come back unchanged", async () => {
      const { gate, fetchMock } = await answerThenChange([ambiguousBracket([11, 20]), UNSAFE_GEAR]);
      await waitFor(() => expect(save(gate).getAttribute("aria-disabled")).toBeNull());
      const bracket = within(gate).getByRole("group", { name: /^bracket\.stl matches/ });
      expect(checked(bracket).map((radio) => radio.labels?.[0]?.textContent)).toEqual([
        "Keep the printed units of the accepted bracket-20.stl (Part 20).",
      ]);
      fireEvent.click(save(gate));
      await waitFor(() => expect(reconciles(fetchMock)).toHaveLength(2));
      expect(reconciles(fetchMock)[1]!.body.decisions).toEqual(
        reconciles(fetchMock)[0]!.body.decisions,
      );
    });
  });

  it("asks to match again when the preview still needs it after a match, without a loop", async () => {
    let sends = 0;
    const fetchMock = core({
      // The workspace says the draft is matched, so the tab thinks it is settled.
      draft: matched,
      preview: () => (sends >= 2 ? json(preview(1)) : needsMatch()),
      write: (path) => {
        if (path === "/api/v2/plans/2/drafts/9/apply") return json(RECEIPT);
        if (path !== RECONCILE) return undefined;
        sends += 1;
        return json(matched());
      },
    });
    renderAt(fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Review and accept" }));
    const gate = await screen.findByRole("dialog", { name: "Accept this draft" });
    await within(gate).findByText(
      "The core still asks for the printed units to be matched. Match them again.",
    );
    expect(sends).toBe(1);
    expect(within(gate).getByRole("button", { name: "Accept" }).getAttribute("aria-disabled")).toBe(
      "true",
    );
    fireEvent.click(within(gate).getByRole("button", { name: "Match printed units" }));
    await within(gate).findByText(/^Printed units matched/);
    const [first, second] = reconciles(fetchMock);
    expect(second!.key).not.toBe(first!.key);
    expect(second!.body.expected_snapshot_digest).toBe(MATCHED);
    await accept(gate);
    expect(sends).toBe(2);
  });

  it("previews again and shows the match step when Accept answers reconciliation_required", async () => {
    let previews = 0;
    let settled = false;
    const fetchMock = core({
      draft: () => (settled ? matched() : workspace()),
      preview: () => {
        previews += 1;
        return previews === 1 || settled ? json(preview(1)) : needsMatch();
      },
      write: (path) => {
        if (path === RECONCILE) {
          settled = true;
          return json(matched());
        }
        if (path !== "/api/v2/plans/2/drafts/9/apply") return undefined;
        return settled
          ? json(RECEIPT)
          : json(
              {
                type: "urn:print-partner:problem:domain",
                title: "Unprocessable",
                status: 422,
                code: "reconciliation_required",
                reason: "unresolved",
              },
              422,
            );
      },
    });
    renderAt(fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Review and accept" }));
    const gate = await screen.findByRole("dialog", { name: "Accept this draft" });
    await within(gate).findByText(/saves a snapshot/);
    fireEvent.click(within(gate).getByRole("button", { name: "Accept" }));
    await within(gate).findByText(/Accept needs the printed units matched/);
    await within(gate).findByText(/^Printed units matched/);
    await accept(gate);
    const applies = writes(fetchMock).filter((sent) => sent.path.endsWith("/apply"));
    expect(applies).toHaveLength(2);
    expect(applies[1]!.key).not.toBe(applies[0]!.key);
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
    const describedBy = (element: HTMLElement) =>
      (element.getAttribute("aria-describedby") ?? "")
        .split(" ")
        .map((id) => document.getElementById(id)?.textContent)
        .join(" ");
    const include = screen.getByRole("checkbox", { name: "Include gear.stl" });
    expect(describedBy(include)).toBe(OFFLINE_REASON);
    expect(describedBy(screen.getByRole("textbox", { name: /Quantity of gear\.stl/ }))).toBe(
      OFFLINE_REASON,
    );
    fireEvent.click(include);
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
