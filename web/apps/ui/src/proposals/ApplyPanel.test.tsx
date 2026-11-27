import type { Proposal } from "@print-partner/rust-client";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter } from "react-router";
import { afterEach, expect, it, vi } from "vitest";
import { App, createQueryClient } from "../App";
import { routes } from "../routes";

const proposal: Proposal = {
  id: "draft-review",
  plan_id: 2,
  label: "Apply reviewed Draft",
  summary: "Accept the reviewed Draft.",
  params: {},
  staged_by: "Evidence assistant",
  state: "pending",
  created_at: "2026-11-26T15:00:00Z",
  expires_at: "2026-11-26T23:00:00Z",
  depends_on: [],
};

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

it("shows the proposal source, expiry and dependencies, then reloads after applying", async () => {
  let actions: Proposal[] = [
    proposal,
    { ...proposal, id: "dependent", label: "Dependent proposal", depends_on: [proposal.id] },
    { ...proposal, id: "expired", label: "Expired proposal", state: "expired" },
  ];
  const fetchMock = vi.fn(async (path: string) => {
    const reply = (body: unknown) =>
      new Response(JSON.stringify(body), { headers: { "content-type": "application/json" } });
    if (path === "/api/v2/proposals") return reply({ actions, omitted: 0 });
    if (path === "/api/v2/proposals/draft-review/apply") {
      actions = actions.slice(1);
      return reply("applied");
    }
    return reply({});
  });
  vi.stubGlobal("fetch", fetchMock);
  render(
    <App
      router={createMemoryRouter(routes, { initialEntries: ["/apply"] })}
      queryClient={createQueryClient()}
    />,
  );
  const main = within(await screen.findByRole("main"));
  const expired = (await main.findByRole("heading", { name: "Expired proposal" })).closest("li")!;
  expect(within(expired).getByText("Expired")).toBeTruthy();
  expect(within(expired).getByRole<HTMLButtonElement>("button", { name: "Apply" }).disabled).toBe(
    true,
  );
  expect(main.getByText("Waiting for Apply reviewed Draft (Pending).")).toBeTruthy();
  const buttons = main.getAllByRole<HTMLButtonElement>("button", { name: "Apply" });
  expect(buttons[1].disabled).toBe(true);
  await waitFor(() => expect(buttons[0].disabled).toBe(false));
  expect(main.getAllByText("Evidence assistant")).toHaveLength(3);
  fireEvent.click(buttons[0]);
  expect(await main.findByText("Applied: Apply reviewed Draft")).toBeTruthy();
  expect(main.queryByRole("heading", { name: proposal.label })).toBeNull();
  expect(
    main.getAllByRole<HTMLButtonElement>("button", { name: "Apply" })[0].disabled,
  ).toBe(false);
});
