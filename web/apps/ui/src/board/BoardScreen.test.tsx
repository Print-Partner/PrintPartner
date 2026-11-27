import type { BoardNote, ProfileSummary } from "@print-partner/rust-client";
import { onlineManager } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { routes } from "../routes";

const build: ProfileSummary = {
  id: 2,
  name: "Enclosure",
  accepted_progress: { kind: "empty" },
  freshness: { status: "unavailable", code: "not_accepted" },
  archived_at: null,
  build_stale: false,
  customer: null,
  due_date: null,
  last_used_at: null,
  order_number: null,
  part_count: 0,
  special_request: null,
};
const note: BoardNote = {
  id: "note-a",
  build_id: build.id,
  build_name: build.name,
  body: "Check the enclosure fit.",
  created_at: "2026-11-27T16:00:00.000Z",
};

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

async function openNoteForm() {
  let notes: BoardNote[] = [];
  const fetchMock = vi.fn(async (path: string, init?: RequestInit) => {
    if (path === "/health") return Response.json({});
    if (path === "/api/v2/board/builds") return Response.json({ builds: [build] });
    if (init?.method === "POST") {
      notes = [note];
      return Response.json(note);
    }
    return Response.json({ notes, imported_posts: [] });
  });
  vi.stubGlobal("fetch", fetchMock);
  const router = createMemoryRouter(routes, { initialEntries: ["/board"] });
  render(<App router={router} queryClient={createQueryClient()} />);
  const add = screen.getByRole<HTMLButtonElement>("button", { name: "Add note" });
  await waitFor(() => expect(add.disabled).toBe(false));
  fireEvent.click(add);
  await screen.findByRole("option", { name: build.name });
  fireEvent.change(screen.getByRole("combobox", { name: "Build" }), {
    target: { value: String(build.id) },
  });
  return fetchMock;
}

it("retains overlong input and refuses to save it until shortened", async () => {
  const fetchMock = await openNoteForm();
  const input = screen.getByRole<HTMLTextAreaElement>("textbox", { name: "Note" });
  expect(input.hasAttribute("maxlength")).toBe(false);
  const pasted = "é".repeat(1_000) + "TAIL";
  fireEvent.change(input, { target: { value: pasted } });
  expect(input.value).toBe(pasted);
  expect(input.getAttribute("aria-invalid")).toBe("true");
  expect(screen.getByRole("alert").textContent).toContain("exceeds 2,000 bytes");
  const save = screen.getByRole<HTMLButtonElement>("button", { name: "Save note" });
  expect(save.disabled).toBe(true);
  fireEvent.click(save);
  expect(fetchMock.mock.calls.some(([, init]) => init?.method === "POST")).toBe(false);
  fireEvent.change(input, { target: { value: note.body } });
  expect(screen.queryByRole("alert")).toBeNull();
  expect(save.disabled).toBe(false);
});

it("sends a Board note immediately when the browser reports offline but the core is reachable", async () => {
  const fetchMock = await openNoteForm();
  fireEvent.change(screen.getByRole("textbox", { name: "Note" }), { target: { value: note.body } });
  onlineManager.setOnline(false);
  try {
    fireEvent.click(screen.getByRole("button", { name: "Save note" }));
    await waitFor(() =>
      expect(
        fetchMock.mock.calls.filter(([path, init]) =>
          path === "/api/v2/board/notes" && init?.method === "POST",
        ),
      ).toHaveLength(1),
    );
    expect(await screen.findByText(note.body)).toBeTruthy();
  } finally {
    onlineManager.setOnline(true);
  }
});

it("lets an owner select the 65th Build when the full Build list is refused", async () => {
  const fetchMock = vi.fn(async (path: string, init?: RequestInit) => {
    if (path === "/api/v2/plans") return Response.json({}, { status: 409 });
    if (path === "/api/v2/board/builds") {
      return Response.json({
        builds: Array.from({ length: 65 }, (_, i) => ({ id: i + 1, name: `Build ${i + 1}` })),
      });
    }
    if (path === "/api/v2/board/notes" && init?.method === "POST") {
      return Response.json({ ...note, build_id: 65, build_name: "Build 65" });
    }
    return Response.json({ notes: [], imported_posts: [] });
  });
  vi.stubGlobal("fetch", fetchMock);
  const router = createMemoryRouter(routes, { initialEntries: ["/board"] });
  render(<App router={router} queryClient={createQueryClient()} />);
  const add = screen.getByRole<HTMLButtonElement>("button", { name: "Add note" });
  await waitFor(() => expect(add.disabled).toBe(false));
  fireEvent.click(add);
  await screen.findByRole("option", { name: "Build 65" });
  fireEvent.change(screen.getByRole("combobox", { name: "Build" }), { target: { value: "65" } });
  fireEvent.change(screen.getByRole("textbox", { name: "Note" }), { target: { value: note.body } });
  fireEvent.click(screen.getByRole("button", { name: "Save note" }));
  await waitFor(() => {
    const post = fetchMock.mock.calls.find(([, init]) => init?.method === "POST");
    expect(JSON.parse(String(post?.[1]?.body ?? "null"))).toEqual({ build_id: 65, body: note.body });
  });
});

it("opens Board from the rail, creates a local note, and links to its Build", async () => {
  let notes: BoardNote[] = [];
  const fetchMock = vi.fn(async (path: string, init?: RequestInit) => {
    const reply = (body: unknown) =>
      new Response(JSON.stringify(body), { headers: { "content-type": "application/json" } });
    if (path === "/api/v2/board/builds") return reply({ builds: [build] });
    if (path === "/api/v2/board/notes") {
      if (init?.method === "POST") {
        notes = [note];
        return reply(note);
      }
      return reply({ notes, imported_posts: [] });
    }
    return reply({});
  });
  vi.stubGlobal("fetch", fetchMock);
  const router = createMemoryRouter(routes, { initialEntries: ["/today"] });
  render(<App router={router} queryClient={createQueryClient()} />);
  const rail = screen.getByRole("navigation", { name: "Main" });
  fireEvent.click(within(rail).getByRole("link", { name: /^Board/ }));
  expect(router.state.location.pathname).toBe("/board");
  expect(await screen.findByText("No Build notes yet.")).toBeTruthy();
  const add = screen.getByRole<HTMLButtonElement>("button", { name: "Add note" });
  await waitFor(() => expect(add.disabled).toBe(false));
  fireEvent.click(add);
  await screen.findByRole("option", { name: build.name });
  fireEvent.change(screen.getByLabelText("Build"), { target: { value: String(build.id) } });
  const save = screen.getByRole<HTMLButtonElement>("button", { name: "Save note" });
  expect(save.disabled).toBe(true);
  fireEvent.change(screen.getByLabelText("Note"), { target: { value: note.body } });
  expect(save.disabled).toBe(false);
  fireEvent.click(save);
  expect(await screen.findByText(note.body)).toBeTruthy();
  expect(screen.getByRole("link", { name: build.name }).getAttribute("href")).toBe("/builds/2");
  expect(screen.queryByText("No Build notes yet.")).toBeNull();
  const posts = fetchMock.mock.calls.filter(
    ([path, init]) => path === "/api/v2/board/notes" && init?.method === "POST",
  );
  expect(posts).toHaveLength(1);
  expect(JSON.parse(String(posts[0][1]?.body))).toEqual({ build_id: 2, body: note.body });
});

it("shows imported posts without any Build links or editing controls", async () => {
  const post = {
    id: "legacy-a",
    title: "Original enclosure",
    caption: "Keep the spare clips.",
    created_at: "2026-11-23T12:00:00.000Z",
  };
  vi.stubGlobal("fetch", vi.fn(async (path: string) =>
    Response.json(path === "/api/v2/board/notes" ? { notes: [], imported_posts: [post] } : {}),
  ));
  const router = createMemoryRouter(routes, { initialEntries: ["/board"] });
  render(<App router={router} queryClient={createQueryClient()} />);

  const imported = within(await screen.findByRole("region", { name: "Imported posts" }));
  expect(imported.getByRole("heading", { name: post.title })).toBeTruthy();
  expect(imported.getByText(post.caption)).toBeTruthy();
  expect(imported.getByRole("time").getAttribute("datetime")).toBe(post.created_at);
  expect(imported.queryByRole("link")).toBeNull();
  expect(imported.queryByRole("button")).toBeNull();
  expect(screen.queryByText("No Build notes yet.")).toBeNull();
});
