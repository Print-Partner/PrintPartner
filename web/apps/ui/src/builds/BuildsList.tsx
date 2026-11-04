import type { ProfileSummary } from "@print-partner/rust-client";
import { useQuery, type UseQueryResult } from "@tanstack/react-query";
import type { KeyboardEvent } from "react";
import { Link } from "react-router";
import { useModule } from "../core/modules";
import { readCore } from "../core/rust";
import { tasksByBuild, tasksQuery, type Task } from "../core/workflow";
import { useConnection } from "../frame/connection";
import { queryScreenState } from "../screen/queryState";
import { ModuleOff, ScreenTemplate } from "../screen/ScreenTemplate";
import { progressText } from "./format";

const buildsQuery = {
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

/** The next-action cell: the projection's task for this Build, or why there is none. */
function NextActionCell({ tasks, id }: { tasks: ReadonlyMap<number, Task> | undefined; id: number }) {
  if (!tasks) return <span className="pp-muted">Not loaded</span>;
  const task = tasks.get(id);
  if (!task) return <span className="pp-muted">Nothing open</span>;
  return (
    <>
      {task.next_action.label}
      {task.attention && <span className="pp-tag">Needs you</span>}
    </>
  );
}

/** Says when next actions are still loading or could not load, with a Retry. */
function TasksNotice({ query }: { query: UseQueryResult<unknown> }) {
  if (query.isError) {
    return (
      <div role="alert" className="pp-banner" data-kind="error">
        <p>
          {query.data === undefined ? "Could not load next actions." : "Could not refresh next actions."}
        </p>
        <p className="pp-banner-detail">{query.error.message}</p>
        <button type="button" className="pp-button" onClick={() => void query.refetch()}>
          Retry
        </button>
      </div>
    );
  }
  if (query.data === undefined) return <p className="pp-muted">Loading next actions.</p>;
  return null;
}

function BuildsTable({
  builds,
  showOrder,
  tasks,
}: {
  builds: ProfileSummary[];
  showOrder: boolean;
  tasks: ReadonlyMap<number, Task> | undefined;
}) {
  return (
    <table className="pp-builds-table" aria-label="Builds">
      <thead>
        <tr>
          <th scope="col">Build</th>
          {showOrder && <th scope="col">Order</th>}
          <th scope="col">Progress</th>
          <th scope="col">Next action</th>
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
            <td>
              <NextActionCell tasks={tasks} id={build.id} />
            </td>
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
  const tasks = useQuery({ ...tasksQuery, select: tasksByBuild });
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
          <TasksNotice query={tasks} />
          <BuildsTable builds={data.profiles} showOrder={orders?.state === "on"} tasks={tasks.data} />
        </>
      )}
      empty={{ message: "No Builds yet. Builds you create appear here." }}
    />
  );
}
