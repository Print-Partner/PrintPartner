import type { BoardNote, ProfileSummary } from "@print-partner/rust-client";
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

it("opens Board from the rail, creates a local note, and links to its Build", async () => {
  let notes: BoardNote[] = [];
  const fetchMock = vi.fn(async (path: string, init?: RequestInit) => {
    const reply = (body: unknown) =>
      new Response(JSON.stringify(body), { headers: { "content-type": "application/json" } });
    if (path === "/api/v2/plans") return reply({ profiles: [build] });
    if (path === "/api/v2/board/notes") {
      if (init?.method === "POST") {
        notes = [note];
        return reply(note);
      }
      return reply({ notes });
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
