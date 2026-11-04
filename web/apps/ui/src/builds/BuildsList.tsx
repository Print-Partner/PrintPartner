import type { ProfileSummary } from "@print-partner/rust-client";
import { useQuery } from "@tanstack/react-query";
import type { KeyboardEvent } from "react";
import { Link } from "react-router";
import { useModule } from "../core/modules";
import { readCore } from "../core/rust";
import {
  WORKFLOW_PENDING_NOTE,
  WORKFLOW_PENDING_TEXT,
  workflowQuery,
  type BuildWorkflows,
} from "../core/workflow";
import { useConnection } from "../frame/connection";
import { queryScreenState } from "../screen/queryState";
import { ModuleOff, ScreenTemplate } from "../screen/ScreenTemplate";
import { progressText } from "./format";

export const buildsQuery = {
  queryKey: ["core", "builds"],
  queryFn: ({ signal }: { signal: AbortSignal }) => readCore("listBuilds", {}, signal),
} as const;

const ROW_KEYS = new Set(["ArrowDown", "ArrowUp", "Home", "End"]);

/**
 * Up, Down, Home and End move focus between the Build links, one per row. Rows are
 * keyed by Build ID, so a refetch keeps the focused link in place.
 */
function moveRowFocus(event: KeyboardEvent<HTMLTableSectionElement>) {
  const link = event.target as HTMLElement;
  const index = Number(link.dataset.row);
  if (!ROW_KEYS.has(event.key) || !Number.isInteger(index)) return;
  const rows = event.currentTarget.rows.length;
  const to = { ArrowDown: index + 1, ArrowUp: index - 1, Home: 0, End: rows - 1 }[event.key]!;
  const next = event.currentTarget.querySelector<HTMLElement>(`[data-row="${to}"]`);
  if (!next) return;
  event.preventDefault();
  next.focus();
}

function openTasksText(workflows: BuildWorkflows | undefined, id: number): string {
  if (workflows?.kind !== "ready") return WORKFLOW_PENDING_TEXT;
  const open = workflows.byBuild.get(id)?.openTasks;
  return open === undefined ? WORKFLOW_PENDING_TEXT : String(open);
}

function BuildsTable({
  builds,
  showOrder,
  workflows,
}: {
  builds: ProfileSummary[];
  showOrder: boolean;
  workflows: BuildWorkflows | undefined;
}) {
  return (
    <table className="pp-builds-table" aria-label="Builds">
      <thead>
        <tr>
          <th scope="col">Build</th>
          {showOrder && <th scope="col">Order</th>}
          <th scope="col">Progress</th>
          <th scope="col">Open tasks</th>
        </tr>
      </thead>
      <tbody onKeyDown={moveRowFocus}>
        {builds.map((build, row) => (
          <tr key={build.id}>
            <th scope="row">
              <Link to={`/builds/${build.id}`} className="pp-link" data-row={row}>
                {build.name}
              </Link>
              {build.archived_at && <span className="pp-tag">Archived</span>}
            </th>
            {showOrder && <td className="num">{build.order_number ?? "None"}</td>}
            <td>{progressText(build.accepted_progress)}</td>
            <td className="pp-muted">{openTasksText(workflows, build.id)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

/** Builds, in the order the core returns them. Read only. */
export function BuildsList() {
  const connection = useConnection();
  const builds = useQuery(buildsQuery);
  const workflows = useQuery(workflowQuery);
  const orders = useModule("orders");
  const state = queryScreenState(builds, connection, (data) => data.profiles.length === 0);
  return (
    <ScreenTemplate
      title="Builds"
      state={state}
      renderData={(data) => (
        <>
          {orders?.state === "off" && (
            <ModuleOff module="Orders" wouldShow="each Build's order number" where="this list" />
          )}
          {workflows.data?.kind !== "ready" && <p className="pp-muted">{WORKFLOW_PENDING_NOTE}</p>}
          <BuildsTable
            builds={data.profiles}
            showOrder={orders?.state === "on"}
            workflows={workflows.data}
          />
        </>
      )}
      empty={{ message: "No Builds yet. Builds you create appear here." }}
    />
  );
}
