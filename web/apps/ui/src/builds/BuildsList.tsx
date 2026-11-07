import type { ProfileSummary } from "@print-partner/rust-client";
import { useQuery, type UseQueryResult } from "@tanstack/react-query";
import { useEffect, useState, type KeyboardEvent } from "react";
import { Link } from "react-router";
import { useModule } from "../core/modules";
import { readCore } from "../core/rust";
import { tasksByBuild, tasksQuery, type Task, type UnavailableBuild } from "../core/workflow";

type FeedEntries = ReadonlyMap<number, Task | UnavailableBuild>;
import { useConnection } from "../frame/connection";
import { queryScreenState } from "../screen/queryState";
import { ModuleOff, ScreenTemplate } from "../screen/ScreenTemplate";
import { dueDateText, progressText } from "./format";

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
function NextActionCell({ tasks, id }: { tasks: FeedEntries | undefined; id: number }) {
  if (!tasks) return <span className="pp-muted">Not loaded</span>;
  const task = tasks.get(id);
  if (!task) return <span className="pp-muted">Nothing open</span>;
  if (!("next_action" in task)) {
    return (
      <>
        <span className="pp-muted">Not available</span>
        <span className="pp-fact-detail">{task.reason}</span>
      </>
    );
  }
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
          {query.data === undefined
            ? "Could not load next actions."
            : "Could not refresh next actions."}
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

type DueFilter = "all" | "overdue" | "undated";

const DUE_FILTERS: { filter: DueFilter; label: string }[] = [
  { filter: "all", label: "All" },
  { filter: "overdue", label: "Overdue" },
  { filter: "undated", label: "No due date" },
];

/** Today in the user's time zone, as `YYYY-MM-DD` like a stored due date. */
function today(): string {
  const now = new Date();
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
}

/** Today's date, rechecked every minute so the list moves past midnight while it stays open. */
function useToday(): string {
  const [on, setOn] = useState(today);
  useEffect(() => {
    const timer = setInterval(() => setOn(today()), 60_000);
    return () => clearInterval(timer);
  }, []);
  return on;
}

/**
 * Past its due date with units still to print. A finished or archived Build is not overdue, as
 * the task feed ranks only unarchived Builds.
 */
function isOverdue(build: ProfileSummary, on: string): boolean {
  const done =
    build.accepted_progress.kind === "ready" && build.accepted_progress.remaining_units === 0;
  return build.archived_at === null && build.due_date !== null && build.due_date < on && !done;
}

/** Earliest due date first and Builds without one last; ties keep the core's order. */
function byDueDate(builds: ProfileSummary[]): ProfileSummary[] {
  return [...builds].sort((a, b) => {
    if (a.due_date === b.due_date) return 0;
    if (a.due_date === null) return 1;
    if (b.due_date === null) return -1;
    return a.due_date < b.due_date ? -1 : 1;
  });
}

/**
 * For each unarchived Build whose order number another unarchived Build also carries, those other
 * Builds. Blank order numbers don't count, matching the Accept preview's duplicate check.
 */
function sharedOrders(builds: ProfileSummary[]): Map<number, ProfileSummary[]> {
  const byOrder = new Map<string, ProfileSummary[]>();
  for (const build of builds) {
    if (build.archived_at !== null || !build.order_number?.trim()) continue;
    byOrder.set(build.order_number, [...(byOrder.get(build.order_number) ?? []), build]);
  }
  const shared = new Map<number, ProfileSummary[]>();
  for (const group of byOrder.values()) {
    if (group.length < 2) continue;
    for (const build of group)
      shared.set(
        build.id,
        group.filter((other) => other !== build),
      );
  }
  return shared;
}

function DueFilters({
  filter,
  setFilter,
  counts,
}: {
  filter: DueFilter;
  setFilter: (filter: DueFilter) => void;
  counts: Record<DueFilter, number>;
}) {
  return (
    <div role="radiogroup" aria-label="Show Builds" className="pp-segmented">
      {DUE_FILTERS.map(({ filter: value, label }) => (
        <button
          key={value}
          type="button"
          role="radio"
          aria-checked={filter === value}
          className="pp-button"
          onClick={() => setFilter(value)}
        >
          {label} ({counts[value]})
        </button>
      ))}
    </div>
  );
}

/** `shared` comes from every Build, so a filter never hides that an order is shared. */
function BuildsTable({
  builds,
  showOrder,
  tasks,
  overdueOn,
  shared = new Map(),
}: {
  builds: ProfileSummary[];
  showOrder: boolean;
  tasks: FeedEntries | undefined;
  overdueOn: string;
  shared?: Map<number, ProfileSummary[]>;
}) {
  return (
    <table className="pp-builds-table" aria-label="Builds">
      <thead>
        <tr>
          <th scope="col">Build</th>
          {showOrder && <th scope="col">Order</th>}
          {showOrder && <th scope="col">Due</th>}
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
            {showOrder && (
              <td className="num">
                {build.order_number ?? "None"}
                {shared.has(build.id) && (
                  <>
                    <span className="pp-tag">Duplicate order</span>
                    <span className="pp-fact-detail">
                      Also on{" "}
                      {shared
                        .get(build.id)!
                        .map((other) => other.name)
                        .join(", ")}
                    </span>
                  </>
                )}
              </td>
            )}
            {showOrder && (
              <td>
                {dueDateText(build.due_date) ?? "None"}
                {isOverdue(build, overdueOn) && <span className="pp-tag">Overdue</span>}
              </td>
            )}
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

/** The Builds sorted by due date, behind the overdue and no-due-date filters. */
function DueList({
  builds,
  filter,
  setFilter,
  tasks,
  on,
}: {
  builds: ProfileSummary[];
  filter: DueFilter;
  setFilter: (filter: DueFilter) => void;
  tasks: FeedEntries | undefined;
  on: string;
}) {
  const matches: Record<DueFilter, (build: ProfileSummary) => boolean> = {
    all: () => true,
    overdue: (build) => isOverdue(build, on),
    undated: (build) => build.due_date === null,
  };
  const counts = {
    all: builds.length,
    overdue: builds.filter(matches.overdue).length,
    undated: builds.filter(matches.undated).length,
  };
  const shown = byDueDate(builds.filter(matches[filter]));
  return (
    <>
      <DueFilters filter={filter} setFilter={setFilter} counts={counts} />
      {shown.length === 0 ? (
        <p className="pp-muted">No Builds match this filter.</p>
      ) : (
        <BuildsTable
          builds={shown}
          showOrder
          tasks={tasks}
          overdueOn={on}
          shared={sharedOrders(builds)}
        />
      )}
    </>
  );
}

/**
 * Builds, read only. With the Orders module on they sort by due date and filter to overdue or
 * undated Builds, and Builds sharing an order number are each shown, never merged. Otherwise
 * they keep the core's order.
 */
export function BuildsList() {
  const connection = useConnection();
  const builds = useQuery(buildsQuery);
  const tasks = useQuery({ ...tasksQuery, select: tasksByBuild });
  const orders = useModule("orders");
  const [filter, setFilter] = useState<DueFilter>("all");
  const state = queryScreenState(builds, connection, (data) => data.profiles.length === 0);
  const showOrder = orders?.state === "on";
  const on = useToday();
  return (
    <ScreenTemplate
      title="Builds"
      state={state}
      renderData={(data) => (
        <>
          {orders?.state === "off" && (
            <ModuleOff
              module="Orders"
              wouldShow="each Build's order number and due date, sorted by due date"
              where="this list"
            />
          )}
          <TasksNotice query={tasks} />
          {showOrder ? (
            <DueList
              builds={data.profiles}
              filter={filter}
              setFilter={setFilter}
              tasks={tasks.data}
              on={on}
            />
          ) : (
            <BuildsTable
              builds={data.profiles}
              showOrder={false}
              tasks={tasks.data}
              overdueOn={on}
            />
          )}
        </>
      )}
      empty={{ message: "No Builds yet. Builds you create appear here." }}
    />
  );
}
